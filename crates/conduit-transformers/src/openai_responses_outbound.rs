//! OpenAI Responses outbound transformer — converts unified [`LlmRequest`] to
//! Responses API wire format and transforms Responses API responses back to
//! unified [`LlmResponse`].
//!
//! Mirrors Go's `conduit/llm/transformer/openai/responses/outbound.go`,
//! `compact_outbound.go`, and `outbound_convert.go`. This module provides:
//!
//! * [`build_responses_request_body`] — Builds the full Responses API request
//!   JSON from a unified [`LlmRequest`].
//! * [`build_responses_compact_request_body`] — Builds the compact API variant.
//! * [`transform_responses_response`] — Converts a Responses API HTTP response
//!   to a unified [`LlmResponse`].
//! * [`transform_responses_compact_response`] — Converts a compact API response.
//!
//! The Responses API has two distinct request shapes (standard vs compact) and
//! correspondingly two response shapes. Both are handled here with full Go parity.

#![forbid(unsafe_code)]

use conduit_core::ConduitError;
use conduit_llm::{
    ApiFormat, Choice, MessageContent, RequestType, Usage,
    model::{
        Annotation, ChatMessage, ChatRequest, ContentPart, ErrorDetail, HeaderMap, HttpAuth,
        HttpRequest, HttpResponse, LlmMessage, LlmRequest, LlmRequestPayload, LlmResponse,
        ResponseError, StreamEvent, ToolCall, UrlCitation,
    },
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, VecDeque};

use crate::{OutboundTransformer, TransformerResult};

/// Default Responses API endpoint path (standard variant).
pub const DEFAULT_RESPONSES_PATH: &str = "/v1/responses";

/// Default Responses API endpoint path (compact variant).
pub const DEFAULT_RESPONSES_COMPACT_PATH: &str = "/v1/responses/compact";

/// Build the full standard Responses API request body from a unified [`LlmRequest`].
///
/// Mirrors Go `OutboundTransformer.TransformRequest` (responses/outbound.go:191-318)
/// for the standard (non-compact) request shape. The function:
///
/// * Validates that `request_type` is `Chat`/`Image` or empty (Rejects
///   `Compact` with a parity-style error; compact requests must use
///   [`build_responses_compact_request_body]`).
/// * Converts tools (`function`, `web_search`, `image_generation`,
///   `responses_custom_tool`) to the Responses API `Tool` format.
/// * Builds the `Request` payload with `input`, `instructions`, `tools`,
///   `reasoning`, `response_format`, and all other fields.
/// * Returns a JSON [`Value`] ready for HTTP transport.
///
/// # Go parity
///
/// Field mapping follows Go's `Request` construction (outbound.go:247-272):
///
/// | LlmRequest field | Responses API field | Notes |
/// |------------------|---------------------|-------|
/// | `model` | `model` | Required |
/// | `payload::Responses.input` | `input` | String or items array |
/// | `payload::Responses.instructions` | `instructions` | System message |
/// | `payload::Responses.tools` | `tools` | Converted per tool type |
/// | `payload::Responses.reasoning` | `reasoning` | `{effort, summary, max_tokens}` |
/// | `payload::Responses.previous_response_id` | `previous_response_id` | Multi-turn |
/// | `stream` | `stream` | From top-level `LlmRequest` |
/// | `payload::Responses.response_format` | `text.format` | Structured outputs |
///
/// Provider extensions on `ResponsesRequest.extra` are flattened onto the
/// output, matching Go's open-ended `Request` struct.
pub fn build_responses_request_body(llm_request: &LlmRequest) -> TransformerResult<Value> {
    // Go dispatch: Compact is handled by a separate builder.
    if matches!(llm_request.request_type, RequestType::Compact) {
        return Err(ConduitError::invalid_request(
            "compact requests must use the compact builder (build_responses_compact_request_body)",
        ));
    }

    let model = llm_request
        .model
        .as_deref()
        .filter(|model| !model.is_empty())
        .ok_or_else(|| ConduitError::invalid_request("model is required for Responses API"))?;

    let mut body = match &llm_request.payload {
        LlmRequestPayload::Responses(payload) => {
            build_native_responses_request_body(model, llm_request.stream, payload)?
        }
        LlmRequestPayload::Chat(payload) => {
            build_chat_responses_request_body(model, llm_request.stream, payload)?
        }
        other => {
            return Err(ConduitError::invalid_request(format!(
                "Responses outbound expects a Chat or Responses payload (got {})",
                other.request_type()
            )));
        }
    };

    // `extra_body` is the explicit provider override bag. Typed fields remain
    // authoritative, matching the Go request marshaler's first-write-wins
    // behavior.
    for (key, value) in &llm_request.extra_body {
        body.entry(key.clone()).or_insert_with(|| value.clone());
    }

    Ok(Value::Object(body))
}

fn build_native_responses_request_body(
    model: &str,
    stream: bool,
    payload: &conduit_llm::model::ResponsesRequest,
) -> TransformerResult<Map<String, Value>> {
    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.to_string()));

    // Input is required for Responses API.
    let input = payload
        .input
        .as_ref()
        .ok_or_else(|| ConduitError::invalid_request("input is required for Responses API"))?;
    body.insert("input".to_string(), input.clone());

    // Instructions (system message).
    if let Some(ref instructions) = payload.instructions {
        if !instructions.is_empty() {
            body.insert(
                "instructions".to_string(),
                Value::String(instructions.clone()),
            );
        }
    }

    // Previous response ID for multi-turn conversations.
    if let Some(ref prev_id) = payload.previous_response_id {
        body.insert(
            "previous_response_id".to_string(),
            Value::String(prev_id.clone()),
        );
    }

    // Convert tools to Responses API format.
    let tools = convert_tools_for_responses(&payload.tools, None)?;
    if !tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(tools));
    }

    // Stream flag.
    body.insert("stream".to_string(), Value::Bool(stream));

    // Reasoning configuration.
    if let Some(ref reasoning) = payload.reasoning {
        body.insert("reasoning".to_string(), reasoning.clone());
    }

    // Response format (structured outputs).
    if let Some(ref response_format) = payload.response_format {
        let text = build_text_options(response_format)?;
        body.insert("text".to_string(), text);
    }

    // Flatten all extra fields from the payload (Go's Request struct has many
    // optional fields that we preserve losslessly via the extra bag).
    for (key, value) in &payload.extra {
        // Don't double-write keys already populated above (first-write-wins).
        if !body.contains_key(key) {
            body.insert(key.clone(), value.clone());
        }
    }

    Ok(body)
}

/// Convert the shared chat payload used by OpenAI Chat, Anthropic Messages and
/// Gemini Contents inbound transformers into a native Responses request.
///
/// This is the Rust counterpart of Go's `convertInputFromMessages` and
/// `convertInstructionsFromMessages`. Keeping the conversion here is what lets
/// the selected *upstream* format drive the wire protocol independently of the
/// client-facing format.
fn build_chat_responses_request_body(
    model: &str,
    stream: bool,
    chat: &ChatRequest,
) -> TransformerResult<Map<String, Value>> {
    if chat.messages.is_empty() {
        return Err(ConduitError::invalid_request(
            "messages are required for Responses API",
        ));
    }

    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.to_string()));
    body.insert(
        "input".to_string(),
        convert_chat_messages_to_responses_input(chat)?,
    );

    let instructions = convert_chat_instructions(chat);
    if !instructions.is_empty() {
        body.insert("instructions".to_string(), Value::String(instructions));
    }

    let tools = convert_tools_for_responses(&chat.tools, None)?;
    if !tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(tools));
    }
    body.insert("stream".to_string(), Value::Bool(stream));

    if let Some(response_format) = &chat.response_format {
        body.insert("text".to_string(), build_text_options(response_format)?);
    }
    if let Some(max_tokens) = chat
        .extra
        .get("max_completion_tokens")
        .cloned()
        .or_else(|| chat.max_tokens.map(Value::from))
    {
        body.insert("max_output_tokens".to_string(), max_tokens);
    }
    if let Some(top_p) = chat.top_p.and_then(serde_json::Number::from_f64) {
        body.insert("top_p".to_string(), Value::Number(top_p));
    }
    if let Some(tool_choice) = chat.tool_choice.as_ref().and_then(convert_tool_choice) {
        body.insert("tool_choice".to_string(), tool_choice);
    }

    if let Some(effort) = &chat.reasoning_effort {
        body.insert(
            "reasoning".to_string(),
            serde_json::json!({"effort": effort}),
        );
    }

    if let Some(stream_options) = chat.stream_options.as_ref()
        && let Some(include_obfuscation) = stream_options.get("include_obfuscation")
    {
        body.insert(
            "stream_options".to_string(),
            serde_json::json!({"include_obfuscation": include_obfuscation}),
        );
    }

    // These are the provider-neutral request fields copied by the canonical
    // Go Responses transformer. Provider-specific inbound leftovers (for
    // example `anthropic_extra`) are intentionally not leaked upstream.
    for key in [
        "parallel_tool_calls",
        "store",
        "service_tier",
        "safety_identifier",
        "user",
        "metadata",
        "top_logprobs",
        "prompt_cache_key",
        "previous_response_id",
        "include",
        "max_tool_calls",
        "prompt_cache_retention",
        "truncation",
    ] {
        if let Some(value) = chat.extra.get(key) {
            body.entry(key.to_string()).or_insert_with(|| value.clone());
        }
    }

    // Responses rejects `parallel_tool_calls` when no tools are supplied.
    if !body.contains_key("tools") {
        body.remove("parallel_tool_calls");
    }

    Ok(body)
}

fn convert_chat_instructions(chat: &ChatRequest) -> String {
    let mut instructions = Vec::new();

    // Anthropic's inbound transformer preserves its top-level system prompt in
    // this slot instead of synthesizing a system-role message.
    if let Some(system) = chat.extra.get("system") {
        collect_instruction_value(system, &mut instructions);
    }

    for message in &chat.messages {
        if message.role == "system" {
            collect_message_text(message.content.as_ref(), &mut instructions);
        }
    }

    instructions.join("\n")
}

fn collect_instruction_value(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(text) if !text.is_empty() => output.push(text.clone()),
        Value::Array(parts) => {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    output.push(text.to_string());
                }
            }
        }
        _ => {}
    }
}

fn collect_message_text(content: Option<&MessageContent>, output: &mut Vec<String>) {
    match content {
        Some(MessageContent::Text(text)) if !text.is_empty() => output.push(text.clone()),
        Some(MessageContent::Parts(parts)) => {
            let joined = parts
                .iter()
                .filter(|part| part.part_type == "text")
                .filter_map(|part| part.text.as_deref())
                .collect::<Vec<_>>()
                .join("\n");
            if !joined.is_empty() {
                output.push(joined);
            }
        }
        Some(MessageContent::Json(Value::String(text))) if !text.is_empty() => {
            output.push(text.clone());
        }
        _ => {}
    }
}

fn convert_chat_messages_to_responses_input(chat: &ChatRequest) -> TransformerResult<Value> {
    if chat.messages.len() == 1 {
        let message = &chat.messages[0];
        if !matches!(message.role.as_str(), "system")
            && let Some(MessageContent::Text(text)) = &message.content
        {
            return Ok(Value::String(text.clone()));
        }
    }

    let mut items = Vec::new();
    let mut tool_result_types = BTreeMap::<String, &'static str>::new();

    for message in &chat.messages {
        match message.role.as_str() {
            "system" => {}
            "user" | "developer" => {
                items.push(convert_user_message_to_responses(message));
            }
            "assistant" | "model" => {
                let assistant_items = convert_assistant_message_to_responses(message)?;
                for item in &assistant_items {
                    let item_type = item.get("type").and_then(Value::as_str);
                    let call_id = item.get("call_id").and_then(Value::as_str);
                    if let (Some(item_type), Some(call_id)) = (item_type, call_id) {
                        let result_type = match item_type {
                            "custom_tool_call" => "custom_tool_call_output",
                            "function_call" => "function_call_output",
                            _ => continue,
                        };
                        tool_result_types.insert(call_id.to_string(), result_type);
                    }
                }
                items.extend(assistant_items);
            }
            "tool" => {
                let call_id = message.tool_call_id.clone().unwrap_or_default();
                let item_type = tool_result_types
                    .get(&call_id)
                    .copied()
                    .unwrap_or("function_call_output");
                items.push(serde_json::json!({
                    "type": item_type,
                    "call_id": call_id,
                    "output": tool_output_from_content(message.content.as_ref()),
                }));
            }
            _ => {}
        }
    }

    Ok(Value::Array(items))
}

fn convert_user_message_to_responses(message: &ChatMessage) -> Value {
    let role = if message.role == "developer" {
        "developer"
    } else {
        "user"
    };
    serde_json::json!({
        "type": "message",
        "role": role,
        "content": input_content_parts(message.content.as_ref()),
    })
}

fn input_content_parts(content: Option<&MessageContent>) -> Vec<Value> {
    match content {
        Some(MessageContent::Text(text)) => {
            vec![serde_json::json!({"type": "input_text", "text": text})]
        }
        Some(MessageContent::Parts(parts)) => parts
            .iter()
            .filter_map(|part| match part.part_type.as_str() {
                "text" => part
                    .text
                    .as_ref()
                    .map(|text| serde_json::json!({"type": "input_text", "text": text})),
                "image_url" => part.image_url.as_ref().map(|image| {
                    let url = image.get("url").cloned().unwrap_or_else(|| image.clone());
                    let mut item = Map::new();
                    item.insert("type".to_string(), Value::String("input_image".to_string()));
                    item.insert("image_url".to_string(), url);
                    if let Some(detail) = image.get("detail") {
                        item.insert("detail".to_string(), detail.clone());
                    }
                    Value::Object(item)
                }),
                "input_audio" => part
                    .input_audio
                    .as_ref()
                    .map(|audio| serde_json::json!({"type": "input_audio", "input_audio": audio})),
                _ => None,
            })
            .collect(),
        Some(MessageContent::Json(Value::String(text))) => {
            vec![serde_json::json!({"type": "input_text", "text": text})]
        }
        Some(MessageContent::Json(value)) => {
            vec![serde_json::json!({"type": "input_text", "text": value.to_string()})]
        }
        None => Vec::new(),
    }
}

fn convert_assistant_message_to_responses(message: &ChatMessage) -> TransformerResult<Vec<Value>> {
    let mut items = Vec::new();

    let encrypted_content = message
        .extra
        .get("reasoning_signature")
        .or_else(|| message.extra.get("encrypted_content"))
        .and_then(Value::as_str);
    if let Some(encrypted_content) = encrypted_content.filter(|value| !value.is_empty()) {
        let summary = message
            .extra
            .get("reasoning_content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(|text| vec![serde_json::json!({"type": "summary_text", "text": text})])
            .unwrap_or_default();
        items.push(serde_json::json!({
            "type": "reasoning",
            "encrypted_content": encrypted_content,
            "summary": summary,
        }));
    }

    let output_parts: Vec<Value> = match message.content.as_ref() {
        Some(MessageContent::Text(text)) => {
            vec![serde_json::json!({"type": "output_text", "text": text})]
        }
        Some(MessageContent::Parts(parts)) => parts
            .iter()
            .filter(|part| part.part_type == "text")
            .filter_map(|part| part.text.as_ref())
            .map(|text| serde_json::json!({"type": "output_text", "text": text}))
            .collect(),
        Some(MessageContent::Json(Value::String(text))) => {
            vec![serde_json::json!({"type": "output_text", "text": text})]
        }
        Some(MessageContent::Json(value)) => {
            vec![serde_json::json!({"type": "output_text", "text": value.to_string()})]
        }
        None => Vec::new(),
    };
    if !output_parts.is_empty() {
        items.push(serde_json::json!({
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": output_parts,
        }));
    }

    for tool_call in &message.tool_calls {
        if tool_call.call_type == "responses_custom_tool" {
            let custom = tool_call.extra.get("response_custom_tool");
            items.push(serde_json::json!({
                "type": "custom_tool_call",
                "call_id": custom.and_then(|v| v.get("call_id")).and_then(Value::as_str)
                    .or(tool_call.id.as_deref()).unwrap_or(""),
                "name": custom.and_then(|v| v.get("name")).and_then(Value::as_str).unwrap_or(""),
                "input": custom.and_then(|v| v.get("input")).and_then(Value::as_str).unwrap_or(""),
            }));
            continue;
        }

        let arguments = tool_call
            .function
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new()));
        let arguments = match arguments {
            Value::String(value) => value,
            value => serde_json::to_string(&value).map_err(|error| {
                ConduitError::internal("failed to serialize tool call arguments").with_source(error)
            })?,
        };
        let mut item = Map::new();
        item.insert(
            "type".to_string(),
            Value::String("function_call".to_string()),
        );
        item.insert(
            "call_id".to_string(),
            Value::String(tool_call.id.clone().unwrap_or_default()),
        );
        item.insert(
            "name".to_string(),
            Value::String(
                tool_call
                    .function
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
        );
        if let Some(namespace) = tool_call.function.get("namespace") {
            item.insert("namespace".to_string(), namespace.clone());
        }
        item.insert("arguments".to_string(), Value::String(arguments));
        items.push(Value::Object(item));
    }

    Ok(items)
}

fn tool_output_from_content(content: Option<&MessageContent>) -> Value {
    match content {
        Some(MessageContent::Text(text)) => Value::String(text.clone()),
        Some(MessageContent::Parts(parts)) => Value::Array(
            parts
                .iter()
                .filter(|part| part.part_type == "text")
                .filter_map(|part| part.text.as_ref())
                .map(|text| serde_json::json!({"type": "input_text", "text": text}))
                .collect(),
        ),
        Some(MessageContent::Json(value)) => value.clone(),
        None => Value::String(String::new()),
    }
}

fn convert_tool_choice(choice: &Value) -> Option<Value> {
    if choice.is_string() {
        return Some(choice.clone());
    }
    let object = choice.as_object()?;
    let choice_type = object.get("type")?.as_str()?;
    let name = object.get("name").and_then(Value::as_str).or_else(|| {
        object
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
    });
    match name {
        Some(name) => Some(serde_json::json!({"type": choice_type, "name": name})),
        None => Some(Value::String(choice_type.to_string())),
    }
}

/// Build the compact Responses API request body from a unified [`LlmRequest`].
///
/// Mirrors Go `transformCompactRequest` (compact_outbound.go:17-63). The compact
/// API accepts a minimal subset of fields:
///
/// * `model` (required)
/// * `input` (required, must be an items array)
/// * `instructions` (optional)
/// * `prompt_cache_key` (optional)
///
/// Tools, stream, reasoning, response_format, and all other fields are **not**
/// supported by the compact endpoint and are silently dropped.
pub fn build_responses_compact_request_body(llm_request: &LlmRequest) -> TransformerResult<Value> {
    let LlmRequestPayload::Responses(payload) = &llm_request.payload else {
        return Err(ConduitError::invalid_request(
            "Compact Responses outbound requires a Responses payload",
        ));
    };

    let model = llm_request.model.as_deref().ok_or_else(|| {
        ConduitError::invalid_request("model is required for Compact Responses API")
    })?;

    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.to_string()));

    // Input is required for compact API.
    let input = payload.input.as_ref().ok_or_else(|| {
        ConduitError::invalid_request("input is required for Compact Responses API")
    })?;
    body.insert("input".to_string(), input.clone());

    // Instructions (optional).
    if let Some(ref instructions) = payload.instructions {
        if !instructions.is_empty() {
            body.insert(
                "instructions".to_string(),
                Value::String(instructions.clone()),
            );
        }
    }

    // Prompt cache key (optional, from extra).
    if let Some(cache_key) = payload.extra.get("prompt_cache_key") {
        if !cache_key.is_null() {
            body.insert("prompt_cache_key".to_string(), cache_key.clone());
        }
    }

    Ok(Value::Object(body))
}

/// Convert unified tools to Responses API `Tool` format.
///
/// Mirrors Go tool conversion in `OutboundTransformer.TransformRequest`
/// (responses/outbound.go:220-245). Supported tool types:
///
/// | Tool type | Responses API type | Conversion |
/// |-----------|-------------------|------------|
/// | `function` | `function` | Name, description, parameters, strict |
/// | `web_search` / `web_search_preview` | `web_search` | Filters, user_location |
/// | `image_generation` | `image_generation` | Model, size, quality, ... |
/// | `responses_custom_tool` | `custom` | Name, description, format |
fn convert_tools_for_responses(
    tools: &[conduit_llm::model::UnifiedTool],
    image_request: Option<&Value>,
) -> TransformerResult<Vec<Value>> {
    let mut result = Vec::with_capacity(tools.len());

    for tool in tools {
        let tool_obj = match tool.tool_type.as_str() {
            "function" => convert_function_tool(tool)?,
            "web_search" | "web_search_preview" => convert_web_search_tool(tool)?,
            "image_generation" => convert_image_generation_tool(tool)?,
            "responses_custom_tool" => convert_custom_tool(tool)?,
            // Unknown tool types are dropped (Go parity: continue loop).
            _ => continue,
        };
        result.push(tool_obj);
    }

    // If an image request is present, inject the image_generation tool.
    if let Some(img_req) = image_request {
        result.push(build_image_generation_tool_from_request(img_req)?);
    }

    Ok(result)
}

/// Convert a function tool to Responses API format.
///
/// Mirrors Go `convertFunctionToTool` (responses/outbound_convert.go:399-464).
fn convert_function_tool(tool: &conduit_llm::model::UnifiedTool) -> TransformerResult<Value> {
    let mut obj = Map::new();
    obj.insert("type".to_string(), Value::String("function".to_string()));

    if let Some(ref name) = tool.name {
        obj.insert("name".to_string(), Value::String(name.clone()));
    }

    if let Some(ref description) = tool.description {
        obj.insert(
            "description".to_string(),
            Value::String(description.clone()),
        );
    }

    // Parameters can be a JSON schema (map or null).
    if let Some(ref params) = tool.parameters {
        obj.insert("parameters".to_string(), params.clone());
    }

    // Strict flag from extra.
    if let Some(strict) = tool.extra.get("strict").and_then(|v| v.as_bool()) {
        obj.insert("strict".to_string(), Value::Bool(strict));
    }

    // Flatten remaining extra fields (Go preserves unknown fields).
    for (key, value) in &tool.extra {
        if !obj.contains_key(key) && key != "strict" {
            obj.insert(key.clone(), value.clone());
        }
    }

    Ok(Value::Object(obj))
}

/// Convert a web search tool to Responses API format.
///
/// Mirrors Go `convertWebSearchToTool` (responses/outbound_convert.go:345-376).
fn convert_web_search_tool(tool: &conduit_llm::model::UnifiedTool) -> TransformerResult<Value> {
    let mut obj = Map::new();
    obj.insert("type".to_string(), Value::String("web_search".to_string()));

    // Extract web_search config from extra (where unified model stores it).
    if let Some(web_search) = tool.extra.get("web_search") {
        // Allowed domains filter.
        if let Some(domains) = web_search.get("allowed_domains").and_then(|v| v.as_array()) {
            let domains_arr: Vec<Value> = domains
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| Value::String(s.to_string()))
                .collect();
            if !domains_arr.is_empty() {
                let mut filters = Map::new();
                filters.insert("allowed_domains".to_string(), Value::Array(domains_arr));
                obj.insert("filters".to_string(), Value::Object(filters));
            }
        }

        // User location.
        if let Some(location) = web_search.get("user_location") {
            if let Some(loc_obj) = location.as_object() {
                let mut user_location = Map::new();

                let loc_type = loc_obj
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("approximate");
                user_location.insert("type".to_string(), Value::String(loc_type.to_string()));

                if let Some(city) = loc_obj.get("city").and_then(|v| v.as_str()) {
                    user_location.insert("city".to_string(), Value::String(city.to_string()));
                }
                if let Some(country) = loc_obj.get("country").and_then(|v| v.as_str()) {
                    user_location.insert("country".to_string(), Value::String(country.to_string()));
                }
                if let Some(region) = loc_obj.get("region").and_then(|v| v.as_str()) {
                    user_location.insert("region".to_string(), Value::String(region.to_string()));
                }
                if let Some(timezone) = loc_obj.get("timezone").and_then(|v| v.as_str()) {
                    user_location
                        .insert("timezone".to_string(), Value::String(timezone.to_string()));
                }

                obj.insert("user_location".to_string(), Value::Object(user_location));
            }
        }
    }

    Ok(Value::Object(obj))
}

/// Convert an image generation tool to Responses API format.
///
/// Mirrors Go `convertImageGenerationToTool` (responses/outbound_convert.go:325-343).
fn convert_image_generation_tool(
    tool: &conduit_llm::model::UnifiedTool,
) -> TransformerResult<Value> {
    let mut obj = Map::new();
    obj.insert(
        "type".to_string(),
        Value::String("image_generation".to_string()),
    );

    // Extract image_generation config from extra.
    if let Some(img_gen) = tool.extra.get("image_generation") {
        if let Some(img_obj) = img_gen.as_object() {
            // Copy all image_generation fields directly.
            for (key, value) in img_obj {
                obj.insert(key.clone(), value.clone());
            }
        }
    }

    Ok(Value::Object(obj))
}

/// Build an image_generation tool from an image request.
///
/// Mirrors Go `buildImageToolRequest` (responses/image_request.go:19-56) and
/// `convertImageGenerationToTool` (responses/outbound_convert.go:325-343).
fn build_image_generation_tool_from_request(image_request: &Value) -> TransformerResult<Value> {
    let mut obj = Map::new();
    obj.insert(
        "type".to_string(),
        Value::String("image_generation".to_string()),
    );

    if let Some(req_obj) = image_request.as_object() {
        // Map image request fields to tool fields.
        if req_obj.get("prompt").and_then(|v| v.as_str()).is_some() {
            obj.insert("action".to_string(), Value::String("generate".to_string()));
        }

        // Model configuration.
        if let Some(model) = req_obj.get("model").and_then(|v| v.as_str()) {
            obj.insert("model".to_string(), Value::String(model.to_string()));
        }

        // Size, quality, etc.
        if let Some(size) = req_obj.get("size").and_then(|v| v.as_str()) {
            obj.insert("size".to_string(), Value::String(size.to_string()));
        }
        if let Some(quality) = req_obj.get("quality").and_then(|v| v.as_str()) {
            obj.insert("quality".to_string(), Value::String(quality.to_string()));
        }
        if let Some(style) = req_obj.get("style").and_then(|v| v.as_str()) {
            obj.insert("style".to_string(), Value::String(style.to_string()));
        }
        if let Some(n) = req_obj.get("n").and_then(|v| v.as_u64()) {
            obj.insert("n".to_string(), Value::Number(n.into()));
        }
    }

    Ok(Value::Object(obj))
}

/// Convert a custom tool to Responses API format.
///
/// Mirrors Go `convertCustomToTool` (responses/outbound_convert.go:378-397).
fn convert_custom_tool(tool: &conduit_llm::model::UnifiedTool) -> TransformerResult<Value> {
    let mut obj = Map::new();
    obj.insert("type".to_string(), Value::String("custom".to_string()));

    if let Some(ref name) = tool.name {
        obj.insert("name".to_string(), Value::String(name.clone()));
    }

    if let Some(ref description) = tool.description {
        obj.insert(
            "description".to_string(),
            Value::String(description.clone()),
        );
    }

    // Custom tool format definition (from extra).
    if let Some(format) = tool.extra.get("format") {
        obj.insert("format".to_string(), format.clone());
    }

    Ok(Value::Object(obj))
}

/// Build the `text` section of the request for structured outputs.
///
/// Mirrors Go `convertToTextOptions` (responses/outbound_convert.go:15-47).
fn build_text_options(response_format: &Value) -> TransformerResult<Value> {
    let mut text = Map::new();

    if let Some(fmt_obj) = response_format.as_object() {
        let format_type = fmt_obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("text");

        let mut format_obj = Map::new();
        format_obj.insert("type".to_string(), Value::String(format_type.to_string()));

        // For json_schema type, extract schema fields.
        if format_type == "json_schema" {
            if let Some(name) = fmt_obj.get("name").and_then(|v| v.as_str()) {
                format_obj.insert("name".to_string(), Value::String(name.to_string()));
            }
            if let Some(description) = fmt_obj.get("description").and_then(|v| v.as_str()) {
                format_obj.insert(
                    "description".to_string(),
                    Value::String(description.to_string()),
                );
            }
            if let Some(schema) = fmt_obj.get("schema") {
                format_obj.insert("schema".to_string(), schema.clone());
            }
            if let Some(strict) = fmt_obj.get("strict").and_then(|v| v.as_bool()) {
                format_obj.insert("strict".to_string(), Value::Bool(strict));
            }
        }

        text.insert("format".to_string(), Value::Object(format_obj));
    }

    Ok(Value::Object(text))
}

/// Transform a Responses API HTTP response into a unified [`LlmResponse`].
///
/// Mirrors Go `transformStandardResponse` (responses/outbound.go:352-438).
///
/// The function:
/// * Parses the Responses API JSON body.
/// * Converts `output[]` items into an assistant [`LlmMessage`] with content,
///   tool calls, reasoning, and annotations.
/// * Extracts `usage` if present.
/// * Builds a standard chat.completion-shaped [`LlmResponse`].
///
/// # Go parity
///
/// The output conversion mirrors Go `convertOutputToMessage`
/// (responses/outbound_convert.go:604-766), handling:
///
/// * `output_text` → message content
/// * `refusal` → typed refusal content part
/// * `function_call` → tool_calls
/// * `custom_tool_call` → tool_calls with `responses_custom_tool` type
/// * `reasoning` → reasoning_content/reasoning_signature
/// * `image_generation_call` → image_url content part
/// * `annotations` → message annotations
///
/// Unknown item types are silently skipped (Go parity: default continue).
pub fn transform_responses_response(status: u16, body: &[u8]) -> TransformerResult<LlmResponse> {
    if status >= 400 {
        // Parse error body for structured error details.
        if let Ok(error_body) = serde_json::from_slice::<Value>(body) {
            if let Some(error_obj) = error_body.get("error") {
                let message = error_obj
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Responses API error");
                return Err(ConduitError::upstream(format!("[{status}] {message}")));
            }
        }
        return Err(ConduitError::upstream(format!(
            "[{status}] Responses API request failed"
        )));
    }

    let raw: Value = serde_json::from_slice(body).map_err(|err| {
        ConduitError::internal("failed to parse Responses API response").with_source(err)
    })?;

    let obj = raw
        .as_object()
        .ok_or_else(|| ConduitError::internal("Responses API response is not an object"))?;

    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let created = obj.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
    let model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut response = LlmResponse {
        id,
        object: "chat.completion".to_string(),
        created,
        model,
        request_type: Some(RequestType::Chat),
        api_format: Some(ApiFormat::OpenAiResponses),
        ..Default::default()
    };

    if response.id.is_empty()
        && response.model.is_empty()
        && obj
            .get("output")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty)
    {
        return Err(ConduitError::internal(format!(
            "Responses API returned an empty response: body={}",
            String::from_utf8_lossy(body)
        )));
    }

    // Extract usage.
    if let Some(usage_obj) = obj.get("usage") {
        if let Ok(usage) = extract_usage_from_responses(usage_obj) {
            response.usage = Some(usage);
        }
    }

    // Convert output to message.
    let output_items = obj.get("output").and_then(|v| v.as_array());
    let (message, finish_reason) = convert_output_to_message(output_items)?;

    let mut choice = Choice {
        index: 0,
        ..Default::default()
    };

    choice.message = message;

    // Map status to finish_reason (Go: outbound.go:406-417).
    let status_finish_reason = obj
        .get("status")
        .and_then(|v| v.as_str())
        .and_then(|status_str| match status_str {
            "completed" => Some("stop"),
            "failed" => Some("error"),
            "incomplete" => Some("length"),
            "cancelled" | "canceled" => Some("cancelled"),
            _ => None,
        });

    // Go gives tool calls precedence over the top-level response status;
    // status is only consulted for responses without tool calls
    // (outbound.go:404-417).
    if let Some(reason) = finish_reason {
        choice.finish_reason = Some(reason.to_string());
    } else if let Some(reason) = status_finish_reason {
        choice.finish_reason = Some(reason.to_string());
    }

    // The canonical transformer always emits one choice, including for a
    // response whose output array is empty.
    if choice.message.is_none() {
        choice.message = Some(LlmMessage {
            role: Some("assistant".to_string()),
            content: Some(MessageContent::Text(String::new())),
            ..Default::default()
        });
    }
    response.choices.push(choice);

    // Previous response ID.
    if let Some(prev_id) = obj.get("previous_response_id").and_then(|v| v.as_str()) {
        response.previous_response_id = Some(prev_id.to_string());
    }

    Ok(response)
}

/// Transform a compact Responses API response into a unified [`LlmResponse`].
///
/// Mirrors Go `transformCompactResponse` (compact_outbound.go:74-125).
///
/// The compact response shape differs from the standard shape:
///
/// * `output` is an array of input items (not output items).
/// * `object` is `"response.compaction"`.
/// * `instructions` is returned as-is.
///
/// The output items must be re-converted to messages via
/// [`convert_responses_input_to_messages`] (from the inbound side).
pub fn transform_responses_compact_response(
    status: u16,
    body: &[u8],
) -> TransformerResult<LlmResponse> {
    if status >= 400 {
        return Err(ConduitError::upstream(format!(
            "Compact Responses API request failed (status {status})"
        )));
    }

    let raw: Value = serde_json::from_slice(body).map_err(|err| {
        ConduitError::internal("failed to parse Compact Responses API response").with_source(err)
    })?;

    let obj = raw
        .as_object()
        .ok_or_else(|| ConduitError::internal("Compact Responses API response is not an object"))?;

    let id = obj
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let created = obj.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
    let model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut response = LlmResponse {
        id: id.clone(),
        object: "response.compaction".to_string(),
        created,
        model: model.clone(),
        ..Default::default()
    };

    // Extract usage.
    if let Some(usage_obj) = obj.get("usage") {
        if let Ok(usage) = extract_usage_from_responses(usage_obj) {
            response.usage = Some(usage);
        }
    }

    // Build the compact response in the `compact` field.
    let mut compact_obj = Map::new();
    compact_obj.insert("id".to_string(), Value::String(id));
    compact_obj.insert("created_at".to_string(), Value::Number(created.into()));
    compact_obj.insert(
        "object".to_string(),
        Value::String("response.compaction".to_string()),
    );
    compact_obj.insert("model".to_string(), Value::String(model));

    if let Some(instructions) = obj.get("instructions").and_then(|v| v.as_str()) {
        compact_obj.insert(
            "instructions".to_string(),
            Value::String(instructions.to_string()),
        );
    }

    // Output items (the compact response returns the input items).
    if let Some(output) = obj.get("output") {
        compact_obj.insert("output".to_string(), output.clone());
    }

    response.compact = Some(Value::Object(compact_obj));

    Ok(response)
}

/// Convert Responses API `output[]` items to a unified [`LlmMessage`].
///
/// Mirrors Go `convertOutputToMessage` (responses/outbound_convert.go:607-766).
///
/// Returns `(Option<LlmMessage>, Option<&'static str>)` where the string is the
/// `finish_reason` if inferable from the response status or item state.
fn convert_output_to_message(
    items: Option<&Vec<Value>>,
) -> TransformerResult<(Option<LlmMessage>, Option<&'static str>)> {
    let Some(items) = items else {
        return Ok((None, None));
    };

    let mut message = LlmMessage {
        role: Some("assistant".to_string()),
        ..Default::default()
    };

    let mut content_parts: Vec<ContentPart> = Vec::new();
    let mut text_content = String::new();
    let mut has_tool_calls = false;
    let mut finish_reason = None;

    for item in items {
        let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match item_type {
            "message" => {
                // Message item with content.
                if let Some(content_items) = item.get("content").and_then(|v| v.as_array()) {
                    for content_item in content_items {
                        let ct = content_item
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        match ct {
                            "output_text" => {
                                if let Some(text) =
                                    content_item.get("text").and_then(|v| v.as_str())
                                {
                                    let start = text_content.len();
                                    text_content.push_str(text);
                                    // Collect annotations.
                                    if let Some(annotations) =
                                        content_item.get("annotations").and_then(|v| v.as_array())
                                    {
                                        for ann in annotations {
                                            if let Some(ann_obj) = ann.as_object() {
                                                if let Some(annotation) =
                                                    convert_annotation(ann_obj, start)
                                                {
                                                    message.annotations.push(annotation);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            "refusal" => {
                                if let Some(refusal) =
                                    content_item.get("refusal").and_then(Value::as_str)
                                {
                                    content_parts.push(ContentPart {
                                        part_type: "refusal".to_string(),
                                        text: Some(refusal.to_string()),
                                        ..Default::default()
                                    });
                                }
                            }
                            "input_image" => {
                                if let Some(url) =
                                    content_item.get("image_url").and_then(|v| v.as_str())
                                {
                                    let mut url_obj = Map::new();
                                    url_obj
                                        .insert("url".to_string(), Value::String(url.to_string()));
                                    content_parts.push(ContentPart {
                                        part_type: "image_url".to_string(),
                                        image_url: Some(Value::Object(url_obj)),
                                        ..Default::default()
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // Extract message ID.
                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                    message.id = Some(id.to_string());
                }
            }
            "output_text" => {
                // Standalone output text item.
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    let start = text_content.len();
                    text_content.push_str(text);
                    // Collect annotations.
                    if let Some(annotations) = item.get("annotations").and_then(|v| v.as_array()) {
                        for ann in annotations {
                            if let Some(ann_obj) = ann.as_object() {
                                if let Some(annotation) = convert_annotation(ann_obj, start) {
                                    message.annotations.push(annotation);
                                }
                            }
                        }
                    }
                }
            }
            "function_call" => {
                has_tool_calls = true;
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let mut function = Map::new();
                function.insert("name".to_string(), Value::String(name.clone()));
                function.insert("arguments".to_string(), Value::String(arguments.clone()));

                message.tool_calls.push(ToolCall {
                    id: Some(call_id),
                    call_type: "function".to_string(),
                    function: Value::Object(function),
                    ..Default::default()
                });
            }
            "custom_tool_call" => {
                has_tool_calls = true;
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = item
                    .get("input")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let mut custom_tool = Map::new();
                custom_tool.insert("call_id".to_string(), Value::String(call_id.clone()));
                custom_tool.insert("name".to_string(), Value::String(name.clone()));
                custom_tool.insert("input".to_string(), Value::String(input.clone()));

                let mut extra = BTreeMap::new();
                extra.insert(
                    "response_custom_tool".to_string(),
                    Value::Object(custom_tool),
                );

                message.tool_calls.push(ToolCall {
                    id: Some(call_id),
                    call_type: "responses_custom_tool".to_string(),
                    function: Value::Object(Map::new()),
                    extra,
                    ..Default::default()
                });
            }
            "reasoning" => {
                // Reasoning content.
                if let Some(summary_array) = item.get("summary").and_then(|v| v.as_array()) {
                    let mut reasoning_text = String::new();
                    for summary in summary_array {
                        if let Some(text) = summary.get("text").and_then(|v| v.as_str()) {
                            reasoning_text.push_str(text);
                        }
                    }
                    if !reasoning_text.is_empty() {
                        message.reasoning_content = Some(reasoning_text);
                    }
                }
                // Encrypted reasoning signature.
                if let Some(encrypted) = item.get("encrypted_content").and_then(|v| v.as_str()) {
                    message.reasoning_signature = Some(encrypted.to_string());
                }
            }
            "image_generation_call" => {
                // Image generation result.
                if let Some(result) = item.get("result").and_then(|v| v.as_str()) {
                    let output_format = item
                        .get("output_format")
                        .and_then(|v| v.as_str())
                        .unwrap_or("png");

                    let data_url = format!("data:image/{};base64,{}", output_format, result);

                    let mut url_obj = Map::new();
                    url_obj.insert("url".to_string(), Value::String(data_url));

                    content_parts.push(ContentPart {
                        part_type: "image_url".to_string(),
                        image_url: Some(Value::Object(url_obj)),
                        ..Default::default()
                    });
                }
            }
            _ => {
                // Unknown item types are silently skipped (Go parity).
            }
        }
    }

    // Build message content.
    if !text_content.is_empty() {
        if content_parts.is_empty() {
            message.content = Some(MessageContent::Text(text_content));
        } else {
            // Prepend text content before other parts.
            content_parts.insert(
                0,
                ContentPart {
                    part_type: "text".to_string(),
                    text: Some(text_content),
                    ..Default::default()
                },
            );
            message.content = Some(MessageContent::Parts(content_parts));
        }
    } else if !content_parts.is_empty() {
        message.content = Some(MessageContent::Parts(content_parts));
    }

    // Determine finish reason.
    if has_tool_calls {
        finish_reason = Some("tool_calls");
    }

    Ok((Some(message), finish_reason))
}

/// Convert a Responses API annotation to a unified annotation.
///
/// Mirrors Go `annotationToLLM` (responses/outbound_convert.go:533-554).
/// `text_offset` is the character offset where the annotated text starts in
/// the accumulated message content.
fn convert_annotation(ann_obj: &Map<String, Value>, text_offset: usize) -> Option<Annotation> {
    let ann_type = ann_obj.get("type").and_then(|v| v.as_str())?;
    let start_index =
        ann_obj.get("start_index").and_then(|v| v.as_i64())? as i64 + text_offset as i64;
    let end_index = ann_obj.get("end_index").and_then(|v| v.as_i64())? as i64 + text_offset as i64;

    let mut annotation = Annotation {
        annotation_type: Some(ann_type.to_string()),
        start_index: Some(start_index),
        end_index: Some(end_index),
        ..Default::default()
    };

    // URL citation if present.
    if let Some(url_citation) = ann_obj.get("url_citation") {
        if let Some(obj) = url_citation.as_object() {
            let url = obj
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let title = obj
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            annotation.url_citation = Some(UrlCitation {
                url: Some(url),
                title: Some(title),
            });
        }
    }

    Some(annotation)
}

/// Extract usage from a Responses API response.
///
/// Mirrors Go `Usage.ToUsage()` (responses/usage.go).
fn extract_usage_from_responses(usage_obj: &Value) -> TransformerResult<Usage> {
    let prompt_tokens = usage_obj
        .get("input_tokens")
        .or_else(|| usage_obj.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = usage_obj
        .get("output_tokens")
        .or_else(|| usage_obj.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_tokens = usage_obj
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mut usage = Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        ..Default::default()
    };

    // Responses names these input/output token details. Accept the historical
    // chat-completions aliases as a compatibility fallback.
    if let Some(details) = usage_obj
        .get("input_tokens_details")
        .or_else(|| usage_obj.get("prompt_tokens_details"))
    {
        if let Some(obj) = details.as_object() {
            if let Some(audio) = obj.get("audio_tokens").and_then(|v| v.as_u64()) {
                usage.prompt_details.audio_tokens = audio;
            }
            if let Some(cached) = obj.get("cached_tokens").and_then(|v| v.as_u64()) {
                usage.prompt_details.cached_tokens = cached;
            }
        }
    }

    if let Some(details) = usage_obj
        .get("output_tokens_details")
        .or_else(|| usage_obj.get("completion_tokens_details"))
    {
        if let Some(obj) = details.as_object() {
            if let Some(audio) = obj.get("audio_tokens").and_then(|v| v.as_u64()) {
                usage.completion_details.audio_tokens = audio;
            }
            if let Some(reasoning) = obj.get("reasoning_tokens").and_then(|v| v.as_u64()) {
                usage.completion_details.reasoning_tokens = reasoning;
            }
        }
    }

    Ok(usage)
}

// ---------------------------------------------------------------------------
// OpenAiResponsesOutbound — full outbound transformer struct.
//
// Mirrors Go `OutboundTransformer` (responses/outbound.go:145-151). Composes
// URL resolution, auth header construction, and body building into a single
// `outbound_request` method that produces a ready-to-send [`HttpRequest`].
// ---------------------------------------------------------------------------

/// Configuration for the Responses outbound transformer.
///
/// Mirrors Go `Config` (responses/outbound.go:35-54). Only the fields needed
/// for the pure outbound request transformation are modeled (the WebSocket
/// executor caching and `ChannelCustomizedExecutor` trait are transport concerns
/// outside this module's scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsesOutboundConfig {
    /// Base URL for the OpenAI Responses API (e.g. `https://api.openai.com/v1`).
    pub base_url: String,
    /// API key for Bearer auth.
    pub api_key: String,
    /// When `true`, the URL is used as-is (no `/responses` suffix). Auto-enabled
    /// when `base_url` ends with `##`.
    pub raw_url: bool,
    /// Custom endpoint path override (e.g. `/v2/responses`). When set, `v1` is
    /// not appended during URL normalization.
    pub endpoint_path: Option<String>,
}

/// OpenAI Responses API outbound transformer.
///
/// Mirrors Go `OutboundTransformer` (responses/outbound.go:145-151). Converts
/// a unified [`LlmRequest`] with a `Responses` payload into a fully-formed
/// [`HttpRequest`] ready for upstream dispatch.
///
/// # Go parity
///
/// The Go `OutboundTransformer.TransformRequest` (outbound.go:191-318):
/// 1. Validates the request.
/// 2. Retrieves the API key via the provider.
/// 3. Converts tools/input/instructions/reasoning to the Responses `Request{}`.
/// 4. Serializes to JSON.
/// 5. Constructs headers (`Content-Type`, `Accept`, `Authorization: Bearer`).
/// 6. Resolves the URL (`buildFullRequestURL`).
/// 7. Returns an `httpclient.Request`.
///
/// This struct reproduces that flow by composing:
/// - [`crate::openai_outbound::normalize_base_url`] for URL normalization.
/// - [`crate::openai_outbound::build_auth_header`] for auth/headers.
/// - [`build_responses_request_body`] / [`build_responses_compact_request_body`]
///   for body construction.
#[derive(Debug, Clone)]
pub struct OpenAiResponsesOutbound {
    config: ResponsesOutboundConfig,
    /// Normalized base URL (post-construction, mirrors Go's in-place mutation
    /// of `config.BaseURL` in `NewOutboundTransformerWithConfig`).
    normalized_base_url: String,
}

impl OpenAiResponsesOutbound {
    /// Create a new Responses outbound transformer with the given base URL and
    /// API key. Mirrors Go `NewOutboundTransformer` (responses/outbound.go:56-67).
    pub fn new(base_url: &str, api_key: &str) -> TransformerResult<Self> {
        let config = ResponsesOutboundConfig {
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
            raw_url: false,
            endpoint_path: None,
        };

        Self::with_config(config)
    }

    /// Create a new Responses outbound transformer with full configuration.
    /// Mirrors Go `NewOutboundTransformerWithConfig` (responses/outbound.go:69-92).
    pub fn with_config(mut config: ResponsesOutboundConfig) -> TransformerResult<Self> {
        // Mirrors Go `##` suffix detection (outbound.go:78-80).
        if let Some(stripped) = config.base_url.strip_suffix("##") {
            config.raw_url = true;
            config.base_url = stripped.to_string();
        }

        // URL normalization (outbound.go:81-86):
        // - EndpointPath set → NormalizeBaseURL(base, "")
        // - Otherwise → NormalizeBaseURL(base, "v1")
        let normalized_base_url = if config.raw_url {
            config.base_url.clone()
        } else if config.endpoint_path.as_deref().unwrap_or("").is_empty() {
            crate::openai_outbound::normalize_base_url(config.base_url.clone(), "v1")
        } else {
            crate::openai_outbound::normalize_base_url(config.base_url.clone(), "")
        };

        Ok(Self {
            config,
            normalized_base_url,
        })
    }

    /// Returns the API format this transformer handles.
    /// Mirrors Go `APIFormat()` (responses/outbound.go:152-154).
    pub fn api_format(&self) -> ApiFormat {
        ApiFormat::OpenAiResponses
    }

    /// Build the full outbound request URL.
    /// Mirrors Go `buildFullRequestURL` (responses/outbound.go:321-331).
    fn build_full_request_url_for(&self, compact: bool) -> Option<String> {
        if self.config.raw_url {
            return (!self.normalized_base_url.is_empty())
                .then(|| self.normalized_base_url.clone());
        }

        if let Some(ref path) = self.config.endpoint_path {
            if !path.is_empty() {
                return (!self.normalized_base_url.is_empty())
                    .then(|| format!("{}{}", self.normalized_base_url, path));
            }
        }

        if self.normalized_base_url.is_empty() {
            None
        } else if compact {
            Some(format!("{}/responses/compact", self.normalized_base_url))
        } else {
            Some(format!("{}/responses", self.normalized_base_url))
        }
    }

    fn request_path(&self, compact: bool) -> String {
        if let Some(path) = self
            .config
            .endpoint_path
            .as_ref()
            .filter(|path| !path.is_empty())
        {
            return path.clone();
        }

        if compact {
            DEFAULT_RESPONSES_COMPACT_PATH.to_string()
        } else {
            DEFAULT_RESPONSES_PATH.to_string()
        }
    }

    /// Transform a unified [`LlmRequest`] into a fully-formed [`HttpRequest`].
    ///
    /// Mirrors Go `OutboundTransformer.TransformRequest`
    /// (responses/outbound.go:191-318). Handles both standard and compact
    /// request types by dispatching to the appropriate body builder.
    ///
    /// # Errors
    ///
    /// Returns an error when:
    /// * The request is `None` (Go: "chat request is nil").
    /// * The payload is not `Responses` variant.
    /// * The model is missing.
    /// * Serialization fails.
    pub fn outbound_request(&self, llm_request: &LlmRequest) -> TransformerResult<HttpRequest> {
        let compact = matches!(llm_request.request_type, RequestType::Compact)
            || matches!(
                &llm_request.payload,
                LlmRequestPayload::Responses(payload) if payload.compact
            );

        // Build the request body based on request type.
        let body_value = match compact {
            true => build_responses_compact_request_body(llm_request)?,
            _ => build_responses_request_body(llm_request)?,
        };

        // Build auth headers (mirrors Go outbound.go:295-298).
        let mut headers = HeaderMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Accept".to_string(), "application/json".to_string());
        if !self.config.api_key.is_empty() {
            headers.insert(
                "Authorization".to_string(),
                format!("Bearer {}", self.config.api_key),
            );
        }

        // Resolve the full URL (mirrors Go outbound.go:299-303).
        let full_url = self.build_full_request_url_for(compact);
        let path = self.request_path(compact);
        let auth = (!self.config.api_key.is_empty()).then(|| HttpAuth {
            scheme: "bearer".to_string(),
            token: Some(self.config.api_key.clone()),
            ..HttpAuth::default()
        });

        Ok(HttpRequest {
            method: "POST".to_string(),
            url: full_url,
            path,
            headers,
            // Keep one canonical representation. The executor serializes
            // `json_body` after request middleware runs; pre-populating `body`
            // would make it win and could send stale JSON after overrides.
            body: None,
            json_body: Some(body_value),
            auth,
            request_type: Some(llm_request.request_type),
            api_format: Some(if compact {
                ApiFormat::OpenAiResponsesCompact
            } else {
                ApiFormat::OpenAiResponses
            }),
            skip_inbound_query_merge: true,
            ..HttpRequest::default()
        })
    }

    /// Transform a Responses API HTTP response body into a unified [`LlmResponse`].
    ///
    /// Mirrors Go `OutboundTransformer.TransformResponse`
    /// (responses/outbound.go:336-350). Routes to the appropriate handler based
    /// on whether the original request was a compact request.
    pub fn transform_response(
        &self,
        status: u16,
        body: &[u8],
        is_compact: bool,
    ) -> TransformerResult<LlmResponse> {
        if is_compact {
            transform_responses_compact_response(status, body)
        } else {
            transform_responses_response(status, body)
        }
    }
}

impl OutboundTransformer for OpenAiResponsesOutbound {
    fn name(&self) -> &'static str {
        "openai-responses"
    }

    fn outbound_request(&self, request: &LlmRequest) -> TransformerResult<HttpRequest> {
        OpenAiResponsesOutbound::outbound_request(self, request)
    }

    fn outbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        Ok(response)
    }

    fn outbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
        Ok(event)
    }

    fn outbound_error(&self, response: HttpResponse) -> TransformerResult<ConduitError> {
        let status = response.status;
        let headers = response.headers.clone();
        let parsed = response.json_body.clone().or_else(|| {
            response
                .body
                .as_deref()
                .and_then(|body| serde_json::from_slice::<Value>(body).ok())
        });
        let message = parsed
            .as_ref()
            .and_then(|body| body.get("error"))
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                response
                    .body
                    .as_deref()
                    .map(String::from_utf8_lossy)
                    .map(|body| body.trim().to_string())
                    .filter(|body| !body.is_empty())
            })
            .unwrap_or_else(|| "Responses API request failed".to_string());

        let client_status = if (400..=599).contains(&status) {
            status
        } else {
            502
        };
        let mut error = ConduitError::upstream(format!("[{status}] {message}"))
            .with_provider_status(status)
            .with_http_status(client_status)
            .with_safe_message(message)
            .with_provider_headers(headers);
        if let Some(body) = parsed {
            error = error.with_provider_body(body);
        }
        Ok(error)
    }

    fn transform_response(&self, response: HttpResponse) -> TransformerResult<LlmResponse> {
        let compact = response
            .metadata
            .get("request_type")
            .and_then(Value::as_str)
            .is_some_and(|request_type| request_type == "compact")
            || response
                .raw_request
                .as_ref()
                .and_then(|request| request.get("request_type"))
                .and_then(Value::as_str)
                .is_some_and(|request_type| request_type == "compact");

        let body = if let Some(body) = response.body {
            body
        } else if let Some(json_body) = response.json_body {
            serde_json::to_vec(&json_body).map_err(|error| {
                ConduitError::internal("failed to serialize Responses API response")
                    .with_source(error)
            })?
        } else {
            return Err(ConduitError::internal(
                "Responses API response body is empty",
            ));
        };

        OpenAiResponsesOutbound::transform_response(self, response.status, &body, compact)
    }

    fn transform_stream(
        &self,
        events: Box<dyn Iterator<Item = StreamEvent> + Send>,
    ) -> TransformerResult<Box<dyn Iterator<Item = LlmResponse> + Send>> {
        Ok(Box::new(ResponsesStreamIter::new(events)))
    }
}

#[derive(Debug, Clone, Default)]
struct ResponsesStreamToolCall {
    index: i64,
    name: String,
    namespace: Option<String>,
    arguments: String,
    custom: bool,
}

#[derive(Debug, Default)]
struct ResponsesStreamState {
    response_id: String,
    response_model: String,
    previous_response_id: Option<String>,
    created: i64,
    next_tool_index: i64,
    tool_calls: BTreeMap<String, ResponsesStreamToolCall>,
    item_to_call_id: BTreeMap<String, String>,
}

struct ResponsesStreamIter {
    inner: Box<dyn Iterator<Item = StreamEvent> + Send>,
    state: ResponsesStreamState,
    queued: VecDeque<LlmResponse>,
}

impl ResponsesStreamIter {
    fn new(inner: Box<dyn Iterator<Item = StreamEvent> + Send>) -> Self {
        Self {
            inner,
            state: ResponsesStreamState::default(),
            queued: VecDeque::new(),
        }
    }

    fn base_chunk(&self) -> LlmResponse {
        LlmResponse {
            id: self.state.response_id.clone(),
            object: "chat.completion.chunk".to_string(),
            created: self.state.created,
            model: self.state.response_model.clone(),
            previous_response_id: self.state.previous_response_id.clone(),
            request_type: Some(RequestType::Chat),
            api_format: Some(ApiFormat::OpenAiResponses),
            ..Default::default()
        }
    }

    fn update_identity(&mut self, response: &Value) {
        if let Some(id) = response.get("id").and_then(Value::as_str) {
            self.state.response_id = id.to_string();
        }
        if let Some(model) = response.get("model").and_then(Value::as_str) {
            self.state.response_model = model.to_string();
        }
        if let Some(created) = response.get("created_at").and_then(Value::as_i64) {
            self.state.created = created;
        }
        if let Some(previous) = response.get("previous_response_id").and_then(Value::as_str) {
            self.state.previous_response_id = Some(previous.to_string());
        }
    }

    fn call_id_for_item(&self, item_id: &str) -> String {
        self.state
            .item_to_call_id
            .get(item_id)
            .cloned()
            .unwrap_or_else(|| item_id.to_string())
    }

    fn transform_event(&mut self, event: StreamEvent) -> TransformerResult<Vec<LlmResponse>> {
        let Some(value) = parse_responses_stream_event(&event)? else {
            return Ok(Vec::new());
        };
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .or(event.event_type.as_deref())
            .unwrap_or("");

        match event_type {
            "response.created" => {
                if let Some(response) = value.get("response") {
                    self.update_identity(response);
                }
                let mut chunk = self.base_chunk();
                chunk.choices.push(Choice {
                    index: 0,
                    delta: Some(LlmMessage {
                        role: Some("assistant".to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                if let Some(usage) = value
                    .get("response")
                    .and_then(|response| response.get("usage"))
                {
                    chunk.usage = Some(extract_usage_from_responses(usage)?);
                }
                Ok(vec![chunk])
            }
            "response.in_progress" => {
                if let Some(response) = value.get("response") {
                    self.update_identity(response);
                }
                Ok(Vec::new())
            }
            "response.output_item.added" => {
                let Some(item) = value.get("item") else {
                    return Ok(Vec::new());
                };
                match item.get("type").and_then(Value::as_str).unwrap_or("") {
                    "function_call" | "custom_tool_call" => {
                        let custom =
                            item.get("type").and_then(Value::as_str) == Some("custom_tool_call");
                        let call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .or_else(|| item.get("id").and_then(Value::as_str))
                            .unwrap_or("")
                            .to_string();
                        let item_id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or(&call_id)
                            .to_string();
                        let index = self.state.next_tool_index;
                        self.state.next_tool_index += 1;
                        let call = ResponsesStreamToolCall {
                            index,
                            name: item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            namespace: item
                                .get("namespace")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            custom,
                            ..Default::default()
                        };
                        self.state.item_to_call_id.insert(item_id, call_id.clone());
                        self.state.tool_calls.insert(call_id.clone(), call.clone());

                        let tool_call = stream_tool_call_start(&call_id, &call);
                        let mut chunk = self.base_chunk();
                        chunk.choices.push(Choice {
                            index: 0,
                            delta: Some(LlmMessage {
                                tool_calls: vec![tool_call],
                                ..Default::default()
                            }),
                            ..Default::default()
                        });
                        Ok(vec![chunk])
                    }
                    _ => Ok(Vec::new()),
                }
            }
            "response.function_call_arguments.delta" => {
                let item_id = value.get("item_id").and_then(Value::as_str).unwrap_or("");
                let call_id = self.call_id_for_item(item_id);
                let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
                let Some(call) = self.state.tool_calls.get_mut(&call_id) else {
                    return Ok(Vec::new());
                };
                call.arguments.push_str(delta);
                let index = call.index;
                let mut extra = BTreeMap::new();
                extra.insert("index".to_string(), Value::from(index));
                let tool_call = ToolCall {
                    id: None,
                    call_type: String::new(),
                    function: serde_json::json!({"arguments": delta}),
                    extra,
                };
                let mut chunk = self.base_chunk();
                chunk.choices.push(Choice {
                    index: 0,
                    delta: Some(LlmMessage {
                        tool_calls: vec![tool_call],
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                Ok(vec![chunk])
            }
            "response.function_call_arguments.done" => {
                let call_id = value.get("call_id").and_then(Value::as_str).unwrap_or("");
                if let Some(call) = self.state.tool_calls.get_mut(call_id) {
                    if let Some(name) = value.get("name").and_then(Value::as_str) {
                        call.name = name.to_string();
                    }
                    call.namespace = value
                        .get("namespace")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| call.namespace.clone());
                    if let Some(arguments) = value.get("arguments").and_then(Value::as_str) {
                        call.arguments = arguments.to_string();
                    }
                }
                Ok(Vec::new())
            }
            "response.custom_tool_call_input.delta" => {
                let item_id = value.get("item_id").and_then(Value::as_str).unwrap_or("");
                let call_id = self.call_id_for_item(item_id);
                let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
                let Some(call) = self.state.tool_calls.get_mut(&call_id) else {
                    return Ok(Vec::new());
                };
                call.arguments.push_str(delta);
                let mut extra = BTreeMap::new();
                extra.insert("index".to_string(), Value::from(call.index));
                extra.insert(
                    "response_custom_tool".to_string(),
                    serde_json::json!({
                        "call_id": call_id,
                        "name": call.name,
                        "input": delta,
                    }),
                );
                let tool_call = ToolCall {
                    id: None,
                    call_type: "responses_custom_tool".to_string(),
                    function: Value::Object(Map::new()),
                    extra,
                };
                let mut chunk = self.base_chunk();
                chunk.choices.push(Choice {
                    index: 0,
                    delta: Some(LlmMessage {
                        tool_calls: vec![tool_call],
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                Ok(vec![chunk])
            }
            "response.custom_tool_call_input.done" => {
                let item_id = value.get("item_id").and_then(Value::as_str).unwrap_or("");
                let call_id = self.call_id_for_item(item_id);
                if let Some(call) = self.state.tool_calls.get_mut(&call_id)
                    && let Some(input) = value.get("input").and_then(Value::as_str)
                {
                    call.arguments = input.to_string();
                }
                Ok(Vec::new())
            }
            "response.output_text.delta" => {
                let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
                let mut chunk = self.base_chunk();
                chunk.choices.push(Choice {
                    index: 0,
                    delta: Some(LlmMessage {
                        content: Some(MessageContent::Text(delta.to_string())),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                Ok(vec![chunk])
            }
            "response.refusal.delta" => {
                let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
                let mut chunk = self.base_chunk();
                chunk.choices.push(Choice {
                    index: 0,
                    delta: Some(LlmMessage {
                        content: Some(MessageContent::Parts(vec![ContentPart {
                            part_type: "refusal".to_string(),
                            text: Some(delta.to_string()),
                            ..Default::default()
                        }])),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                Ok(vec![chunk])
            }
            "response.refusal.done" => Ok(Vec::new()),
            "response.reasoning_summary_text.delta" => {
                let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
                let mut chunk = self.base_chunk();
                chunk.choices.push(Choice {
                    index: 0,
                    delta: Some(LlmMessage {
                        reasoning_content: Some(delta.to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                Ok(vec![chunk])
            }
            "response.output_item.done" => {
                let Some(item) = value.get("item") else {
                    return Ok(Vec::new());
                };
                if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                    return Ok(Vec::new());
                }
                let Some(signature) = item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .filter(|signature| !signature.is_empty())
                else {
                    return Ok(Vec::new());
                };
                let mut chunk = self.base_chunk();
                chunk.choices.push(Choice {
                    index: 0,
                    delta: Some(LlmMessage {
                        reasoning_signature:
                            crate::openai_compatible::encode_openai_encrypted_content(Some(
                                signature.to_string(),
                            )),
                        ..Default::default()
                    }),
                    ..Default::default()
                });
                Ok(vec![chunk])
            }
            "response.completed"
            | "response.failed"
            | "response.incomplete"
            | "response.cancelled"
            | "response.canceled" => {
                if let Some(response) = value.get("response") {
                    self.update_identity(response);
                }
                let finish_reason = match event_type {
                    "response.completed" if !self.state.tool_calls.is_empty() => "tool_calls",
                    "response.completed" => "stop",
                    "response.failed" => "error",
                    "response.incomplete" => "length",
                    _ => "cancelled",
                };
                let mut finish = self.base_chunk();
                finish.choices.push(Choice {
                    index: 0,
                    delta: Some(LlmMessage::default()),
                    finish_reason: Some(finish_reason.to_string()),
                    ..Default::default()
                });
                let mut output = vec![finish];
                if let Some(usage) = value
                    .get("response")
                    .and_then(|response| response.get("usage"))
                    .or_else(|| value.get("usage"))
                {
                    let mut usage_chunk = self.base_chunk();
                    usage_chunk.usage = Some(extract_usage_from_responses(usage)?);
                    output.push(usage_chunk);
                }
                Ok(output)
            }
            "error" => {
                let mut chunk = self.base_chunk();
                chunk.error = Some(ResponseError {
                    detail: ErrorDetail {
                        code: value
                            .get("code")
                            .and_then(Value::as_str)
                            .unwrap_or("stream_error")
                            .to_string(),
                        message: value
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Responses API stream error")
                            .to_string(),
                        detail_type: "api_error".to_string(),
                        param: value
                            .get("param")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        ..Default::default()
                    },
                    ..Default::default()
                });
                Ok(vec![chunk])
            }
            _ => Ok(Vec::new()),
        }
    }
}

impl Iterator for ResponsesStreamIter {
    type Item = LlmResponse;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(response) = self.queued.pop_front() {
                return Some(response);
            }
            let event = self.inner.next()?;
            // The trait cannot surface per-item parse errors. This matches the
            // other provider adapters: malformed SSE events are skipped while
            // the remaining stream stays consumable.
            if let Ok(responses) = self.transform_event(event) {
                self.queued.extend(responses);
            }
        }
    }
}

fn parse_responses_stream_event(event: &StreamEvent) -> TransformerResult<Option<Value>> {
    if event.done {
        return Ok(None);
    }
    if let Some(value) = event.json_data.as_ref() {
        return Ok(Some(value.clone()));
    }
    let Some(data) = event.data.as_deref() else {
        return Ok(None);
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    let data = data.strip_prefix("data:").map(str::trim).unwrap_or(data);
    serde_json::from_str(data).map(Some).map_err(|error| {
        ConduitError::internal("failed to unmarshal Responses API stream event").with_source(error)
    })
}

fn stream_tool_call_start(call_id: &str, call: &ResponsesStreamToolCall) -> ToolCall {
    let mut extra = BTreeMap::new();
    extra.insert("index".to_string(), Value::from(call.index));
    if call.custom {
        extra.insert(
            "response_custom_tool".to_string(),
            serde_json::json!({
                "call_id": call_id,
                "name": call.name,
                "input": "",
            }),
        );
        ToolCall {
            id: Some(call_id.to_string()),
            call_type: "responses_custom_tool".to_string(),
            function: Value::Object(Map::new()),
            extra,
        }
    } else {
        let mut function = Map::new();
        function.insert("name".to_string(), Value::String(call.name.clone()));
        function.insert("arguments".to_string(), Value::String(String::new()));
        if let Some(namespace) = &call.namespace {
            function.insert("namespace".to_string(), Value::String(namespace.clone()));
        }
        ToolCall {
            id: Some(call_id.to_string()),
            call_type: "function".to_string(),
            function: Value::Object(function),
            extra,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — mirror Go `outbound_test.go` and `compact_outbound_test.go`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::model::ResponsesRequest;
    use serde_json::json;

    fn llm_request_defaults() -> LlmRequest {
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiResponses,
            model: None,
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest::default()),
            extra_body: Default::default(),
            extra_headers: Default::default(),
            metadata: Default::default(),
            extra: Default::default(),
        }
    }

    fn unified_tool_defaults() -> conduit_llm::model::UnifiedTool {
        conduit_llm::model::UnifiedTool {
            tool_type: String::new(),
            name: None,
            description: None,
            parameters: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn test_build_standard_responses_request_body() -> TransformerResult<()> {
        let llm_request = LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiResponses,
            model: Some("gpt-4o".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: Some(json!("Hello, world!")),
                instructions: Some("You are a helpful assistant.".to_string()),
                tools: vec![],
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let body = build_responses_request_body(&llm_request)?;

        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["input"], "Hello, world!");
        assert_eq!(body["instructions"], "You are a helpful assistant.");
        assert_eq!(body["stream"], false);

        Ok(())
    }

    #[test]
    fn test_build_compact_responses_request_body() -> TransformerResult<()> {
        let llm_request = LlmRequest {
            request_type: RequestType::Compact,
            api_format: ApiFormat::OpenAiResponsesCompact,
            model: Some("gpt-4o".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: Some(json!([
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Hi"}]}
                ])),
                instructions: Some("Be concise.".to_string()),
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let body = build_responses_compact_request_body(&llm_request)?;

        assert_eq!(body["model"], "gpt-4o");
        assert!(body["input"].is_array());
        assert_eq!(body["instructions"], "Be concise.");

        Ok(())
    }

    #[test]
    fn test_build_responses_request_with_function_tool() -> TransformerResult<()> {
        let tool = conduit_llm::model::UnifiedTool {
            tool_type: "function".to_string(),
            name: Some("get_weather".to_string()),
            description: Some("Get current weather.".to_string()),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                }
            })),
            ..unified_tool_defaults()
        };

        let llm_request = LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiResponses,
            model: Some("gpt-4o".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: Some(json!("What's the weather?")),
                tools: vec![tool],
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let body = build_responses_request_body(&llm_request)?;

        assert!(body["tools"].is_array());
        let tools = body["tools"]
            .as_array()
            .ok_or_else(|| ConduitError::internal("expected tools array"))?;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["description"], "Get current weather.");

        Ok(())
    }

    #[test]
    fn test_transform_standard_response() -> TransformerResult<()> {
        let response_body = json!({
            "id": "resp_123",
            "object": "response",
            "created_at": 1700000000,
            "model": "gpt-4o",
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "Hello!"}
                    ]
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });

        let body_bytes = serde_json::to_vec(&response_body)
            .map_err(|err| ConduitError::internal("serialize response body").with_source(err))?;
        let response = transform_responses_response(200, &body_bytes)?;

        assert_eq!(response.id, "resp_123");
        assert_eq!(response.object, "chat.completion");
        assert_eq!(response.model, "gpt-4o");
        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].finish_reason.as_deref(), Some("stop"));

        if let Some(ref msg) = response.choices[0].message {
            assert_eq!(msg.role.as_deref(), Some("assistant"));
        }

        assert_eq!(response.usage.as_ref().map(|u| u.total_tokens), Some(15));

        Ok(())
    }

    #[test]
    fn test_transform_response_with_reasoning() -> TransformerResult<()> {
        let response_body = json!({
            "id": "resp_456",
            "object": "response",
            "created_at": 1700000000,
            "model": "o1-preview",
            "status": "completed",
            "output": [
                {
                    "type": "reasoning",
                    "summary": [
                        {"type": "summary_text", "text": "Let me think..."}
                    ]
                },
                {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "The answer is 42."}
                    ]
                }
            ]
        });

        let body_bytes = serde_json::to_vec(&response_body)
            .map_err(|err| ConduitError::internal("serialize response body").with_source(err))?;
        let response = transform_responses_response(200, &body_bytes)?;

        assert_eq!(response.choices.len(), 1);

        if let Some(ref msg) = response.choices[0].message {
            assert_eq!(msg.reasoning_content.as_deref(), Some("Let me think..."));
        }

        Ok(())
    }

    #[test]
    fn test_transform_response_with_function_call() -> TransformerResult<()> {
        let response_body = json!({
            "id": "resp_789",
            "object": "response",
            "created_at": 1700000000,
            "model": "gpt-4o",
            "status": "completed",
            "output": [
                {
                    "type": "function_call",
                    "call_id": "call_123",
                    "name": "get_weather",
                    "arguments": "{\"location\":\"Tokyo\"}"
                }
            ]
        });

        let body_bytes = serde_json::to_vec(&response_body)
            .map_err(|err| ConduitError::internal("serialize response body").with_source(err))?;
        let response = transform_responses_response(200, &body_bytes)?;

        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );

        if let Some(ref msg) = response.choices[0].message {
            assert_eq!(msg.tool_calls.len(), 1);
            assert_eq!(msg.tool_calls[0].id.as_deref(), Some("call_123"));
            assert_eq!(msg.tool_calls[0].call_type, "function");
        }

        Ok(())
    }

    #[test]
    fn test_transform_compact_response() -> TransformerResult<()> {
        let response_body = json!({
            "id": "compact_123",
            "object": "response.compaction",
            "created_at": 1700000000,
            "model": "gpt-4o",
            "instructions": "Summarize this.",
            "output": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Hi"}]}
            ],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 2,
                "total_tokens": 7
            }
        });

        let body_bytes = serde_json::to_vec(&response_body)
            .map_err(|err| ConduitError::internal("serialize response body").with_source(err))?;
        let response = transform_responses_compact_response(200, &body_bytes)?;

        assert_eq!(response.id, "compact_123");
        assert_eq!(response.object, "response.compaction");

        let compact = response
            .compact
            .as_ref()
            .ok_or_else(|| ConduitError::internal("expected compact field"))?;
        assert_eq!(compact["instructions"], "Summarize this.");
        assert!(compact["output"].is_array());

        assert_eq!(response.usage.as_ref().map(|u| u.total_tokens), Some(7));

        Ok(())
    }

    #[test]
    fn test_build_responses_request_rejects_compact_request_type() {
        let llm_request = LlmRequest {
            request_type: RequestType::Compact,
            api_format: ApiFormat::OpenAiResponses,
            model: Some("gpt-4o".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: Some(json!("test")),
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let result = build_responses_request_body(&llm_request);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_responses_request_requires_model() {
        let llm_request = LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiResponses,
            model: None,
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: Some(json!("test")),
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let result = build_responses_request_body(&llm_request);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_responses_request_requires_input() {
        let llm_request = LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiResponses,
            model: Some("gpt-4o".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: None,
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let result = build_responses_request_body(&llm_request);
        assert!(result.is_err());
    }

    // ---- OpenAiResponsesOutbound struct tests --------------------------------
    // Mirror Go `outbound_test.go` cases for the full transformer struct.

    #[test]
    fn test_responses_outbound_new_valid() -> TransformerResult<()> {
        // Mirrors Go TestNewOutboundTransformer "valid parameters" case.
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com", "test-api-key")?;
        assert_eq!(outbound.api_format(), ApiFormat::OpenAiResponses);
        // Base URL should be normalized with v1.
        assert_eq!(outbound.normalized_base_url, "https://api.openai.com/v1");
        Ok(())
    }

    #[test]
    fn test_responses_outbound_allows_shared_transformer_without_api_key() -> TransformerResult<()>
    {
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com", "")?;
        assert_eq!(outbound.normalized_base_url, "https://api.openai.com/v1");
        Ok(())
    }

    #[test]
    fn test_responses_outbound_allows_candidate_supplied_base_url() -> TransformerResult<()> {
        let outbound = OpenAiResponsesOutbound::new("", "")?;
        assert!(outbound.normalized_base_url.is_empty());
        assert_eq!(outbound.request_path(false), DEFAULT_RESPONSES_PATH);
        Ok(())
    }

    #[test]
    fn test_responses_outbound_build_url_default() -> TransformerResult<()> {
        // Mirrors Go TestOutboundTransformer_buildFullRequestURL "no v1 prefix".
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com", "test-key")?;
        let url = outbound.build_full_request_url_for(false);
        assert_eq!(url.as_deref(), Some("https://api.openai.com/v1/responses"));
        Ok(())
    }

    #[test]
    fn test_responses_outbound_build_url_with_v1_suffix() -> TransformerResult<()> {
        // Mirrors Go TestOutboundTransformer_buildFullRequestURL "with v1 suffix".
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com/v1", "test-key")?;
        let url = outbound.build_full_request_url_for(false);
        assert_eq!(url.as_deref(), Some("https://api.openai.com/v1/responses"));
        Ok(())
    }

    #[test]
    fn test_responses_outbound_build_url_with_v1_in_path() -> TransformerResult<()> {
        // Mirrors Go TestOutboundTransformer_buildFullRequestURL "with v1 in path".
        let outbound =
            OpenAiResponsesOutbound::new("https://api.openai.com/v1/custom", "test-key")?;
        let url = outbound.build_full_request_url_for(false);
        assert_eq!(
            url.as_deref(),
            Some("https://api.openai.com/v1/custom/responses")
        );
        Ok(())
    }

    #[test]
    fn test_responses_outbound_build_url_hash_suffix() -> TransformerResult<()> {
        // Mirrors Go TestOutboundTransformer_buildFullRequestURL "raw url with # suffix".
        // The `#` at end is processed by normalize_base_url (not `##`).
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com/custom#", "test-key")?;
        let url = outbound.build_full_request_url_for(false);
        // With trailing `#`, normalize_base_url strips it and skips version.
        // Then `/responses` is appended.
        assert_eq!(
            url.as_deref(),
            Some("https://api.openai.com/custom/responses")
        );
        Ok(())
    }

    #[test]
    fn test_responses_outbound_build_url_double_hash_raw() -> TransformerResult<()> {
        // Mirrors Go: base URL ending with `##` → raw_url = true, `##` stripped.
        let config = ResponsesOutboundConfig {
            base_url: "https://api.openai.com/custom##".to_string(),
            api_key: "test-key".to_string(),
            raw_url: false,
            endpoint_path: None,
        };
        let outbound = OpenAiResponsesOutbound::with_config(config)?;
        let url = outbound.build_full_request_url_for(false);
        // Raw URL mode → base returned as-is (no `/responses`).
        assert_eq!(url.as_deref(), Some("https://api.openai.com/custom"));
        Ok(())
    }

    #[test]
    fn test_responses_outbound_build_url_custom_endpoint_path() -> TransformerResult<()> {
        // Custom endpoint_path overrides the default `/responses`.
        let config = ResponsesOutboundConfig {
            base_url: "https://custom-endpoint.com/api".to_string(),
            api_key: "test-key".to_string(),
            raw_url: false,
            endpoint_path: Some("/v2/responses".to_string()),
        };
        let outbound = OpenAiResponsesOutbound::with_config(config)?;
        let url = outbound.build_full_request_url_for(false);
        assert_eq!(
            url.as_deref(),
            Some("https://custom-endpoint.com/api/v2/responses")
        );
        Ok(())
    }

    #[test]
    fn test_responses_outbound_request_simple_text() -> TransformerResult<()> {
        // Mirrors Go TestOutboundTransformer_TransformRequest "simple text request".
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com", "test-api-key")?;

        let llm_request = LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiResponses,
            model: Some("gpt-4o".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: Some(json!("Hello, world!")),
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let http_req = outbound.outbound_request(&llm_request)?;

        assert_eq!(http_req.method, "POST");
        assert_eq!(
            http_req.url.as_deref(),
            Some("https://api.openai.com/v1/responses")
        );
        assert_eq!(
            http_req.headers.get("Content-Type"),
            Some(&"application/json".to_string())
        );
        assert_eq!(
            http_req.headers.get("Accept"),
            Some(&"application/json".to_string())
        );
        assert_eq!(
            http_req.headers.get("Authorization"),
            Some(&"Bearer test-api-key".to_string())
        );

        // Verify auth config.
        let auth = http_req
            .auth
            .as_ref()
            .ok_or_else(|| ConduitError::internal("expected auth"))?;
        assert_eq!(auth.scheme, "bearer");
        assert_eq!(auth.token.as_deref(), Some("test-api-key"));

        // Verify body contains model and input.
        let body = outbound_json(&http_req)?;
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["input"], "Hello, world!");
        assert_eq!(body["stream"], false);

        Ok(())
    }

    #[test]
    fn test_responses_outbound_request_with_instructions() -> TransformerResult<()> {
        // Mirrors Go TestOutboundTransformer_TransformRequest "request with system message".
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com", "test-api-key")?;

        let llm_request = LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiResponses,
            model: Some("gpt-4o".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: Some(json!("Hello!")),
                instructions: Some("You are a helpful assistant.".to_string()),
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let http_req = outbound.outbound_request(&llm_request)?;

        let body = outbound_json(&http_req)?;
        assert_eq!(body["instructions"], "You are a helpful assistant.");

        Ok(())
    }

    #[test]
    fn test_responses_outbound_request_with_streaming() -> TransformerResult<()> {
        // Mirrors Go TestOutboundTransformer_TransformRequest "request with streaming enabled".
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com", "test-api-key")?;

        let llm_request = LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiResponses,
            model: Some("gpt-4o".to_string()),
            stream: true,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: Some(json!("Hello")),
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let http_req = outbound.outbound_request(&llm_request)?;

        let body = outbound_json(&http_req)?;
        assert_eq!(body["stream"], true);

        Ok(())
    }

    #[test]
    fn test_responses_outbound_request_with_tools() -> TransformerResult<()> {
        // Mirrors Go TestOutboundTransformer_TransformRequest "request with function tool".
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com", "test-api-key")?;

        let tool = conduit_llm::model::UnifiedTool {
            tool_type: "function".to_string(),
            name: Some("get_weather".to_string()),
            description: Some("Get weather information".to_string()),
            parameters: Some(
                json!({"type": "object", "properties": {"location": {"type": "string"}}}),
            ),
            ..unified_tool_defaults()
        };

        let llm_request = LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::OpenAiResponses,
            model: Some("gpt-4o".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: Some(json!("What's the weather?")),
                tools: vec![tool],
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let http_req = outbound.outbound_request(&llm_request)?;

        let body = outbound_json(&http_req)?;

        let tools = body["tools"]
            .as_array()
            .ok_or_else(|| ConduitError::internal("expected tools array"))?;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["description"], "Get weather information");

        Ok(())
    }

    #[test]
    fn test_responses_outbound_transform_response() -> TransformerResult<()> {
        // Mirrors Go TestOutboundTransformer_TransformResponse "valid response with text output".
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com", "test-api-key")?;

        let response_body = serde_json::to_vec(&json!({
            "id": "resp_123",
            "object": "response",
            "created_at": 1700000000,
            "model": "gpt-4o",
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "id": "msg_123",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "Hello! How can I help you?"}
                    ]
                }
            ],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        }))
        .map_err(|e| ConduitError::internal("serialize").with_source(e))?;

        let response = outbound.transform_response(200, &response_body, false)?;

        assert_eq!(response.id, "resp_123");
        assert_eq!(response.model, "gpt-4o");
        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].finish_reason.as_deref(), Some("stop"));

        let usage = response
            .usage
            .as_ref()
            .ok_or_else(|| ConduitError::internal("expected usage"))?;
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.total_tokens, 30);

        Ok(())
    }

    #[test]
    fn non_stream_refusal_round_trips_as_typed_content() -> TransformerResult<()> {
        let outbound = OpenAiResponsesOutbound::new("", "")?;
        let unified = OutboundTransformer::transform_response(
            &outbound,
            HttpResponse {
                status: 200,
                json_body: Some(json!({
                    "id": "resp_refusal",
                    "object": "response",
                    "created_at": 1700000000,
                    "model": "gpt-test",
                    "status": "completed",
                    "output": [{
                        "type": "message",
                        "id": "msg_refusal",
                        "role": "assistant",
                        "content": [{
                            "type": "refusal",
                            "refusal": "I cannot assist with that."
                        }]
                    }]
                })),
                ..HttpResponse::default()
            },
        )?;

        let content = unified.choices[0]
            .message
            .as_ref()
            .and_then(|message| message.content.as_ref())
            .ok_or_else(|| ConduitError::internal("missing unified refusal content"))?;
        let MessageContent::Parts(parts) = content else {
            return Err(ConduitError::internal(
                "refusal must remain a typed content part",
            ));
        };
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].part_type, "refusal");
        assert_eq!(parts[0].text.as_deref(), Some("I cannot assist with that."));

        let client_response = crate::openai_responses_inbound::transform_response(unified, false)?;
        let client_body = client_response
            .json_body
            .ok_or_else(|| ConduitError::internal("missing client Responses body"))?;
        assert_eq!(
            client_body["output"][0]["content"][0],
            json!({
                "type": "refusal",
                "refusal": "I cannot assist with that.",
            })
        );
        Ok(())
    }

    #[test]
    fn test_responses_outbound_compact_request() -> TransformerResult<()> {
        // Verify that the struct dispatches compact requests correctly.
        let outbound = OpenAiResponsesOutbound::new("https://api.openai.com", "test-api-key")?;

        let llm_request = LlmRequest {
            request_type: RequestType::Compact,
            api_format: ApiFormat::OpenAiResponsesCompact,
            model: Some("gpt-4o".to_string()),
            stream: false,
            payload: LlmRequestPayload::Responses(ResponsesRequest {
                input: Some(json!([
                    {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Hi"}]}
                ])),
                instructions: Some("Be concise.".to_string()),
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let http_req = outbound.outbound_request(&llm_request)?;

        let body = outbound_json(&http_req)?;

        assert_eq!(body["model"], "gpt-4o");
        assert!(body["input"].is_array());
        assert_eq!(body["instructions"], "Be concise.");
        // Compact requests do not carry `stream` field.
        assert!(body.get("stream").is_none());
        assert_eq!(http_req.path, DEFAULT_RESPONSES_COMPACT_PATH);
        assert_eq!(
            http_req.url.as_deref(),
            Some("https://api.openai.com/v1/responses/compact")
        );
        assert_eq!(http_req.api_format, Some(ApiFormat::OpenAiResponsesCompact));

        Ok(())
    }

    #[test]
    fn chat_payload_builds_native_responses_input_and_tools() -> TransformerResult<()> {
        let tool_call = ToolCall {
            id: Some("call_weather".to_string()),
            call_type: "function".to_string(),
            function: json!({
                "name": "get_weather",
                "arguments": "{\"city\":\"Shanghai\"}"
            }),
            ..Default::default()
        };
        let request = LlmRequest {
            model: Some("gpt-5".to_string()),
            payload: LlmRequestPayload::Chat(ChatRequest {
                messages: vec![
                    ChatMessage {
                        role: "system".to_string(),
                        content: Some(MessageContent::Text("Be concise.".to_string())),
                        ..chat_message_defaults()
                    },
                    ChatMessage {
                        role: "user".to_string(),
                        content: Some(MessageContent::Parts(vec![
                            ContentPart {
                                part_type: "text".to_string(),
                                text: Some("Weather?".to_string()),
                                ..Default::default()
                            },
                            ContentPart {
                                part_type: "image_url".to_string(),
                                image_url: Some(json!({"url": "https://example.test/a.png"})),
                                ..Default::default()
                            },
                        ])),
                        ..chat_message_defaults()
                    },
                    ChatMessage {
                        role: "assistant".to_string(),
                        tool_calls: vec![tool_call],
                        ..chat_message_defaults()
                    },
                    ChatMessage {
                        role: "tool".to_string(),
                        tool_call_id: Some("call_weather".to_string()),
                        content: Some(MessageContent::Text("Sunny".to_string())),
                        ..chat_message_defaults()
                    },
                ],
                tools: vec![conduit_llm::model::UnifiedTool {
                    tool_type: "function".to_string(),
                    name: Some("get_weather".to_string()),
                    parameters: Some(json!({"type": "object", "properties": {}})),
                    ..unified_tool_defaults()
                }],
                tool_choice: Some(json!({
                    "type": "function",
                    "function": {"name": "get_weather"}
                })),
                max_tokens: Some(256),
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let body = build_responses_request_body(&request)?;
        assert_eq!(body["instructions"], "Be concise.");
        assert_eq!(body["max_output_tokens"], 256);
        assert_eq!(
            body["tool_choice"],
            json!({"type": "function", "name": "get_weather"})
        );
        let input = body["input"]
            .as_array()
            .ok_or_else(|| ConduitError::internal("expected Responses input items"))?;
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[0]["content"][1]["type"], "input_image");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["output"], "Sunny");
        Ok(())
    }

    #[test]
    fn shared_transformer_emits_authoritative_path_without_fixed_url() -> TransformerResult<()> {
        let outbound = OpenAiResponsesOutbound::new("", "")?;
        let request = LlmRequest {
            api_format: ApiFormat::OpenAiChatCompletions,
            model: Some("gpt-5".to_string()),
            payload: LlmRequestPayload::Chat(ChatRequest {
                messages: vec![ChatMessage {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text("Hello".to_string())),
                    ..chat_message_defaults()
                }],
                ..Default::default()
            }),
            ..llm_request_defaults()
        };

        let http_request = OutboundTransformer::outbound_request(&outbound, &request)?;
        assert_eq!(http_request.path, "/v1/responses");
        assert!(http_request.url.is_none());
        assert!(http_request.auth.is_none());
        assert!(!http_request.headers.contains_key("Authorization"));
        assert_eq!(http_request.request_type, Some(RequestType::Chat));
        assert_eq!(http_request.api_format, Some(ApiFormat::OpenAiResponses));
        assert_eq!(
            http_request.json_body.as_ref().map(|body| &body["input"]),
            Some(&json!("Hello"))
        );
        Ok(())
    }

    #[test]
    fn trait_non_stream_response_reads_responses_usage_fields() -> TransformerResult<()> {
        let outbound = OpenAiResponsesOutbound::new("", "")?;
        let unified = OutboundTransformer::transform_response(
            &outbound,
            HttpResponse {
                status: 200,
                json_body: Some(json!({
                    "id": "resp_usage",
                    "model": "gpt-5",
                    "created_at": 123,
                    "status": "completed",
                    "output": [{
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "done"}]
                    }],
                    "usage": {
                        "input_tokens": 11,
                        "input_tokens_details": {"cached_tokens": 3},
                        "output_tokens": 7,
                        "output_tokens_details": {"reasoning_tokens": 2},
                        "total_tokens": 18
                    }
                })),
                ..Default::default()
            },
        )?;

        let usage = unified
            .usage
            .ok_or_else(|| ConduitError::internal("expected usage"))?;
        assert_eq!(unified.object, "chat.completion");
        assert_eq!(usage.prompt_tokens, 11);
        assert_eq!(usage.prompt_details.cached_tokens, 3);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(usage.completion_details.reasoning_tokens, 2);
        assert_eq!(usage.total_tokens, 18);
        Ok(())
    }

    #[test]
    fn trait_stream_converts_text_tool_finish_and_usage() -> TransformerResult<()> {
        fn sse(value: Value) -> StreamEvent {
            StreamEvent {
                data: Some(value.to_string()),
                ..Default::default()
            }
        }

        let outbound = OpenAiResponsesOutbound::new("", "")?;
        let events = vec![
            sse(json!({
                "type": "response.created",
                "response": {"id": "resp_stream", "model": "gpt-5", "created_at": 456}
            })),
            sse(json!({"type": "response.output_text.delta", "delta": "hello"})),
            sse(json!({
                "type": "response.output_item.added",
                "item": {
                    "id": "fc_item",
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "lookup"
                }
            })),
            sse(json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_item",
                "delta": "{\"q\":\"x\"}"
            })),
            sse(json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_stream",
                    "model": "gpt-5",
                    "created_at": 456,
                    "usage": {"input_tokens": 4, "output_tokens": 2, "total_tokens": 6}
                }
            })),
        ];

        let chunks: Vec<_> =
            OutboundTransformer::transform_stream(&outbound, Box::new(events.into_iter()))?
                .collect();
        assert_eq!(chunks.len(), 6);
        assert_eq!(
            chunks[0].choices[0]
                .delta
                .as_ref()
                .and_then(|delta| delta.role.as_deref()),
            Some("assistant")
        );
        assert_eq!(
            chunks[1].choices[0]
                .delta
                .as_ref()
                .and_then(|delta| delta.content.as_ref()),
            Some(&MessageContent::Text("hello".to_string()))
        );
        let start_call = &chunks[2].choices[0]
            .delta
            .as_ref()
            .ok_or_else(|| ConduitError::internal("expected tool start delta"))?
            .tool_calls[0];
        assert_eq!(start_call.id.as_deref(), Some("call_1"));
        assert_eq!(start_call.function["name"], "lookup");
        let args_call = &chunks[3].choices[0]
            .delta
            .as_ref()
            .ok_or_else(|| ConduitError::internal("expected arguments delta"))?
            .tool_calls[0];
        assert_eq!(args_call.function["arguments"], "{\"q\":\"x\"}");
        assert_eq!(
            chunks[4].choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
        assert_eq!(
            chunks[5].usage.as_ref().map(|usage| usage.total_tokens),
            Some(6)
        );
        Ok(())
    }

    #[test]
    fn stream_refusal_round_trip_preserves_deltas_without_done_duplication() -> TransformerResult<()>
    {
        fn sse(value: Value) -> StreamEvent {
            StreamEvent {
                data: Some(value.to_string()),
                ..StreamEvent::default()
            }
        }

        let outbound = OpenAiResponsesOutbound::new("", "")?;
        let upstream_events = vec![
            sse(json!({
                "type": "response.created",
                "response": {
                    "id": "resp_refusal_stream",
                    "model": "gpt-test",
                    "created_at": 1700000001
                }
            })),
            sse(json!({
                "type": "response.refusal.delta",
                "item_id": "msg_refusal_stream",
                "output_index": 0,
                "content_index": 0,
                "delta": "I cannot"
            })),
            sse(json!({
                "type": "response.refusal.delta",
                "item_id": "msg_refusal_stream",
                "output_index": 0,
                "content_index": 0,
                "delta": " assist with that."
            })),
            sse(json!({
                "type": "response.refusal.done",
                "item_id": "msg_refusal_stream",
                "output_index": 0,
                "content_index": 0,
                "refusal": "I cannot assist with that."
            })),
            sse(json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_refusal_stream",
                    "model": "gpt-test",
                    "created_at": 1700000001,
                    "status": "completed"
                }
            })),
        ];

        let unified_chunks: Vec<_> = OutboundTransformer::transform_stream(
            &outbound,
            Box::new(upstream_events.into_iter()),
        )?
        .collect();
        assert_eq!(unified_chunks.len(), 4);

        let refusal_deltas = unified_chunks
            .iter()
            .filter_map(|chunk| chunk.choices.first())
            .filter_map(|choice| choice.delta.as_ref())
            .filter_map(|message| message.content.as_ref())
            .filter_map(|content| match content {
                MessageContent::Parts(parts) if parts.len() == 1 => Some(&parts[0]),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(refusal_deltas.len(), 2);
        assert!(
            refusal_deltas
                .iter()
                .all(|part| part.part_type == "refusal")
        );
        assert_eq!(
            refusal_deltas
                .iter()
                .filter_map(|part| part.text.as_deref())
                .collect::<String>(),
            "I cannot assist with that."
        );

        let client_events: Vec<_> = crate::openai_responses_inbound::transform_stream(
            Box::new(unified_chunks.into_iter()),
            false,
        )?
        .collect();
        let refusal_delta_events = client_events
            .iter()
            .filter(|event| event.event_type.as_deref() == Some("response.refusal.delta"))
            .collect::<Vec<_>>();
        assert_eq!(refusal_delta_events.len(), 2);
        assert_eq!(
            refusal_delta_events
                .iter()
                .filter_map(|event| event.json_data.as_ref())
                .filter_map(|value| value.get("delta"))
                .filter_map(Value::as_str)
                .collect::<String>(),
            "I cannot assist with that."
        );

        let refusal_done_events = client_events
            .iter()
            .filter(|event| event.event_type.as_deref() == Some("response.refusal.done"))
            .collect::<Vec<_>>();
        assert_eq!(refusal_done_events.len(), 1);
        assert_eq!(
            refusal_done_events[0]
                .json_data
                .as_ref()
                .and_then(|value| value.get("refusal"))
                .and_then(Value::as_str),
            Some("I cannot assist with that.")
        );

        let completed = client_events
            .iter()
            .find(|event| event.event_type.as_deref() == Some("response.completed"))
            .and_then(|event| event.json_data.as_ref())
            .ok_or_else(|| ConduitError::internal("missing response.completed event"))?;
        assert_eq!(
            completed["response"]["output"][0]["content"][0],
            json!({
                "type": "refusal",
                "refusal": "I cannot assist with that.",
            })
        );
        Ok(())
    }

    fn chat_message_defaults() -> ChatMessage {
        ChatMessage {
            role: String::new(),
            name: None,
            content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        }
    }

    fn outbound_json(request: &HttpRequest) -> TransformerResult<Value> {
        request
            .json_body
            .clone()
            .ok_or_else(|| ConduitError::internal("expected JSON body"))
    }
}
