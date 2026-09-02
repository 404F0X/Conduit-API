//! Client-facing OpenAI Responses transformations.
//!
//! The unified response model is intentionally chat-shaped.  A Responses
//! client therefore needs an explicit response leg whenever the selected
//! upstream speaks another protocol.  Keeping that conversion here avoids the
//! trait default silently serialising `choices`/`delta` as if every OpenAI
//! surface used the Chat Completions wire format.

use std::collections::{BTreeMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use conduit_core::{ConduitError, ErrorKind, openai_error_json};
use conduit_llm::{
    Annotation, ErrorDetail, LlmMessage, LlmResponse, MessageContent, ResponseError, StreamEvent,
    ToolCall, Usage,
    model::{HeaderMap, HttpResponse},
};
use serde_json::{Map, Value, json};

use crate::TransformerResult;

pub(crate) fn transform_response(
    response: LlmResponse,
    compact: bool,
) -> TransformerResult<HttpResponse> {
    let value = if compact {
        compact_response_value(&response)?
    } else {
        standard_response_value(&response)
    };
    json_http_response(200, value)
}

pub(crate) fn transform_error(error: &ConduitError) -> TransformerResult<HttpResponse> {
    json_http_response(error.http_status, openai_error_json(error))
}

pub(crate) fn transform_stream(
    events: Box<dyn Iterator<Item = LlmResponse> + Send>,
    compact: bool,
) -> TransformerResult<Box<dyn Iterator<Item = StreamEvent> + Send>> {
    if compact {
        return Err(ConduitError::invalid_request(
            "OpenAI Responses compact does not support streaming responses",
        ));
    }
    Ok(Box::new(ResponsesClientStream::new(events)))
}

fn json_http_response(status: u16, value: Value) -> TransformerResult<HttpResponse> {
    let body = serde_json::to_vec(&value).map_err(|error| {
        ConduitError::internal("failed to serialize OpenAI Responses client response")
            .with_source(error)
    })?;
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("Cache-Control".to_string(), "no-cache".to_string());
    Ok(HttpResponse {
        status,
        headers,
        body: Some(body),
        json_body: Some(value),
        ..HttpResponse::default()
    })
}

fn standard_response_value(response: &LlmResponse) -> Value {
    let status = response_status(response);
    let item_status = output_item_status(status);
    let mut output = Vec::new();

    for choice in &response.choices {
        if let Some(message) = choice.message.as_ref().or(choice.delta.as_ref()) {
            output.extend(message_output_items(message, item_status));
        }
    }

    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(response_id(response)));
    object.insert("object".to_string(), Value::String("response".to_string()));
    object.insert("created_at".to_string(), Value::from(created_at(response)));
    object.insert("status".to_string(), Value::String(status.to_string()));
    object.insert("model".to_string(), Value::String(response.model.clone()));
    object.insert("output".to_string(), Value::Array(output));

    if let Some(previous_response_id) = &response.previous_response_id {
        object.insert(
            "previous_response_id".to_string(),
            Value::String(previous_response_id.clone()),
        );
    }
    if let Some(service_tier) = &response.service_tier {
        object.insert(
            "service_tier".to_string(),
            Value::String(service_tier.clone()),
        );
    }
    if let Some(usage) = &response.usage {
        object.insert("usage".to_string(), responses_usage(usage));
    }
    if let Some(error) = &response.error {
        object.insert("error".to_string(), response_error_value(error));
    }
    if status == "incomplete" {
        object.insert(
            "incomplete_details".to_string(),
            json!({"reason": incomplete_reason(response)}),
        );
    }

    Value::Object(object)
}

fn compact_response_value(response: &LlmResponse) -> TransformerResult<Value> {
    let compact = response.compact.as_ref().and_then(Value::as_object);
    let id = compact
        .and_then(|value| value.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| response_id(response));
    let created = compact
        .and_then(|value| value.get("created_at"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| created_at(response));
    let object_name = compact
        .and_then(|value| value.get("object"))
        .and_then(Value::as_str)
        .unwrap_or("response.compaction");
    let model = compact
        .and_then(|value| value.get("model"))
        .and_then(Value::as_str)
        .unwrap_or(&response.model);

    let mut output = Vec::new();
    if let Some(items) = compact
        .and_then(|value| value.get("output"))
        .and_then(Value::as_array)
    {
        for item in items {
            if item.get("type").and_then(Value::as_str).is_some() {
                output.push(item.clone());
            } else {
                let message = compact_message(item)?;
                output.extend(message_output_items(&message, "completed"));
            }
        }
    } else {
        for choice in &response.choices {
            if let Some(message) = choice.message.as_ref().or(choice.delta.as_ref()) {
                output.extend(message_output_items(message, "completed"));
            }
        }
    }

    let mut value = Map::new();
    value.insert("id".to_string(), Value::String(id));
    value.insert("created_at".to_string(), Value::from(created));
    value.insert("object".to_string(), Value::String(object_name.to_string()));
    value.insert("model".to_string(), Value::String(model.to_string()));
    if let Some(instructions) = compact.and_then(|value| value.get("instructions")).cloned() {
        value.insert("instructions".to_string(), instructions);
    }
    value.insert("output".to_string(), Value::Array(output));
    if let Some(usage) = &response.usage {
        value.insert("usage".to_string(), responses_usage(usage));
    }
    Ok(Value::Object(value))
}

fn compact_message(value: &Value) -> TransformerResult<LlmMessage> {
    let mut message: LlmMessage = serde_json::from_value(value.clone()).map_err(|error| {
        ConduitError::new(
            ErrorKind::InvalidResponse,
            "compact response output item is not a valid message",
        )
        .with_source(error)
    })?;
    if let Some(MessageContent::Json(content)) = message.content.as_ref()
        && let Some(text) = content
            .get("content")
            .or_else(|| content.get("text"))
            .and_then(Value::as_str)
    {
        message.content = Some(MessageContent::Text(text.to_string()));
    }
    Ok(message)
}

fn message_output_items(message: &LlmMessage, status: &str) -> Vec<Value> {
    let mut output = Vec::new();

    if message.reasoning_content.is_some() || message.reasoning_signature.is_some() {
        let mut reasoning = Map::new();
        reasoning.insert("id".to_string(), Value::String(generated_id("rs")));
        reasoning.insert("type".to_string(), Value::String("reasoning".to_string()));
        reasoning.insert("status".to_string(), Value::String(status.to_string()));
        if let Some(summary) = &message.reasoning_content {
            reasoning.insert(
                "summary".to_string(),
                json!([{"type": "summary_text", "text": summary}]),
            );
        }
        if let Some(encrypted_content) = &message.reasoning_signature {
            reasoning.insert(
                "encrypted_content".to_string(),
                Value::String(encrypted_content.clone()),
            );
        }
        output.push(Value::Object(reasoning));
    }

    let content = output_content(message);
    if !content.is_empty() || message.tool_calls.is_empty() {
        output.push(json!({
            "id": message.id.clone().unwrap_or_else(|| generated_id("msg")),
            "type": "message",
            "status": status,
            "role": message.role.as_deref().unwrap_or("assistant"),
            "content": content,
        }));
    }

    for tool_call in &message.tool_calls {
        output.push(tool_output_item(tool_call, status));
    }

    output
}

fn output_content(message: &LlmMessage) -> Vec<Value> {
    let mut content = Vec::new();
    match message.content.as_ref() {
        Some(MessageContent::Text(text)) => {
            content.push(output_text(text, &message.annotations));
        }
        Some(MessageContent::Parts(parts)) => {
            for part in parts {
                if let Some(text) = &part.text {
                    if part.part_type == "refusal" {
                        content.push(json!({"type": "refusal", "refusal": text}));
                    } else {
                        content.push(output_text(text, &message.annotations));
                    }
                }
            }
        }
        Some(MessageContent::Json(value)) => {
            if let Some(text) = json_content_text(value) {
                content.push(output_text(&text, &message.annotations));
            }
        }
        None => {}
    }
    if let Some(refusal) = &message.refusal {
        content.push(json!({"type": "refusal", "refusal": refusal}));
    }
    content
}

fn output_text(text: &str, annotations: &[Annotation]) -> Value {
    let annotations = annotations.iter().map(annotation_value).collect::<Vec<_>>();
    json!({
        "type": "output_text",
        "annotations": annotations,
        "text": text,
    })
}

fn annotation_value(annotation: &Annotation) -> Value {
    serde_json::to_value(annotation).unwrap_or_else(|_| Value::Object(Map::new()))
}

fn json_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(object) => object
            .get("content")
            .or_else(|| object.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn tool_output_item(tool_call: &ToolCall, status: &str) -> Value {
    if tool_call.call_type == "responses_custom_tool"
        && let Some(custom) = tool_call.extra.get("response_custom_tool")
    {
        let mut value = custom.clone();
        if let Some(object) = value.as_object_mut() {
            object
                .entry("id".to_string())
                .or_insert_with(|| Value::String(generated_id("ct")));
            object.insert(
                "type".to_string(),
                Value::String("custom_tool_call".to_string()),
            );
            object.insert("status".to_string(), Value::String(status.to_string()));
        }
        return value;
    }

    let call_id = tool_call.id.clone().unwrap_or_else(|| generated_id("call"));
    let name = tool_call
        .function
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let arguments = tool_call
        .function
        .get("arguments")
        .map(string_value)
        .unwrap_or_default();
    json!({
        "id": generated_id("fc"),
        "type": "function_call",
        "status": status,
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    })
}

fn string_value(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn responses_usage(usage: &Usage) -> Value {
    json!({
        "input_tokens": usage.prompt_tokens,
        "input_tokens_details": {
            "cached_tokens": usage.prompt_details.cached_tokens,
        },
        "output_tokens": usage.completion_tokens,
        "output_tokens_details": {
            "reasoning_tokens": usage.completion_details.reasoning_tokens,
        },
        "total_tokens": usage.total_tokens,
    })
}

fn response_error_value(error: &ResponseError) -> Value {
    serde_json::to_value(error)
        .ok()
        .and_then(|value| value.get("error").cloned())
        .unwrap_or_else(|| json!({"message": "response failed", "type": "api_error"}))
}

fn response_status(response: &LlmResponse) -> &'static str {
    if response.error.is_some() {
        return "failed";
    }
    for choice in &response.choices {
        match choice.finish_reason.as_deref() {
            Some("length" | "content_filter") => return "incomplete",
            Some("error" | "failed") => return "failed",
            Some("cancelled" | "canceled") => return "cancelled",
            _ => {}
        }
    }
    "completed"
}

fn incomplete_reason(response: &LlmResponse) -> &'static str {
    response
        .choices
        .iter()
        .find_map(|choice| choice.finish_reason.as_deref())
        .map(|reason| match reason {
            "content_filter" => "content_filter",
            _ => "max_output_tokens",
        })
        .unwrap_or("max_output_tokens")
}

fn output_item_status(response_status: &str) -> &'static str {
    match response_status {
        "completed" => "completed",
        "failed" => "failed",
        "cancelled" => "cancelled",
        _ => "incomplete",
    }
}

fn response_id(response: &LlmResponse) -> String {
    if response.id.is_empty() {
        generated_id("resp")
    } else {
        response.id.clone()
    }
}

fn created_at(response: &LlmResponse) -> i64 {
    if response.created != 0 {
        response.created
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or(0)
    }
}

fn generated_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7().simple())
}

#[derive(Debug, Default)]
struct TextStreamPart {
    content_index: u64,
    text: String,
}

#[derive(Debug, Default)]
struct RefusalStreamPart {
    content_index: u64,
    refusal: String,
}

#[derive(Debug, Default)]
struct MessageStreamItem {
    id: String,
    output_index: u64,
    next_content_index: u64,
    text: Option<TextStreamPart>,
    refusal: Option<RefusalStreamPart>,
}

#[derive(Debug, Default)]
struct ReasoningStreamItem {
    id: String,
    output_index: u64,
    text: String,
    encrypted_content: Option<String>,
}

#[derive(Debug, Default)]
struct ToolStreamItem {
    id: String,
    call_id: String,
    output_index: u64,
    name: String,
    arguments: String,
    custom: bool,
}

struct ResponsesClientStream {
    inner: Box<dyn Iterator<Item = LlmResponse> + Send>,
    queue: VecDeque<StreamEvent>,
    response_id: String,
    model: String,
    created_at: i64,
    previous_response_id: Option<String>,
    usage: Option<Usage>,
    error: Option<ResponseError>,
    finish_reason: Option<String>,
    message: Option<MessageStreamItem>,
    reasoning: Option<ReasoningStreamItem>,
    tools: BTreeMap<i64, ToolStreamItem>,
    next_output_index: u64,
    sequence: u64,
    started: bool,
    finished: bool,
}

impl ResponsesClientStream {
    fn new(inner: Box<dyn Iterator<Item = LlmResponse> + Send>) -> Self {
        Self {
            inner,
            queue: VecDeque::new(),
            response_id: String::new(),
            model: String::new(),
            created_at: 0,
            previous_response_id: None,
            usage: None,
            error: None,
            finish_reason: None,
            message: None,
            reasoning: None,
            tools: BTreeMap::new(),
            next_output_index: 0,
            sequence: 0,
            started: false,
            finished: false,
        }
    }

    fn consume(&mut self, response: LlmResponse) {
        self.update_identity(&response);
        if !self.started {
            self.started = true;
            let snapshot = self.response_snapshot("in_progress", Vec::new());
            self.push_event("response.created", json!({"response": snapshot}));
            let snapshot = self.response_snapshot("in_progress", Vec::new());
            self.push_event("response.in_progress", json!({"response": snapshot}));
        }

        if let Some(usage) = response.usage {
            self.usage = Some(usage);
        }
        if let Some(error) = response.error {
            self.error = Some(error);
            self.finish_reason = Some("error".to_string());
        }

        for choice in response.choices {
            if let Some(message) = choice.delta.or(choice.message) {
                self.consume_message(message);
            }
            if let Some(reason) = choice.finish_reason {
                self.finish_reason = Some(reason);
            }
        }
    }

    fn update_identity(&mut self, response: &LlmResponse) {
        if self.response_id.is_empty() && !response.id.is_empty() {
            self.response_id = response.id.clone();
        }
        if self.model.is_empty() && !response.model.is_empty() {
            self.model = response.model.clone();
        }
        if self.created_at == 0 && response.created != 0 {
            self.created_at = response.created;
        }
        if response.previous_response_id.is_some() {
            self.previous_response_id = response.previous_response_id.clone();
        }
        if self.response_id.is_empty() {
            self.response_id = generated_id("resp");
        }
        if self.created_at == 0 {
            self.created_at = created_at(response);
        }
    }

    fn consume_message(&mut self, message: LlmMessage) {
        let LlmMessage {
            id,
            content,
            refusal,
            reasoning_content,
            reasoning_signature,
            tool_calls,
            ..
        } = message;
        if let Some(reasoning_delta) = reasoning_content {
            self.push_reasoning_delta(&reasoning_delta);
        }
        if let Some(signature) = reasoning_signature {
            self.ensure_reasoning();
            if let Some(reasoning) = &mut self.reasoning {
                reasoning.encrypted_content = Some(signature);
            }
        }
        if let Some(content) = content {
            for delta in content_stream_deltas(&content) {
                match delta {
                    ContentStreamDelta::OutputText(delta) => {
                        self.push_text_delta(&delta, id.as_deref());
                    }
                    ContentStreamDelta::Refusal(delta) => {
                        self.push_refusal_delta(&delta, id.as_deref());
                    }
                }
            }
        }
        if let Some(refusal) = refusal {
            self.push_refusal_delta(&refusal, id.as_deref());
        }
        for tool_call in tool_calls {
            self.push_tool_delta(tool_call);
        }
    }

    fn ensure_message(&mut self, message_id: Option<&str>) {
        if self.message.is_some() {
            return;
        }
        let item = MessageStreamItem {
            id: message_id
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| generated_id("msg")),
            output_index: self.take_output_index(),
            ..MessageStreamItem::default()
        };
        self.push_event(
            "response.output_item.added",
            json!({
                "output_index": item.output_index,
                "item": {
                    "id": item.id,
                    "type": "message",
                    "status": "in_progress",
                    "role": "assistant",
                    "content": [],
                }
            }),
        );
        self.message = Some(item);
    }

    fn ensure_text(&mut self, message_id: Option<&str>) {
        self.ensure_message(message_id);
        let Some(message) = self.message.as_mut() else {
            return;
        };
        if message.text.is_some() {
            return;
        }
        let content_index = message.next_content_index;
        message.next_content_index = message.next_content_index.saturating_add(1);
        message.text = Some(TextStreamPart {
            content_index,
            text: String::new(),
        });
        let id = message.id.clone();
        let output_index = message.output_index;
        self.push_event(
            "response.content_part.added",
            json!({
                "item_id": id,
                "output_index": output_index,
                "content_index": content_index,
                "part": {
                    "type": "output_text",
                    "text": "",
                    "annotations": [],
                    "logprobs": [],
                }
            }),
        );
    }

    fn push_text_delta(&mut self, delta: &str, message_id: Option<&str>) {
        self.ensure_text(message_id);
        let Some(message) = &mut self.message else {
            return;
        };
        let Some(text) = &mut message.text else {
            return;
        };
        text.text.push_str(delta);
        let id = message.id.clone();
        let output_index = message.output_index;
        let content_index = text.content_index;
        self.push_event(
            "response.output_text.delta",
            json!({
                "item_id": id,
                "output_index": output_index,
                "content_index": content_index,
                "delta": delta,
                "logprobs": [],
            }),
        );
    }

    fn ensure_refusal(&mut self, message_id: Option<&str>) {
        self.ensure_message(message_id);
        let Some(message) = self.message.as_mut() else {
            return;
        };
        if message.refusal.is_some() {
            return;
        }
        let content_index = message.next_content_index;
        message.next_content_index = message.next_content_index.saturating_add(1);
        message.refusal = Some(RefusalStreamPart {
            content_index,
            refusal: String::new(),
        });
        let id = message.id.clone();
        let output_index = message.output_index;
        self.push_event(
            "response.content_part.added",
            json!({
                "item_id": id,
                "output_index": output_index,
                "content_index": content_index,
                "part": {
                    "type": "refusal",
                    "refusal": "",
                }
            }),
        );
    }

    fn push_refusal_delta(&mut self, delta: &str, message_id: Option<&str>) {
        self.ensure_refusal(message_id);
        let Some(message) = &mut self.message else {
            return;
        };
        let Some(refusal) = &mut message.refusal else {
            return;
        };
        refusal.refusal.push_str(delta);
        let id = message.id.clone();
        let output_index = message.output_index;
        let content_index = refusal.content_index;
        self.push_event(
            "response.refusal.delta",
            json!({
                "item_id": id,
                "output_index": output_index,
                "content_index": content_index,
                "delta": delta,
            }),
        );
    }

    fn ensure_reasoning(&mut self) {
        if self.reasoning.is_some() {
            return;
        }
        let item = ReasoningStreamItem {
            id: generated_id("rs"),
            output_index: self.take_output_index(),
            text: String::new(),
            encrypted_content: None,
        };
        self.push_event(
            "response.output_item.added",
            json!({
                "output_index": item.output_index,
                "item": {
                    "id": item.id,
                    "type": "reasoning",
                    "status": "in_progress",
                    "summary": [],
                }
            }),
        );
        self.push_event(
            "response.reasoning_summary_part.added",
            json!({
                "item_id": item.id,
                "output_index": item.output_index,
                "summary_index": 0,
                "part": {"type": "summary_text", "text": ""},
            }),
        );
        self.reasoning = Some(item);
    }

    fn push_reasoning_delta(&mut self, delta: &str) {
        self.ensure_reasoning();
        let Some(reasoning) = &mut self.reasoning else {
            return;
        };
        reasoning.text.push_str(delta);
        let id = reasoning.id.clone();
        let output_index = reasoning.output_index;
        self.push_event(
            "response.reasoning_summary_text.delta",
            json!({
                "item_id": id,
                "output_index": output_index,
                "summary_index": 0,
                "delta": delta,
            }),
        );
    }

    fn push_tool_delta(&mut self, tool_call: ToolCall) {
        let index = tool_call
            .extra
            .get("index")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| i64::try_from(self.tools.len()).unwrap_or(i64::MAX));
        let custom = tool_call.call_type == "responses_custom_tool";
        let call_id = tool_call
            .id
            .clone()
            .or_else(|| {
                tool_call
                    .extra
                    .get("response_custom_tool")
                    .and_then(|value| value.get("call_id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| generated_id("call"));
        let name = tool_call
            .function
            .get("name")
            .or_else(|| {
                tool_call
                    .extra
                    .get("response_custom_tool")
                    .and_then(|value| value.get("name"))
            })
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let delta = tool_call
            .function
            .get("arguments")
            .or_else(|| {
                tool_call
                    .extra
                    .get("response_custom_tool")
                    .and_then(|value| value.get("input"))
            })
            .map(string_value)
            .unwrap_or_default();

        if !self.tools.contains_key(&index) {
            let item = ToolStreamItem {
                id: generated_id(if custom { "ct" } else { "fc" }),
                call_id,
                output_index: self.take_output_index(),
                name: name.clone(),
                arguments: String::new(),
                custom,
            };
            let item_value = stream_tool_value(&item, "in_progress");
            self.push_event(
                "response.output_item.added",
                json!({"output_index": item.output_index, "item": item_value}),
            );
            self.tools.insert(index, item);
        }

        let Some(item) = self.tools.get_mut(&index) else {
            return;
        };
        if item.name.is_empty() && !name.is_empty() {
            item.name = name;
        }
        item.arguments.push_str(&delta);
        let event_type = if item.custom {
            "response.custom_tool_call_input.delta"
        } else {
            "response.function_call_arguments.delta"
        };
        let id = item.id.clone();
        let output_index = item.output_index;
        self.push_event(
            event_type,
            json!({
                "item_id": id,
                "output_index": output_index,
                "delta": delta,
            }),
        );
    }

    fn finish(&mut self) {
        if self.finished || !self.started {
            self.finished = true;
            return;
        }
        self.finished = true;
        // A provider stream that delivered chunks but closed without a finish
        // reason is malformed. Closing the client stream silently leaves a
        // Responses SDK permanently without a terminal lifecycle event. Emit
        // a deterministic failure terminal instead; the live pipeline drops
        // this synthetic terminal when it has a concrete upstream read error
        // and forwards that real error frame instead.
        if self.finish_reason.is_none() && self.error.is_none() {
            self.finish_reason = Some("error".to_string());
            self.error = Some(ResponseError {
                status_code: 502,
                detail: ErrorDetail {
                    code: "upstream_stream_missing_finish_reason".to_string(),
                    message: "Upstream stream ended without a finish reason".to_string(),
                    detail_type: "invalid_response_error".to_string(),
                    ..ErrorDetail::default()
                },
            });
        }
        let status = stream_status(self.finish_reason.as_deref(), self.error.as_ref());
        let item_status = output_item_status(status);
        let mut completed = BTreeMap::<u64, Value>::new();

        if let Some(reasoning) = self.reasoning.take() {
            self.push_event(
                "response.reasoning_summary_text.done",
                json!({
                    "item_id": reasoning.id,
                    "output_index": reasoning.output_index,
                    "summary_index": 0,
                    "text": reasoning.text,
                }),
            );
            let part = json!({"type": "summary_text", "text": reasoning.text});
            self.push_event(
                "response.reasoning_summary_part.done",
                json!({
                    "item_id": reasoning.id,
                    "output_index": reasoning.output_index,
                    "summary_index": 0,
                    "part": part,
                }),
            );
            let mut item = json!({
                "id": reasoning.id,
                "type": "reasoning",
                "status": item_status,
                "summary": [part],
            });
            if let Some(encrypted_content) = reasoning.encrypted_content
                && let Some(object) = item.as_object_mut()
            {
                object.insert(
                    "encrypted_content".to_string(),
                    Value::String(encrypted_content),
                );
            }
            self.push_event(
                "response.output_item.done",
                json!({"output_index": reasoning.output_index, "item": item}),
            );
            completed.insert(reasoning.output_index, item);
        }

        if let Some(message) = self.message.take() {
            let MessageStreamItem {
                id,
                output_index,
                text,
                refusal,
                ..
            } = message;
            let mut content = BTreeMap::<u64, Value>::new();

            if let Some(text) = text {
                self.push_event(
                    "response.output_text.done",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "content_index": text.content_index,
                        "text": text.text,
                        "logprobs": [],
                    }),
                );
                let part = json!({
                    "type": "output_text",
                    "text": text.text,
                    "annotations": [],
                    "logprobs": [],
                });
                self.push_event(
                    "response.content_part.done",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "content_index": text.content_index,
                        "part": part,
                    }),
                );
                content.insert(text.content_index, part);
            }

            if let Some(refusal) = refusal {
                self.push_event(
                    "response.refusal.done",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "content_index": refusal.content_index,
                        "refusal": refusal.refusal,
                    }),
                );
                let part = json!({
                    "type": "refusal",
                    "refusal": refusal.refusal,
                });
                self.push_event(
                    "response.content_part.done",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "content_index": refusal.content_index,
                        "part": part,
                    }),
                );
                content.insert(refusal.content_index, part);
            }

            let item = json!({
                "id": id,
                "type": "message",
                "status": item_status,
                "role": "assistant",
                "content": content.into_values().collect::<Vec<_>>(),
            });
            self.push_event(
                "response.output_item.done",
                json!({"output_index": output_index, "item": item}),
            );
            completed.insert(output_index, item);
        }

        let tools = std::mem::take(&mut self.tools);
        for (_, tool) in tools {
            let done_type = if tool.custom {
                "response.custom_tool_call_input.done"
            } else {
                "response.function_call_arguments.done"
            };
            self.push_event(
                done_type,
                json!({
                    "item_id": tool.id,
                    "output_index": tool.output_index,
                    "call_id": tool.call_id,
                    "name": tool.name,
                    if tool.custom { "input" } else { "arguments" }: tool.arguments,
                }),
            );
            let item = stream_tool_value(&tool, item_status);
            self.push_event(
                "response.output_item.done",
                json!({"output_index": tool.output_index, "item": item}),
            );
            completed.insert(tool.output_index, item);
        }

        let output = completed.into_values().collect();
        let response = self.response_snapshot(status, output);
        let event_type = match status {
            "failed" => "response.failed",
            "incomplete" => "response.incomplete",
            "cancelled" => "response.cancelled",
            _ => "response.completed",
        };
        self.push_event(event_type, json!({"response": response}));
    }

    fn response_snapshot(&self, status: &str, output: Vec<Value>) -> Value {
        let mut response = Map::new();
        response.insert("id".to_string(), Value::String(self.response_id.clone()));
        response.insert("object".to_string(), Value::String("response".to_string()));
        response.insert("created_at".to_string(), Value::from(self.created_at));
        response.insert("status".to_string(), Value::String(status.to_string()));
        response.insert("model".to_string(), Value::String(self.model.clone()));
        response.insert("output".to_string(), Value::Array(output));
        if let Some(previous_response_id) = &self.previous_response_id {
            response.insert(
                "previous_response_id".to_string(),
                Value::String(previous_response_id.clone()),
            );
        }
        if let Some(usage) = &self.usage {
            response.insert("usage".to_string(), responses_usage(usage));
        }
        if let Some(error) = &self.error {
            response.insert("error".to_string(), response_error_value(error));
        }
        if status == "incomplete" {
            response.insert(
                "incomplete_details".to_string(),
                json!({"reason": incomplete_reason_from_finish(self.finish_reason.as_deref())}),
            );
        }
        Value::Object(response)
    }

    fn push_event(&mut self, event_type: &str, payload: Value) {
        let mut object = payload.as_object().cloned().unwrap_or_default();
        object.insert("type".to_string(), Value::String(event_type.to_string()));
        object.insert("sequence_number".to_string(), Value::from(self.sequence));
        self.sequence = self.sequence.saturating_add(1);
        let value = Value::Object(object);
        self.queue.push_back(StreamEvent {
            event_type: Some(event_type.to_string()),
            data: Some(value.to_string()),
            json_data: Some(value),
            ..StreamEvent::default()
        });
    }

    fn take_output_index(&mut self) -> u64 {
        let index = self.next_output_index;
        self.next_output_index = self.next_output_index.saturating_add(1);
        index
    }
}

impl Iterator for ResponsesClientStream {
    type Item = StreamEvent;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Some(event);
            }
            if self.finished {
                return None;
            }
            match self.inner.next() {
                Some(response) => self.consume(response),
                None => self.finish(),
            }
        }
    }
}

enum ContentStreamDelta {
    OutputText(String),
    Refusal(String),
}

fn content_stream_deltas(content: &MessageContent) -> Vec<ContentStreamDelta> {
    match content {
        MessageContent::Text(text) => vec![ContentStreamDelta::OutputText(text.clone())],
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| {
                part.text.clone().map(|text| {
                    if part.part_type == "refusal" {
                        ContentStreamDelta::Refusal(text)
                    } else {
                        ContentStreamDelta::OutputText(text)
                    }
                })
            })
            .collect(),
        MessageContent::Json(value) => json_content_text(value)
            .map(ContentStreamDelta::OutputText)
            .into_iter()
            .collect(),
    }
}

fn stream_tool_value(tool: &ToolStreamItem, status: &str) -> Value {
    if tool.custom {
        json!({
            "id": tool.id,
            "type": "custom_tool_call",
            "status": status,
            "call_id": tool.call_id,
            "name": tool.name,
            "input": tool.arguments,
        })
    } else {
        json!({
            "id": tool.id,
            "type": "function_call",
            "status": status,
            "call_id": tool.call_id,
            "name": tool.name,
            "arguments": tool.arguments,
        })
    }
}

fn stream_status(finish_reason: Option<&str>, error: Option<&ResponseError>) -> &'static str {
    if error.is_some() {
        return "failed";
    }
    match finish_reason {
        Some("length" | "content_filter") => "incomplete",
        Some("error" | "failed") => "failed",
        Some("cancelled" | "canceled") => "cancelled",
        _ => "completed",
    }
}

fn incomplete_reason_from_finish(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("content_filter") => "content_filter",
        _ => "max_output_tokens",
    }
}

#[cfg(test)]
mod tests {
    use conduit_llm::{Choice, ContentPart, RequestType, TokenDetails};

    use super::*;

    fn text_response() -> LlmResponse {
        LlmResponse {
            id: "resp_cross_protocol".to_string(),
            object: "chat.completion".to_string(),
            created: 1_700_000_000,
            model: "gpt-test".to_string(),
            choices: vec![Choice {
                index: 0,
                message: Some(LlmMessage {
                    id: Some("msg_cross_protocol".to_string()),
                    role: Some("assistant".to_string()),
                    content: Some(MessageContent::Text("hello".to_string())),
                    ..LlmMessage::default()
                }),
                finish_reason: Some("stop".to_string()),
                ..Choice::default()
            }],
            usage: Some(Usage {
                prompt_tokens: 3,
                completion_tokens: 2,
                total_tokens: 5,
                prompt_details: TokenDetails {
                    cached_tokens: 1,
                    ..TokenDetails::default()
                },
                completion_details: TokenDetails {
                    reasoning_tokens: 1,
                    ..TokenDetails::default()
                },
                ..Usage::default()
            }),
            ..LlmResponse::default()
        }
    }

    #[test]
    fn committed_cross_protocol_non_stream_contract() -> TransformerResult<()> {
        let contract: Value = serde_json::from_str(include_str!(
            "../../../tests/contracts/llm_cases/openai_responses/chat_cross_protocol_client_response.json"
        ))
        .map_err(|error| ConduitError::internal("invalid committed contract").with_source(error))?;
        let response = transform_response(text_response(), false)?;
        assert_eq!(response.status, 200);
        assert_eq!(
            response.json_body,
            Some(contract["client_http"]["body_json"].clone())
        );
        Ok(())
    }

    #[test]
    fn committed_compact_contract_builds_reasoning_and_message_items() -> TransformerResult<()> {
        let contract: Value = serde_json::from_str(include_str!(
            "../../../tests/contracts/llm_cases/openai_responses/compact_client_response_reasoning.json"
        ))
        .map_err(|error| ConduitError::internal("invalid committed contract").with_source(error))?;
        let mut response: LlmResponse =
            serde_json::from_value(contract["unified_response"].clone()).map_err(|error| {
                ConduitError::internal("invalid unified fixture").with_source(error)
            })?;
        response.request_type = Some(RequestType::Compact);
        let actual = transform_response(response, true)?;
        let actual = actual
            .json_body
            .ok_or_else(|| ConduitError::internal("missing compact body"))?;
        let expected = &contract["client_http"]["body_json"];
        assert_eq!(actual["id"], expected["id"]);
        assert_eq!(actual["object"], expected["object"]);
        assert_eq!(actual["output"][0]["type"], "reasoning");
        assert_eq!(actual["output"][0]["encrypted_content"], "gAAAAAB...");
        assert_eq!(actual["output"][1], expected["output"][1]);
        assert_eq!(actual["usage"], expected["usage"]);
        Ok(())
    }

    #[test]
    fn cross_protocol_stream_emits_responses_lifecycle_and_no_chat_chunks() -> TransformerResult<()>
    {
        let chunks = vec![
            LlmResponse {
                id: "resp_stream".to_string(),
                created: 42,
                model: "gpt-test".to_string(),
                choices: vec![Choice {
                    delta: Some(LlmMessage {
                        role: Some("assistant".to_string()),
                        content: Some(MessageContent::Text("hel".to_string())),
                        ..LlmMessage::default()
                    }),
                    ..Choice::default()
                }],
                ..LlmResponse::default()
            },
            LlmResponse {
                id: "resp_stream".to_string(),
                created: 42,
                model: "gpt-test".to_string(),
                choices: vec![Choice {
                    delta: Some(LlmMessage {
                        content: Some(MessageContent::Text("lo".to_string())),
                        ..LlmMessage::default()
                    }),
                    finish_reason: Some("stop".to_string()),
                    ..Choice::default()
                }],
                usage: Some(Usage {
                    prompt_tokens: 2,
                    completion_tokens: 1,
                    total_tokens: 3,
                    ..Usage::default()
                }),
                ..LlmResponse::default()
            },
        ];
        let events: Vec<_> = transform_stream(Box::new(chunks.into_iter()), false)?.collect();
        let event_types = events
            .iter()
            .filter_map(|event| event.event_type.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(event_types.first().copied(), Some("response.created"));
        assert!(event_types.contains(&"response.output_text.delta"));
        assert_eq!(event_types.last().copied(), Some("response.completed"));
        assert!(events.iter().all(|event| {
            event
                .data
                .as_deref()
                .and_then(|data| serde_json::from_str::<Value>(data).ok())
                .is_some_and(|value| value.get("choices").is_none())
        }));
        let completed: Value = serde_json::from_str(
            events
                .last()
                .and_then(|event| event.data.as_deref())
                .ok_or_else(|| ConduitError::internal("missing completed event"))?,
        )
        .map_err(|error| ConduitError::internal("invalid completed event").with_source(error))?;
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"],
            "hello"
        );
        assert_eq!(completed["response"]["usage"]["total_tokens"], 3);
        Ok(())
    }

    #[test]
    fn cross_protocol_stream_preserves_refusal_events_and_content_type() -> TransformerResult<()> {
        let chunks = vec![
            LlmResponse {
                id: "resp_refusal".to_string(),
                created: 42,
                model: "gpt-test".to_string(),
                choices: vec![Choice {
                    delta: Some(LlmMessage {
                        id: Some("msg_refusal".to_string()),
                        role: Some("assistant".to_string()),
                        content: Some(MessageContent::Parts(vec![ContentPart {
                            part_type: "refusal".to_string(),
                            text: Some("I cannot".to_string()),
                            ..ContentPart::default()
                        }])),
                        ..LlmMessage::default()
                    }),
                    ..Choice::default()
                }],
                ..LlmResponse::default()
            },
            LlmResponse {
                id: "resp_refusal".to_string(),
                created: 42,
                model: "gpt-test".to_string(),
                choices: vec![Choice {
                    delta: Some(LlmMessage {
                        refusal: Some(" assist with that".to_string()),
                        ..LlmMessage::default()
                    }),
                    finish_reason: Some("stop".to_string()),
                    ..Choice::default()
                }],
                ..LlmResponse::default()
            },
        ];

        let events: Vec<_> = transform_stream(Box::new(chunks.into_iter()), false)?.collect();
        let event_types = events
            .iter()
            .filter_map(|event| event.event_type.as_deref())
            .collect::<Vec<_>>();
        assert!(event_types.contains(&"response.refusal.delta"));
        assert!(event_types.contains(&"response.refusal.done"));
        assert!(!event_types.contains(&"response.output_text.delta"));

        let refusal_done = events
            .iter()
            .find(|event| event.event_type.as_deref() == Some("response.refusal.done"))
            .and_then(|event| event.json_data.as_ref())
            .ok_or_else(|| ConduitError::internal("missing refusal done event"))?;
        assert_eq!(refusal_done["refusal"], "I cannot assist with that");

        let completed = events
            .last()
            .and_then(|event| event.json_data.as_ref())
            .ok_or_else(|| ConduitError::internal("missing completed event"))?;
        assert_eq!(
            completed["response"]["output"][0]["content"][0],
            json!({
                "type": "refusal",
                "refusal": "I cannot assist with that",
            })
        );
        Ok(())
    }

    #[test]
    fn compact_streaming_is_rejected() {
        let result = transform_stream(Box::new(std::iter::empty()), true);
        assert!(result.is_err());
    }

    #[test]
    fn stream_without_finish_reason_emits_stable_failed_terminal() -> TransformerResult<()> {
        let chunks = vec![LlmResponse {
            id: "resp_truncated".to_string(),
            created: 42,
            model: "gpt-test".to_string(),
            choices: vec![Choice {
                delta: Some(LlmMessage {
                    content: Some(MessageContent::Text("partial".to_string())),
                    ..LlmMessage::default()
                }),
                ..Choice::default()
            }],
            ..LlmResponse::default()
        }];

        let events: Vec<_> = transform_stream(Box::new(chunks.into_iter()), false)?.collect();
        let terminal = events
            .last()
            .ok_or_else(|| ConduitError::internal("missing failed terminal"))?;
        assert_eq!(terminal.event_type.as_deref(), Some("response.failed"));
        let payload = terminal
            .json_data
            .as_ref()
            .ok_or_else(|| ConduitError::internal("missing failed terminal payload"))?;
        assert_eq!(payload["response"]["status"], "failed");
        assert_eq!(
            payload["response"]["error"]["code"],
            "upstream_stream_missing_finish_reason"
        );
        Ok(())
    }
}
