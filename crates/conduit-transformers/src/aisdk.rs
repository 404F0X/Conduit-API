//! AI SDK inbound transformer — mirrors Go package `conduit/llm/transformer/aisdk`.
//!
//! ## Scope markers
//! - **S04**: header-only factory dispatch — [`select_aisdk_format`].
//! - **S08**: bare-JSON data-stream frame parser — [`parse_datastream_frame`].
//! - **S09**: minimal text-delta frame builder — [`datastream_text_frame`].
//! - **S05**: data stream event conversion (this file) — [`StreamEvent`] wire
//!   model + [`AiSdkDataStreamConverter`] state machine that mirrors Go's
//!   `aiSDKConvertStream` (`convert_stream.go`) one LLM chunk at a time, and
//!   [`aggregate_stream_events`] mirroring `AggregateStreamChunks`
//!   (`datastream.go:52-170`).
//! - **S06**: AI-SDK ↔ unified LLM request interop (this file) — [`Request`],
//!   [`UIMessage`], [`UIMessagePart`], [`Tool`] mirroring `model.go`, plus
//!   [`convert_to_llm_request`] mirroring `convert_request.go` so AI-SDK
//!   clients can drive an OpenAI-chat-shaped downstream channel.
//!
//! ## Why pure helpers (not a `Stream` impl)
//!
//! Go wires the per-chunk transform up as a stateful `aiSDKConvertStream`
//! wrapping `streams.Stream[*llm.Response]`. The Rust transformer trait
//! (`traits::InboundTransformer`) operates one event at a time, so the state
//! machine lives in [`AiSdkDataStreamConverter`]: the caller feeds it one
//! [`LlmResponse`] chunk via [`AiSdkDataStreamConverter::convert_chunk`] and
//! receives zero or more [`StreamEvent`]s, preserving the start/delta/end
//! lifecycle Go emits.
//!
//! ## Go framing contract (load-bearing)
//!
//! 1. `enqueueEvent` (`convert_stream.go:57-69`) does ONLY `json.Marshal(data)`
//!    — no `data:` prefix, no trailing `\n`. Each `httpclient.StreamEvent.Data`
//!    is exactly the JSON encoding of one [`StreamEvent`] struct.
//! 2. `WriteJSONStream` (`aisdk.go:93`) writes that byte slice VERBATIM. The
//!    on-the-wire chunk is a single bare JSON object, NOT SSE-framed.
//! 3. The `[DONE]` marker is dropped CHUNK-LEVEL upstream at
//!    `convert_stream.go:93-95` (`if chunk.Object == "[DONE]" { return s.Next() }`),
//!    BEFORE any event is enqueued — it never reaches the converter in normal
//!    flow.
//! 4. The factory (`factory.go:18-26`) dispatches PURELY on the
//!    `X-Vercel-Ai-Ui-Message-Stream` header value being exactly `"v1"`. No
//!    content-type heuristic is in any Go factory file.

use std::collections::HashMap;

use conduit_core::ConduitError;
use conduit_llm::{
    ApiFormat, ChatMessage, ChatRequest, ContentPart, HttpResponse, LlmRequest, LlmRequestPayload,
    LlmResponse, MessageContent, RequestType, ToolCall as LlmToolCall, UnifiedTool,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ============================================================================
// S04: header-only factory dispatch (mirrors factory.go)
// ============================================================================

/// AI SDK inbound wire format selected by the factory.
///
/// Mirrors Go's `TransformerType` constants (`"text"` / `"datastream"`) plus
/// the `NewTransformer` factory in `conduit/llm/transformer/aisdk/factory.go`
/// which dispatches on the `X-Vercel-Ai-Ui-Message-Stream` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiSdkFormat {
    /// Plain-text / JSON inbound (default for backward compatibility).
    Text,
    /// Vercel AI SDK Data Stream Protocol (`X-Vercel-Ai-Ui-Message-Stream: v1`).
    DataStream,
}

/// Header-only helper that classifies the inbound format from the value of
/// the `X-Vercel-Ai-Ui-Message-Stream` request header.
///
/// Mirrors Go's `NewTransformer(headers)` / `IsDataStream(headers)` in
/// `factory.go` lines 18-30 EXACTLY: the dispatch is a single exact-match
/// comparison against the literal string `"v1"`. There is NO content-type
/// heuristic in any Go factory file. Pass the raw header value (Go's
/// `http.Header.Get` canonicalises the header name and compares the value
/// case-sensitively, so only the exact lowercase `v1` matches).
///
/// Rules:
/// 1. The exact string `"v1"` → [`AiSdkFormat::DataStream`].
/// 2. Anything else → [`AiSdkFormat::Text`], matching Go's default branch.
pub fn select_aisdk_format(header_value: &str) -> AiSdkFormat {
    // Go factory.go:20 — `headers.Get("X-Vercel-Ai-Ui-Message-Stream") == "v1"`.
    if header_value == "v1" {
        return AiSdkFormat::DataStream;
    }
    AiSdkFormat::Text
}

// ============================================================================
// S05: data stream event wire model (mirrors steam.go)
// ============================================================================

/// Wire model for one AI SDK Data Stream event. Mirrors Go's `StreamEvent`
/// struct in `steam.go` lines 9-49: a single struct whose `type` discriminator
/// selects which fields are populated. Serialized as a bare JSON object per
/// chunk (see the framing contract at the top of this file).
///
/// Field names mirror the Go json tags EXACTLY (camelCase, including the
/// `toolCallId` / `messageId` / `errorText` acronym-friendly forms). All
/// optional fields carry `skip_serializing_if = "Option::is_none"` to match
/// Go's `omitempty`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamEvent {
    /// Discriminator. Go's `Type string \`json:"type"\`` — always serialized,
    /// even when empty (no `omitempty`).
    #[serde(rename = "type")]
    pub event_type: String,
    /// `start` event: the message id.
    #[serde(rename = "messageId", default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// text-*/reasoning-* events: the block id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// text-delta / reasoning-delta: incremental content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    /// tool-input-* events: tool call id.
    #[serde(
        rename = "toolCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id: Option<String>,
    /// tool-input-start / tool-input-available: tool name.
    #[serde(rename = "toolName", default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// tool-input-delta: streaming arguments fragment.
    #[serde(
        rename = "inputTextDelta",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_text_delta: Option<String>,
    /// tool-input-available: complete parsed arguments (JSON value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// tool-output-available: tool execution result (JSON value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// error event: error text.
    #[serde(rename = "errorText", default, skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
}

impl StreamEvent {
    /// Convenience constructor for a `start` event carrying the message id.
    /// Mirrors Go `StreamEvent{Type:"start", MessageID: ...}` in
    /// `convert_stream.go:106-109`.
    pub fn start(message_id: impl Into<String>) -> Self {
        Self {
            event_type: "start".to_string(),
            message_id: Some(message_id.into()),
            ..Self::default()
        }
    }

    /// Convenience constructor for a `finish` event (no payload).
    /// Mirrors Go `StreamEvent{Type:"finish"}` in `convert_stream.go:326-328`.
    pub fn finish() -> Self {
        Self {
            event_type: "finish".to_string(),
            ..Self::default()
        }
    }

    /// Convenience constructor for an `error` event.
    /// Mirrors Go's `error` StreamEvent shape (`steam.go:39`).
    pub fn error(error_text: impl Into<String>) -> Self {
        Self {
            event_type: "error".to_string(),
            error_text: Some(error_text.into()),
            ..Self::default()
        }
    }

    /// Convenience constructor for a `start-step` event.
    /// Mirrors Go `startStep()` (`convert_stream.go:365-371`).
    pub fn start_step() -> Self {
        Self {
            event_type: "start-step".to_string(),
            ..Self::default()
        }
    }

    /// Convenience constructor for a `finish-step` event.
    /// Mirrors Go `finishStep()` (`convert_stream.go:373-379`).
    pub fn finish_step() -> Self {
        Self {
            event_type: "finish-step".to_string(),
            ..Self::default()
        }
    }

    /// Convenience constructor for a `text-start` event.
    /// Mirrors Go `startTextContent()` (`convert_stream.go:381-395`).
    pub fn text_start(id: impl Into<String>) -> Self {
        Self {
            event_type: "text-start".to_string(),
            id: Some(id.into()),
            ..Self::default()
        }
    }

    /// Convenience constructor for a `text-delta` event.
    /// Mirrors Go `convert_stream.go:187-191`.
    pub fn text_delta(id: impl Into<String>, delta: impl Into<String>) -> Self {
        Self {
            event_type: "text-delta".to_string(),
            id: Some(id.into()),
            delta: Some(delta.into()),
            ..Self::default()
        }
    }

    /// Convenience constructor for a `text-end` event.
    /// Mirrors Go `endTextContent()` (`convert_stream.go:397-418`).
    pub fn text_end(id: impl Into<String>) -> Self {
        Self {
            event_type: "text-end".to_string(),
            id: Some(id.into()),
            ..Self::default()
        }
    }

    /// Convenience constructor for a `tool-input-start` event.
    /// Mirrors Go `convert_stream.go:239-243`.
    pub fn tool_input_start(tool_call_id: impl Into<String>, tool_name: impl Into<String>) -> Self {
        Self {
            event_type: "tool-input-start".to_string(),
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            ..Self::default()
        }
    }

    /// Convenience constructor for a `tool-input-delta` event.
    /// Mirrors Go `convert_stream.go:255-259`.
    pub fn tool_input_delta(
        tool_call_id: impl Into<String>,
        input_text_delta: impl Into<String>,
    ) -> Self {
        Self {
            event_type: "tool-input-delta".to_string(),
            tool_call_id: Some(tool_call_id.into()),
            input_text_delta: Some(input_text_delta.into()),
            ..Self::default()
        }
    }

    /// Convenience constructor for a `tool-input-available` event.
    /// Mirrors Go `convert_stream.go:286-291`.
    pub fn tool_input_available(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        input: Value,
    ) -> Self {
        Self {
            event_type: "tool-input-available".to_string(),
            tool_call_id: Some(tool_call_id.into()),
            tool_name: Some(tool_name.into()),
            input: Some(input),
            ..Self::default()
        }
    }
}

// ============================================================================
// S05: stateful outbound converter (mirrors aiSDKConvertStream)
// ============================================================================

/// Counter used to make generated ids unique within one converter run.
/// Go uses `uuid.New()` (`generateID`, `convert_stream.go:477-479`); we cannot
/// pull a UUID crate without bloating the workspace deps, so a monotonic
/// counter combined with the conventional prefix is sufficient for parity
/// tests (which only assert the prefix and non-empty, never the exact uuid).
fn generate_id(prefix: &str, counter: u64) -> String {
    format!("{prefix}_{counter}")
}

/// Stateful AI SDK Data Stream converter. Mirrors Go's
/// `aiSDKConvertStream` (`convert_stream.go:35-55`) one LLM chunk at a time.
///
/// Usage:
/// 1. Construct with [`AiSdkDataStreamConverter::new`].
/// 2. Feed each upstream [`LlmResponse`] chunk via [`Self::convert_chunk`],
///    collecting the returned [`StreamEvent`]s.
/// 3. When the upstream stream ends, call [`Self::flush`] to emit any closing
///    events for still-open content blocks, followed by the terminal
///    [`StreamEvent::finish`] (Go emits `finish` on the first chunk that
///    carries a `finish_reason`; `flush` is a safety net).
///
/// The converter reproduces Go's block-lifecycle rules: only one of
/// text/reasoning/tool content is "open" at a time, and switching blocks
/// emits the matching `*-end` + `finish-step` (and `start-step` for the new
/// block). Tool calls are tracked by id so `tool-input-start` is emitted only
/// once per call, with `tool-input-delta` fragments following.
pub struct AiSdkDataStreamConverter {
    has_started: bool,
    has_finished: bool,
    message_id: String,
    id_counter: u64,
    // Content block lifecycle flags (mirrors Go's has*ContentStarted).
    has_text_content_started: bool,
    has_reasoning_content_started: bool,
    has_tool_content_started: bool,
    current_text_id: String,
    current_reasoning_id: String,
    // tool call ids already announced via tool-input-start.
    active_tool_calls: HashMap<String, ()>,
}

impl Default for AiSdkDataStreamConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl AiSdkDataStreamConverter {
    /// Create a fresh converter with no blocks open.
    pub fn new() -> Self {
        Self {
            has_started: false,
            has_finished: false,
            message_id: String::new(),
            id_counter: 0,
            has_text_content_started: false,
            has_reasoning_content_started: false,
            has_tool_content_started: false,
            current_text_id: String::new(),
            current_reasoning_id: String::new(),
            active_tool_calls: HashMap::new(),
        }
    }

    /// Allocate a unique id with the given prefix (mirrors Go `generateID`).
    fn alloc_id(&mut self, prefix: &str) -> String {
        self.id_counter += 1;
        generate_id(prefix, self.id_counter)
    }

    /// Close the text block if open: emit `text-end` + `finish-step`.
    /// Mirrors Go `endTextContent()` (`convert_stream.go:397-418`).
    fn end_text_content(&mut self, out: &mut Vec<StreamEvent>) {
        if !self.has_text_content_started {
            return;
        }
        self.has_text_content_started = false;
        out.push(StreamEvent::text_end(std::mem::take(
            &mut self.current_text_id,
        )));
        out.push(StreamEvent::finish_step());
    }

    /// Close the reasoning block if open: emit `reasoning-end` + `finish-step`.
    /// Mirrors Go `endReasoningContent()` (`convert_stream.go:439-459`).
    fn end_reasoning_content(&mut self, out: &mut Vec<StreamEvent>) {
        if !self.has_reasoning_content_started {
            return;
        }
        self.has_reasoning_content_started = false;
        let id = std::mem::take(&mut self.current_reasoning_id);
        out.push(StreamEvent {
            event_type: "reasoning-end".to_string(),
            id: Some(id),
            ..StreamEvent::default()
        });
        out.push(StreamEvent::finish_step());
    }

    /// Close the tool block if open: emit `finish-step` only (Go has no
    /// explicit tool-end event). Mirrors Go `endToolContent()`
    /// (`convert_stream.go:461-474`).
    fn end_tool_content(&mut self, out: &mut Vec<StreamEvent>) {
        if !self.has_tool_content_started {
            return;
        }
        self.has_tool_content_started = false;
        out.push(StreamEvent::finish_step());
    }

    /// Open the text block if not already open: emit `start-step` +
    /// `text-start`. Mirrors Go `startTextContent()`
    /// (`convert_stream.go:381-395`).
    fn start_text_content(&mut self, out: &mut Vec<StreamEvent>) {
        if self.has_text_content_started {
            return;
        }
        out.push(StreamEvent::start_step());
        self.has_text_content_started = true;
        let id = self.alloc_id("text");
        self.current_text_id = id.clone();
        out.push(StreamEvent::text_start(id));
    }

    /// Open the reasoning block if not already open: emit `start-step` +
    /// `reasoning-start`. Mirrors Go `startReasoningContent()`
    /// (`convert_stream.go:420-437`).
    fn start_reasoning_content(&mut self, out: &mut Vec<StreamEvent>) {
        if self.has_reasoning_content_started {
            return;
        }
        out.push(StreamEvent::start_step());
        self.has_reasoning_content_started = true;
        let id = self.alloc_id("reasoning");
        self.current_reasoning_id = id.clone();
        out.push(StreamEvent {
            event_type: "reasoning-start".to_string(),
            id: Some(id),
            ..StreamEvent::default()
        });
    }

    /// Convert one upstream [`LlmResponse`] chunk into zero or more AI SDK
    /// [`StreamEvent`]s. Mirrors Go `aiSDKConvertStream.Next()` /
    /// `Current()` (`convert_stream.go:72-338`) for the chunk-processing
    /// branches; the queue/iteration machinery is replaced by the returned
    /// `Vec`.
    ///
    /// Branches reproduced (in Go's order):
    /// 1. Skip the `[DONE]` marker chunk (defensive; Go filters it upstream).
    /// 2. Capture the message id from the first non-empty `chunk.id`.
    /// 3. Emit `start` on the very first chunk.
    /// 4. For each choice: handle reasoning delta, text delta, tool-call
    ///    deltas (start + delta), and complete tool calls
    ///    (`tool-input-available`).
    /// 5. On the first non-empty `finish_reason`, close any open blocks and
    ///    emit `finish`.
    pub fn convert_chunk(&mut self, chunk: &LlmResponse) -> Vec<StreamEvent> {
        let mut out: Vec<StreamEvent> = Vec::new();

        // (1) [DONE] sentinel — defensive; Go filters this upstream.
        if chunk.object == "[DONE]" {
            return out;
        }

        // (2) Capture message id.
        if self.message_id.is_empty() && !chunk.id.is_empty() {
            self.message_id = chunk.id.clone();
        }

        // (3) Emit start on the first chunk.
        if !self.has_started {
            self.has_started = true;
            out.push(StreamEvent::start(self.message_id.clone()));
        }

        // (4)(5) Walk the first choice only — Go always reads `chunk.Choices[0]`.
        if let Some(choice) = chunk.choices.first() {
            // Reasoning delta.
            if let Some(delta) = choice.delta.as_ref() {
                if let Some(reasoning) = delta.reasoning_content.as_ref() {
                    if !reasoning.is_empty() {
                        // Close tool then text before opening reasoning.
                        if self.has_tool_content_started {
                            self.end_tool_content(&mut out);
                        }
                        if self.has_text_content_started {
                            self.end_text_content(&mut out);
                        }
                        self.start_reasoning_content(&mut out);
                        out.push(StreamEvent {
                            event_type: "reasoning-delta".to_string(),
                            id: Some(self.current_reasoning_id.clone()),
                            delta: Some(reasoning.clone()),
                            ..StreamEvent::default()
                        });
                    }
                }
            }

            // Text delta.
            if let Some(delta) = choice.delta.as_ref() {
                if let Some(text) = text_delta_content(&delta.content) {
                    if !text.is_empty() {
                        // Close reasoning then tool before opening text.
                        if self.has_reasoning_content_started {
                            self.end_reasoning_content(&mut out);
                        }
                        if self.has_tool_content_started {
                            self.end_tool_content(&mut out);
                        }
                        self.start_text_content(&mut out);
                        out.push(StreamEvent::text_delta(
                            self.current_text_id.clone(),
                            text.to_owned(),
                        ));
                    }
                }
            }

            // Tool-call deltas (streaming start + arg fragments).
            if let Some(delta) = choice.delta.as_ref() {
                if !delta.tool_calls.is_empty() {
                    // Close text then reasoning before opening tools.
                    if self.has_text_content_started {
                        self.end_text_content(&mut out);
                    }
                    if self.has_reasoning_content_started {
                        self.end_reasoning_content(&mut out);
                    }
                    for tc in &delta.tool_calls {
                        let raw_id = tc.id.clone().unwrap_or_default();
                        let id = if raw_id.is_empty() {
                            self.alloc_id("tool")
                        } else {
                            raw_id
                        };
                        if !self.active_tool_calls.contains_key(&id) {
                            self.active_tool_calls.insert(id.clone(), ());
                            if !self.has_tool_content_started {
                                self.has_tool_content_started = true;
                            }
                            let name = function_field_string(tc, "name");
                            out.push(StreamEvent::tool_input_start(id.clone(), name));
                        }
                        let args = function_field_string(tc, "arguments");
                        if !args.is_empty() {
                            out.push(StreamEvent::tool_input_delta(id, args));
                        }
                    }
                }
            }

            // Complete tool calls (tool-input-available) from `message.tool_calls`.
            // Mirrors Go `convert_stream.go:269-296`. S10 parity: the Go
            // `StreamEvent.ToolCallID` / `ToolName` fields carry `omitempty`
            // (`steam.go:32-33`), so when `Message.ToolCalls[i].ID` (Go
            // `llm.ToolCall.ID`, `tools.go:66`) is the zero string the wire JSON
            // OMITS the field. The Rust `StreamEvent` instead uses
            // `Option<String>` with `skip_serializing_if = "Option::is_none"`,
            // which only skips `None` — `Some("")` would leak as `"toolCallId":""`
            // on the wire. To preserve Go parity (and avoid silently
            // orphaning a downstream tool-result router that keys on
            // `toolCallId`), we normalise an empty id/name back to `None`.
            if let Some(message) = choice.message.as_ref() {
                if !message.tool_calls.is_empty() {
                    for tc in &message.tool_calls {
                        let id_opt = tc.id.clone().filter(|s| !s.is_empty());
                        let name = function_field_string(tc, "name");
                        let name_opt = if name.is_empty() { None } else { Some(name) };
                        let args_str = function_field_string(tc, "arguments");
                        let input: Value = if args_str.is_empty() {
                            Value::Null
                        } else {
                            serde_json::from_str(&args_str).unwrap_or(Value::String(args_str))
                        };
                        // Build the event inline so we can carry the Option-typed
                        // id/name through verbatim (the convenience constructor
                        // `StreamEvent::tool_input_available` takes
                        // `impl Into<String>` and would re-wrap as `Some(...)`).
                        out.push(StreamEvent {
                            event_type: "tool-input-available".to_string(),
                            tool_call_id: id_opt,
                            tool_name: name_opt,
                            input: Some(input),
                            ..StreamEvent::default()
                        });
                    }
                }
            }

            // Finish reason.
            if let Some(reason) = choice.finish_reason.as_ref() {
                if !reason.is_empty() && !self.has_finished {
                    self.has_finished = true;
                    self.end_text_content(&mut out);
                    self.end_reasoning_content(&mut out);
                    self.end_tool_content(&mut out);
                    out.push(StreamEvent::finish());
                }
            }
        }

        out
    }

    /// Close any still-open content blocks and emit `finish` if not already
    /// emitted. Call once after the last chunk has been fed to
    /// [`Self::convert_chunk`]. Mirrors the safety-net behaviour of Go's
    /// terminal `finish` event when the upstream stream ends without an
    /// explicit finish reason.
    pub fn flush(&mut self) -> Vec<StreamEvent> {
        let mut out: Vec<StreamEvent> = Vec::new();
        self.end_text_content(&mut out);
        self.end_reasoning_content(&mut out);
        self.end_tool_content(&mut out);
        if !self.has_finished {
            self.has_finished = true;
            out.push(StreamEvent::finish());
        }
        out
    }
}

/// Extract the text delta from a possibly-string [`MessageContent`].
/// Mirrors Go's `*choice.Delta.Content.Content != nil && *... != ""` guard
/// (`convert_stream.go:161`). Returns `None` when the content is absent,
/// non-text, or empty.
fn text_delta_content(content: &Option<MessageContent>) -> Option<&str> {
    match content {
        Some(MessageContent::Text(s)) if !s.is_empty() => Some(s.as_str()),
        _ => None,
    }
}

/// Read a string field from a [`LlmToolCall`]'s `function` JSON object.
/// The unified model keeps `function` as a `serde_json::Value` (per S14 model
/// note), so we index by key. Returns `""` when absent (matching Go's zero
/// string).
fn function_field_string(tc: &LlmToolCall, field: &str) -> String {
    tc.function
        .get(field)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

// ============================================================================
// S05: aggregation (mirrors DataStreamTransformer.AggregateStreamChunks)
// ============================================================================

/// Aggregated output of [`aggregate_stream_events`]: the reconstructed
/// [`UIMessage`] plus the captured message id (mirrors Go's
/// `llm.ResponseMeta.ID`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AggregatedStream {
    /// The message id captured from the `start` event.
    pub message_id: String,
    /// The reconstructed assistant [`UIMessage`].
    pub message: UIMessage,
}

/// Aggregate a sequence of AI SDK data stream [`StreamEvent`]s into a single
/// assistant [`UIMessage`], mirroring Go's
/// `DataStreamTransformer.AggregateStreamChunks` (`datastream.go:52-170`).
///
/// Rules reproduced:
/// - `start` captures the `messageId`.
/// - `text-start`/`text-delta`/`text-end` accumulate one text part.
/// - `reasoning-start`/`reasoning-delta`/`reasoning-end` accumulate one
///   reasoning part.
/// - `finish-step` / `finish` / tool-input-* / unknown types are ignored at
///   the aggregation layer (matching Go).
/// - Dangling open blocks at the end of the stream are flushed (Go closes
///   them defensively, `datastream.go:154-160`).
pub fn aggregate_stream_events(events: &[StreamEvent]) -> AggregatedStream {
    let mut result = UIMessage {
        role: "assistant".to_string(),
        ..UIMessage::default()
    };
    let mut message_id = String::new();
    let mut current_text = String::new();
    let mut text_open = false;
    let mut current_reason = String::new();
    let mut reasoning_open = false;
    let mut parts: Vec<UIMessagePart> = Vec::new();

    for ev in events {
        match ev.event_type.as_str() {
            "start" => {
                if let Some(id) = ev.message_id.as_ref() {
                    message_id = id.clone();
                    result.id = Some(id.clone());
                }
            }
            "text-start" => {
                if text_open {
                    parts.push(UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some(std::mem::take(&mut current_text)),
                        ..UIMessagePart::default()
                    });
                }
                text_open = true;
            }
            "text-delta" => {
                if text_open {
                    if let Some(d) = ev.delta.as_ref() {
                        current_text.push_str(d);
                    }
                }
            }
            "text-end" => {
                if text_open {
                    parts.push(UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some(std::mem::take(&mut current_text)),
                        ..UIMessagePart::default()
                    });
                    text_open = false;
                }
            }
            "reasoning-start" => {
                if reasoning_open {
                    parts.push(UIMessagePart {
                        part_type: "reasoning".to_string(),
                        text: Some(std::mem::take(&mut current_reason)),
                        ..UIMessagePart::default()
                    });
                }
                reasoning_open = true;
            }
            "reasoning-delta" => {
                if reasoning_open {
                    if let Some(d) = ev.delta.as_ref() {
                        current_reason.push_str(d);
                    }
                }
            }
            "reasoning-end" => {
                if reasoning_open {
                    parts.push(UIMessagePart {
                        part_type: "reasoning".to_string(),
                        text: Some(std::mem::take(&mut current_reason)),
                        ..UIMessagePart::default()
                    });
                    reasoning_open = false;
                }
            }
            // Go explicitly ignores these in aggregation.
            "finish-step"
            | "finish"
            | "start-step"
            | "tool-input-start"
            | "tool-input-delta"
            | "tool-input-available" => {}
            _ => {}
        }
    }
    // Flush dangling blocks (Go: datastream.go:154-160).
    if text_open {
        parts.push(UIMessagePart {
            part_type: "text".to_string(),
            text: Some(current_text),
            ..UIMessagePart::default()
        });
    }
    if reasoning_open {
        parts.push(UIMessagePart {
            part_type: "reasoning".to_string(),
            text: Some(current_reason),
            ..UIMessagePart::default()
        });
    }
    result.parts = parts;
    AggregatedStream {
        message_id,
        message: result,
    }
}

/// Aggregate a sequence of AI SDK data stream gateway events into a single
/// non-streaming HTTP response body, mirroring Go's
/// `DataStreamTransformer.AggregateStreamChunks` (`datastream.go:52-170`).
///
/// This is the inbound-aggregator entry point that wraps
/// [`aggregate_stream_events`] (Mencius-the-4th's pure fold) with the
/// HTTP-layer envelope an [`crate::traits::InboundTransformer::aggregate_stream_chunks`]
/// implementation would produce. Pipeline `AutoAggregate` arm calls this when
/// a non-streaming caller hits an AI-SDK Data Stream provider that only
/// streams (Go `autoAggregateStream`, `non_streaming.go:110`).
///
/// Each gateway-level [`conduit_llm::StreamEvent`] carries a JSON-encoded
/// AI-SDK data-stream frame on its `data` field (mirroring Go's
/// `enqueueEvent` which marshals the typed event to JSON bytes); this helper
/// decodes each frame back into the typed [`StreamEvent`] wire model before
/// folding. Malformed frames are silently skipped, matching Go's
/// `continue` on `json.Unmarshal` error.
///
/// Returns a fully-formed [`HttpResponse`] carrying the reconstructed
/// assistant [`UIMessage`] JSON on `body`, with `Content-Type: application/json`
/// + `Cache-Control: no-cache` headers (matching the Go non-streaming envelope
/// at `non_streaming.go:122-125`). Empty input is rejected with the Go-shaped
/// `"empty stream chunks"` error so the pipeline surfaces a 400-equivalent.
///
/// The original events are preserved on `stream` for downstream retry/debug
/// code. There is currently no Rust `InboundTransformer` impl for AI-SDK (the
/// inbound dispatch is a header-only factory); this helper is exposed so a
/// future inbound struct can wire it directly via
/// `aggregate_aisdk_stream_chunks(events)`.
pub fn aggregate_aisdk_stream_chunks(
    events: Vec<conduit_llm::StreamEvent>,
) -> crate::TransformerResult<HttpResponse> {
    use conduit_core::ConduitError;
    if events.is_empty() {
        return Err(ConduitError::invalid_request("empty stream chunks"));
    }
    // Decode each gateway-level event's `data` payload into the typed wire
    // model. Malformed frames are dropped (Go `continue`).
    let mut typed: Vec<StreamEvent> = Vec::with_capacity(events.len());
    for event in &events {
        let Some(data) = event.data.as_deref() else {
            continue;
        };
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        match serde_json::from_str::<StreamEvent>(data) {
            Ok(parsed) => typed.push(parsed),
            Err(_) => continue,
        }
    }
    let aggregated = aggregate_stream_events(&typed);
    let body = serde_json::to_vec(&aggregated.message).map_err(|err| {
        ConduitError::internal("failed to marshal aggregated AI-SDK UIMessage").with_source(err)
    })?;
    if body.is_empty() {
        return Err(ConduitError::internal(
            "aggregated AI-SDK UIMessage body is empty",
        ));
    }
    let mut headers = conduit_llm::model::HeaderMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("Cache-Control".to_string(), "no-cache".to_string());
    Ok(HttpResponse {
        status: 200,
        headers,
        body: Some(body),
        stream: events,
        ..HttpResponse::default()
    })
}

// ============================================================================
// S06: AI-SDK request model (mirrors model.go)
// ============================================================================

/// Mirrors Go `aisdk.Request` (`model.go:8-16`). The inbound AI-SDK chat
/// request shape. Note `max_tokens` carries a Go snake_case json tag (NOT
/// camelCase); the rest of the struct is camelCase per Go json tags.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    #[serde(default)]
    pub messages: Vec<UIMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    // Go tag is `max_tokens` (snake_case) — explicit rename to match exactly.
    #[serde(
        rename = "max_tokens",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub max_tokens: Option<i64>,
}

/// Mirrors Go `aisdk.UIMessage` (`model.go:19-25`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Role of the message. Go json tag is `role` (required, no omitempty).
    #[serde(default)]
    pub role: String,
    /// Go `Content any \`json:"content"\`` — can be string or array. Kept as
    /// `Value` to mirror Go's `any` permissiveness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    /// Go `Parts []UIMessagePart \`json:"parts"\`` — no omitempty, but an
    /// empty array is fine.
    #[serde(default)]
    pub parts: Vec<UIMessagePart>,
}

/// Mirrors Go `aisdk.Tool` (`model.go:28-31`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// Go tag is `type` (lowercase, no omitempty).
    #[serde(rename = "type", default)]
    pub tool_type: String,
    #[serde(default)]
    pub function: Function,
}

/// Mirrors Go `aisdk.Function` (`model.go:34-38`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Function {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Go `Parameters any \`json:"parameters,omitempty"\``. Kept as `Value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

/// Mirrors Go `aisdk.Usage` (`model.go:42-44`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i64>,
}

/// Mirrors Go `aisdk.UIMessagePart` (`model.go:46-124`). A unified part
/// struct whose `type` discriminator selects which fields are populated. All
/// optional fields carry `skip_serializing_if` to match Go's `omitempty`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UIMessagePart {
    /// Go `Type string \`json:"type"\`` — required, no omitempty.
    /// NOTE: renamed to `part_type` here to avoid the Rust keyword; the wire
    /// tag is restored with `#[serde(rename = "type")]`.
    #[serde(rename = "type", default)]
    pub part_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// data-* part id. (Note: distinct from message-level `id`.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(
        rename = "toolCallId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id: Option<String>,
    #[serde(rename = "toolName", default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(
        rename = "inputTextDelta",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_text_delta: Option<String>,
    /// Go `Input any` — permissive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// Go `Output any` — permissive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_executed: Option<bool>,
    #[serde(
        rename = "callProviderMetadata",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub call_provider_metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preliminary: Option<bool>,
}

// ============================================================================
// S06: AI-SDK request -> unified LLM request (mirrors convert_request.go)
// ============================================================================

/// Options for [`convert_to_llm_request`]. Mirrors Go
/// `ConvertToLLMRequestOptions` (`convert_request.go:14-18`).
#[derive(Debug, Clone, Default)]
pub struct ConvertToLlmRequestOptions {
    /// When true, filter out tool calls that are still streaming
    /// (`input-streaming` / `input-available` state). Mirrors Go
    /// `IgnoreIncompleteToolCalls`.
    pub ignore_incomplete_tool_calls: bool,
}

/// Convert an AI-SDK [`Request`] into the unified [`LlmRequest`], mirroring
/// Go's `convertToLLMRequestWithAPIFormat` (`convert_request.go:30-402`).
///
/// Sets `request_type = Chat` and `api_format = datastream` (callers may pass
/// a different format — e.g. `text` — via [`Self::with_api_format`]).
///
/// Reproduced behaviour (in Go order):
/// 1. Base request: model, stream=true, temperature, max_tokens.
/// 2. Prepend a system message from the top-level `system` field if present.
/// 3. Per message role:
///    - `system`: aggregate text parts / content string.
///    - `user`: text + file (image_url) parts, or content string.
///    - `assistant`: step-start-separated blocks; text/reasoning/file parts +
///      tool calls + tool result messages. Honours `output-available`,
///      `output-error` (raw_input fallback), `dynamic-tool`.
///    - any other role → [`ConduitError::invalid_request`].
/// 4. Tools mapping (name/description/parameters).
///
/// The unified Rust model uses [`ChatMessage`] / [`ChatRequest`], so the
/// output is wrapped in [`LlmRequestPayload::Chat`]. Note: the unified
/// `ChatMessage.content` is `Option<MessageContent>`; we map the Go "single
/// text" path to [`MessageContent::Text`] and the multi-part path to
/// [`MessageContent::Parts`].
pub fn convert_to_llm_request(req: &Request) -> Result<LlmRequest, ConduitError> {
    convert_to_llm_request_with_format(req, ApiFormat::AiSdkDatastream)
}

/// Like [`convert_to_llm_request`] but lets the caller pick the api_format
/// tag (e.g. [`ApiFormat::AiSdkText`] for the text transformer).
pub fn convert_to_llm_request_with_format(
    req: &Request,
    api_format: ApiFormat,
) -> Result<LlmRequest, ConduitError> {
    convert_inner(req, &ConvertToLlmRequestOptions::default(), api_format)
}

/// Like [`convert_to_llm_request_with_format`] but with explicit options.
pub fn convert_to_llm_request_with_options(
    req: &Request,
    options: &ConvertToLlmRequestOptions,
    api_format: ApiFormat,
) -> Result<LlmRequest, ConduitError> {
    convert_inner(req, options, api_format)
}

fn convert_inner(
    req: &Request,
    options: &ConvertToLlmRequestOptions,
    api_format: ApiFormat,
) -> Result<LlmRequest, ConduitError> {
    let mut messages: Vec<ChatMessage> = Vec::new();

    // (2) Prepend system message from the top-level field.
    if let Some(system) = req.system.as_ref() {
        if !system.is_empty() {
            messages.push(text_chat_message("system", system));
        }
    }

    // Optionally filter incomplete tool calls (Go: convert_request.go:132-152).
    let messages_iter: Vec<UIMessage> = if options.ignore_incomplete_tool_calls {
        req.messages
            .iter()
            .map(|msg| UIMessage {
                parts: filter_incomplete_parts(&msg.parts),
                ..msg.clone()
            })
            .collect()
    } else {
        req.messages.clone()
    };

    for msg in &messages_iter {
        let role = msg.role.to_ascii_lowercase();
        match role.as_str() {
            "system" => {
                let content_text = system_message_text(msg);
                if !content_text.is_empty() {
                    messages.push(text_chat_message("system", &content_text));
                }
            }
            "user" => {
                let (parts, content_text) = user_message_parts(msg);
                if !parts.is_empty() {
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        name: None,
                        content: Some(MessageContent::Parts(parts)),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        extra: Default::default(),
                    });
                } else if !content_text.is_empty() {
                    messages.push(text_chat_message("user", &content_text));
                }
            }
            "assistant" => {
                process_assistant_message(msg, &mut messages)?;
            }
            other => {
                return Err(ConduitError::invalid_request(format!(
                    "unsupported role: {other}"
                )));
            }
        }
    }

    // (4) Tools mapping.
    let tools = map_tools(&req.tools);

    Ok(LlmRequest {
        request_type: RequestType::Chat,
        api_format,
        model: req.model.clone(),
        stream: true,
        payload: LlmRequestPayload::Chat(ChatRequest {
            messages,
            tools,
            temperature: req.temperature,
            max_tokens: req.max_tokens.map(|v| v as u32),
            ..ChatRequest::default()
        }),
        extra_body: Default::default(),
        extra_headers: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    })
}

/// Mirror Go's `IgnoreIncompleteToolCalls` filter (`convert_request.go:141-149`):
/// drop `dynamic-tool` and `tool-*` parts whose state is `input-streaming` or
/// `input-available`.
fn filter_incomplete_parts(parts: &[UIMessagePart]) -> Vec<UIMessagePart> {
    parts
        .iter()
        .filter(|p| {
            let is_tool = p.part_type == "dynamic-tool" || p.part_type.starts_with("tool-");
            if !is_tool {
                return true;
            }
            let state = p.state.as_deref().unwrap_or("");
            state != "input-streaming" && state != "input-available"
        })
        .cloned()
        .collect()
}

/// Aggregate system message text from parts or fall back to the content
/// string. Mirrors Go `convert_request.go:159-195`.
fn system_message_text(msg: &UIMessage) -> String {
    if !msg.parts.is_empty() {
        let mut sb = String::new();
        for p in &msg.parts {
            if p.part_type == "text" {
                if let Some(t) = p.text.as_ref() {
                    if !t.is_empty() {
                        sb.push_str(t);
                    }
                }
            }
        }
        sb
    } else {
        msg.content
            .as_ref()
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .unwrap_or_default()
    }
}

/// Build user-message content parts / fallback string. Mirrors Go
/// `convert_request.go:197-231`. Returns `(parts, content_text)`; the caller
/// prefers parts when non-empty.
fn user_message_parts(msg: &UIMessage) -> (Vec<ContentPart>, String) {
    let mut parts: Vec<ContentPart> = Vec::new();
    if !msg.parts.is_empty() {
        for p in &msg.parts {
            match p.part_type.as_str() {
                "text" => {
                    if let Some(t) = p.text.as_ref() {
                        if !t.is_empty() {
                            parts.push(text_content_part(t));
                        }
                    }
                }
                "file" => {
                    if let Some(cp) = file_part_to_content(p) {
                        parts.push(cp);
                    }
                }
                _ => {}
            }
        }
    }
    let content_text = msg
        .content
        .as_ref()
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_default();
    (parts, content_text)
}

/// Map a `file` part to an `image_url` content part when the media type is an
/// image. Mirrors Go `toContentPartFromFile` (`convert_request.go:79-92`).
fn file_part_to_content(p: &UIMessagePart) -> Option<ContentPart> {
    let url = p.url.as_ref().filter(|s| !s.is_empty())?;
    let media = p.media_type.as_deref().unwrap_or("");
    if !media.to_ascii_lowercase().starts_with("image/") {
        return None;
    }
    Some(ContentPart {
        part_type: "image_url".to_string(),
        text: None,
        image_url: Some(json!({"url": url})),
        input_audio: None,
        extra: Default::default(),
    })
}

/// Process an assistant message, splitting it into step-start-separated
/// blocks. Mirrors Go `convert_request.go:233-371`.
///
/// For each block: collect text/reasoning/file content parts + tool calls +
/// tool result messages, then push the assistant message followed by any tool
/// messages.
fn process_assistant_message(
    msg: &UIMessage,
    out: &mut Vec<ChatMessage>,
) -> Result<(), ConduitError> {
    // Simple text-only assistant message (no parts).
    if msg.parts.is_empty() {
        if let Some(s) = msg.content.as_ref().and_then(|c| c.as_str()) {
            if !s.is_empty() {
                out.push(text_chat_message("assistant", s));
            }
        }
        return Ok(());
    }

    let mut block: Vec<&UIMessagePart> = Vec::new();
    for p in &msg.parts {
        if p.part_type == "step-start" {
            process_assistant_block(&block, out);
            block.clear();
        } else {
            block.push(p);
        }
    }
    process_assistant_block(&block, out);
    Ok(())
}

/// Process one assistant block: build content parts + tool calls + tool
/// result messages. Mirrors Go `processBlock` closure
/// (`convert_request.go:249-359`).
fn process_assistant_block(block: &[&UIMessagePart], out: &mut Vec<ChatMessage>) {
    let mut content_parts: Vec<ContentPart> = Vec::new();
    let mut tool_calls: Vec<LlmToolCall> = Vec::new();
    let mut tool_messages: Vec<ChatMessage> = Vec::new();

    for p in block {
        let is_tool_part = p.part_type == "dynamic-tool" || p.part_type.starts_with("tool-");
        if is_tool_part {
            // Skip input-streaming tool calls.
            let state = p.state.as_deref().unwrap_or("");
            if state == "input-streaming" {
                continue;
            }
            let tool_name = tool_name_of(p);
            // Pick args: output-error prefers input then raw_input; else input.
            let args_value = if state == "output-error" {
                p.input
                    .clone()
                    .or_else(|| p.raw_input.clone())
                    .unwrap_or(Value::Null)
            } else {
                p.input.clone().unwrap_or(Value::Null)
            };
            let arg_str = raw_to_string(&args_value);
            let arg_str = if arg_str.is_empty() {
                "{}".to_string()
            } else {
                arg_str
            };
            let id = p.tool_call_id.clone().unwrap_or_default();
            tool_calls.push(LlmToolCall {
                id: Some(id.clone()),
                call_type: "function".to_string(),
                function: json!({"name": tool_name, "arguments": arg_str}),
                extra: Default::default(),
            });

            // Tool result message for output-available / output-error.
            if state == "output-available" || state == "output-error" {
                let provider_executed = p.provider_executed == Some(true);
                // Go emits the tool message when NOT provider-executed, or
                // when provider-executed but still has output-* state.
                let include =
                    !provider_executed || state == "output-available" || state == "output-error";
                if include {
                    let output_text = if state == "output-error" {
                        p.error_text.clone().unwrap_or_default()
                    } else {
                        raw_to_string(&p.output.clone().unwrap_or(Value::Null))
                    };
                    if !output_text.is_empty() {
                        tool_messages.push(ChatMessage {
                            role: "tool".to_string(),
                            name: None,
                            tool_call_id: Some(id),
                            content: Some(MessageContent::Text(output_text)),
                            tool_calls: Vec::new(),
                            extra: Default::default(),
                        });
                    }
                }
            }
            continue;
        }
        match p.part_type.as_str() {
            "text" => {
                if let Some(t) = p.text.as_ref() {
                    if !t.is_empty() {
                        content_parts.push(text_content_part(t));
                    }
                }
            }
            "file" => {
                if let Some(cp) = file_part_to_content(p) {
                    content_parts.push(cp);
                }
            }
            "reasoning" => {
                if let Some(t) = p.text.as_ref() {
                    if !t.is_empty() {
                        // Go maps reasoning to a text content part for now.
                        content_parts.push(text_content_part(t));
                    }
                }
            }
            _ => {}
        }
    }

    if !content_parts.is_empty() || !tool_calls.is_empty() {
        out.push(ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: Some(MessageContent::Parts(content_parts)),
            tool_calls,
            tool_call_id: None,
            extra: Default::default(),
        });
    }
    if !tool_messages.is_empty() {
        out.extend(tool_messages);
    }
}

/// Build a `ChatMessage` with a single text content. The unified `ChatMessage`
/// does not derive `Default`, so this helper centralises the field list.
fn text_chat_message(role: &str, text: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        name: None,
        content: Some(MessageContent::Text(text.to_string())),
        tool_calls: Vec::new(),
        tool_call_id: None,
        extra: Default::default(),
    }
}

/// Build a text `ContentPart`. The unified `ContentPart` does not derive
/// `Default`, so this helper centralises the field list.
fn text_content_part(text: &str) -> ContentPart {
    ContentPart {
        part_type: "text".to_string(),
        text: Some(text.to_string()),
        image_url: None,
        input_audio: None,
        extra: Default::default(),
    }
}

/// Determine the tool name from a part: prefer the explicit `toolName`,
/// otherwise strip the `tool-` prefix from the part type. Mirrors Go
/// `getToolName` (`convert_request.go:118-129`).
fn tool_name_of(p: &UIMessagePart) -> String {
    if let Some(name) = p.tool_name.as_ref() {
        if !name.is_empty() {
            return name.clone();
        }
    }
    if let Some(rest) = p.part_type.strip_prefix("tool-") {
        return rest.to_string();
    }
    String::new()
}

/// Compact a JSON value to its string form, mirroring Go `rawToString`
/// (`convert_request.go:95-116`): a JSON string unwraps to its value; any
/// other JSON re-marshals; null/empty → "".
fn raw_to_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Map AI-SDK tools to unified tools. Mirrors Go
/// `convert_request.go:379-399`.
fn map_tools(tools: &[Tool]) -> Vec<UnifiedTool> {
    tools
        .iter()
        .map(|t| UnifiedTool {
            tool_type: t.tool_type.clone(),
            name: Some(t.function.name.clone()),
            description: t.function.description.clone(),
            parameters: t.function.parameters.clone(),
            extra: Default::default(),
        })
        .collect()
}

// ============================================================================
// S08: bare-JSON frame parser (kept from the prior skeleton)
// ============================================================================

/// A typed view of a single AI SDK Data Stream frame.
///
/// This is the simplified Rust-side enum the S07/S08 task scoped. It covers
/// only the four frame kinds the original task names (`Text`, `ToolCall`,
/// `Finish`, `Error`). The Go side emits many more event types
/// (`text-start`/`text-delta`/`text-end`, `reasoning-*`, `tool-input-*`,
/// `start-step`/`finish-step`, `start`/`finish`); reasoning and usage are
/// intentionally funnelled into [`DataStreamFrame::Other`] so the data is
/// preserved losslessly. Prefer [`StreamEvent`] (the full wire model) for new
/// code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum DataStreamFrame {
    /// A text content frame. Maps to Go `text-start` / `text-delta` /
    /// `text-end` events.
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        text: String,
    },
    /// A tool-call frame. Maps to Go `tool-input-start` /
    /// `tool-input-delta` / `tool-input-available`.
    ToolCall {
        #[serde(
            rename = "toolCallId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        tool_call_id: Option<String>,
        #[serde(rename = "toolName", default, skip_serializing_if = "Option::is_none")]
        tool_name: Option<String>,
        input: String,
    },
    /// A finish frame. Maps to Go's `finish` event.
    Finish,
    /// An error frame. Maps to Go's `error` event (`errorText`).
    Error {
        #[serde(rename = "errorText", default, skip_serializing_if = "Option::is_none")]
        error_text: Option<String>,
    },
    /// Any other / unhandled event type — preserves the raw `type` string and
    /// the original JSON so downstream code can re-serialize it losslessly.
    Other { kind: String, raw: Value },
}

/// Parse one chunk of AI SDK Data Stream wire text into typed frames.
///
/// Mirrors the Go wire format produced by `enqueueEvent`
/// (`convert_stream.go:57-69`) and written verbatim by `WriteJSONStream`:
/// each `httpclient.StreamEvent.Data` is exactly one bare JSON object — the
/// result of `json.Marshal(StreamEvent{...})` with NO `data:` SSE prefix and
/// NO trailing `\n` separator.
///
/// Behaviour:
/// - Trims surrounding ASCII whitespace (transport buffering).
/// - Defensive `[DONE]` handling (Go filters this chunk-level upstream).
/// - Decodes the trimmed chunk as a single JSON object and classifies it into
///   a [`DataStreamFrame`].
/// - Malformed JSON is dropped leniently (empty vec), mirroring Go's tests.
pub fn parse_datastream_frame(chunk: &str) -> Vec<DataStreamFrame> {
    let trimmed = chunk.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed == "[DONE]" {
        return Vec::new();
    }
    let value = match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    match classify_value(&value) {
        Some(frame) => vec![frame],
        None => Vec::new(),
    }
}

/// Build a [`DataStreamFrame::Text`] carrying the given text delta.
///
/// Minimal `to_llm` helper (S09). The id is left as `None`; the Go side
/// auto-generates ids via `generateID("text")` but that requires UUID state.
pub fn datastream_text_frame(text: impl Into<String>) -> DataStreamFrame {
    DataStreamFrame::Text {
        id: None,
        text: text.into(),
    }
}

/// Map a decoded JSON object to a [`DataStreamFrame`] based on its `type`.
fn classify_value(value: &Value) -> Option<DataStreamFrame> {
    let obj = value.as_object()?;
    let kind = obj.get("type").and_then(|v| v.as_str())?;
    let frame = match kind {
        "text-start" | "text-delta" | "text-end" => DataStreamFrame::Text {
            id: obj.get("id").and_then(|v| v.as_str()).map(|s| s.to_owned()),
            text: obj
                .get("delta")
                .and_then(|v| v.as_str())
                .or_else(|| obj.get("text").and_then(|v| v.as_str()))
                .unwrap_or_default()
                .to_owned(),
        },
        "tool-input-start" | "tool-input-delta" | "tool-input-available" => {
            DataStreamFrame::ToolCall {
                tool_call_id: obj
                    .get("toolCallId")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_owned()),
                tool_name: obj
                    .get("toolName")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_owned()),
                input: if let Some(delta) = obj.get("inputTextDelta").and_then(|v| v.as_str()) {
                    delta.to_owned()
                } else if let Some(input) = obj.get("input") {
                    match input {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    }
                } else {
                    String::new()
                },
            }
        }
        "finish" => DataStreamFrame::Finish,
        "error" => DataStreamFrame::Error {
            error_text: obj
                .get("errorText")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned()),
        },
        other => DataStreamFrame::Other {
            kind: other.to_owned(),
            raw: value.clone(),
        },
    };
    Some(frame)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- S04: select_aisdk_format (mirrors factory_test.go) ------------

    #[test]
    fn select_format_data_stream_header_v1_is_datastream() -> Result<(), Box<dyn std::error::Error>>
    {
        assert_eq!(select_aisdk_format("v1"), AiSdkFormat::DataStream);
        Ok(())
    }

    #[test]
    fn select_format_data_stream_header_other_value_is_text()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(select_aisdk_format("v2"), AiSdkFormat::Text);
        assert_eq!(select_aisdk_format("V1"), AiSdkFormat::Text);
        Ok(())
    }

    #[test]
    fn select_format_no_header_or_other_headers_is_text() -> Result<(), Box<dyn std::error::Error>>
    {
        assert_eq!(select_aisdk_format(""), AiSdkFormat::Text);
        assert_eq!(select_aisdk_format("text/plain"), AiSdkFormat::Text);
        assert_eq!(select_aisdk_format("application/json"), AiSdkFormat::Text);
        assert_eq!(
            select_aisdk_format("application/x-ndjson"),
            AiSdkFormat::Text
        );
        assert_eq!(
            select_aisdk_format("application/x-vercel-ai-data-stream"),
            AiSdkFormat::Text
        );
        assert_eq!(select_aisdk_format("text/x-unknown"), AiSdkFormat::Text);
        Ok(())
    }

    // ---- S05: StreamEvent wire parity (mirrors datastream_test.go:182-280) ----

    #[test]
    fn stream_event_start_serialises_with_message_id() -> Result<(), serde_json::Error> {
        let ev = StreamEvent::start("gen-123");
        let v = serde_json::to_value(&ev)?;
        assert_eq!(v["type"], "start");
        assert_eq!(v["messageId"], "gen-123");
        // No other fields should be emitted (omitempty).
        assert!(v.get("id").is_none());
        assert!(v.get("delta").is_none());
        Ok(())
    }

    #[test]
    fn stream_event_text_delta_round_trip() -> Result<(), serde_json::Error> {
        let ev = StreamEvent::text_delta("text_1", "Hello");
        let v = serde_json::to_value(&ev)?;
        // Mirrors datastream_test.go "text delta part".
        assert_eq!(v["type"], "text-delta");
        assert_eq!(v["id"], "text_1");
        assert_eq!(v["delta"], "Hello");
        let back: StreamEvent = serde_json::from_value(v)?;
        assert_eq!(back, ev);
        Ok(())
    }

    #[test]
    fn stream_event_tool_input_start_round_trip() -> Result<(), serde_json::Error> {
        let ev = StreamEvent::tool_input_start("call_123", "get_weather");
        let v = serde_json::to_value(&ev)?;
        // Mirrors datastream_test.go "tool input start part".
        assert_eq!(v["type"], "tool-input-start");
        assert_eq!(v["toolCallId"], "call_123");
        assert_eq!(v["toolName"], "get_weather");
        Ok(())
    }

    #[test]
    fn stream_event_tool_input_delta_round_trip() -> Result<(), serde_json::Error> {
        let ev = StreamEvent::tool_input_delta("call_123", "San Francisco");
        let v = serde_json::to_value(&ev)?;
        assert_eq!(v["type"], "tool-input-delta");
        assert_eq!(v["toolCallId"], "call_123");
        assert_eq!(v["inputTextDelta"], "San Francisco");
        Ok(())
    }

    #[test]
    fn stream_event_tool_input_available_round_trip() -> Result<(), serde_json::Error> {
        let ev = StreamEvent::tool_input_available(
            "call_123",
            "get_weather",
            json!({"location": "San Francisco"}),
        );
        let v = serde_json::to_value(&ev)?;
        // Mirrors datastream_test.go "tool input available part".
        assert_eq!(v["type"], "tool-input-available");
        assert_eq!(v["toolCallId"], "call_123");
        assert_eq!(v["toolName"], "get_weather");
        assert_eq!(v["input"]["location"], "San Francisco");
        Ok(())
    }

    #[test]
    fn stream_event_finish_step_serialises_type_only() -> Result<(), serde_json::Error> {
        let v = serde_json::to_value(&StreamEvent::finish_step())?;
        assert_eq!(v["type"], "finish-step");
        // No optional fields.
        assert!(v.as_object().map(|o| o.len() == 1).unwrap_or(false));
        Ok(())
    }

    #[test]
    fn stream_event_finish_serialises_type_only() -> Result<(), serde_json::Error> {
        let v = serde_json::to_value(&StreamEvent::finish())?;
        assert_eq!(v["type"], "finish");
        assert!(v.as_object().map(|o| o.len() == 1).unwrap_or(false));
        Ok(())
    }

    #[test]
    fn stream_event_error_round_trip() -> Result<(), serde_json::Error> {
        let ev = StreamEvent::error("Something went wrong");
        let v = serde_json::to_value(&ev)?;
        // Mirrors datastream_test.go "error part".
        assert_eq!(v["type"], "error");
        assert_eq!(v["errorText"], "Something went wrong");
        Ok(())
    }

    // ---- S05: converter lifecycle (mirrors convert_stream.go branches) ----

    /// Build a streaming LlmResponse chunk via JSON. `LlmResponse` / `Choice`
    /// / `ToolCall` are `#[non_exhaustive]` (or lack `Default`) in
    /// `conduit-llm`, so the tests deserialise a JSON shape instead of using
    /// struct literals. The wire shape mirrors an OpenAI chat.completion.chunk.
    fn chunk_from_json(value: serde_json::Value) -> LlmResponse {
        serde_json::from_value(value).unwrap_or_else(|err| panic!("invalid chunk JSON: {err}"))
    }

    /// Build a text-delta chunk.
    fn delta_chunk(id: &str, content: Option<&str>, finish: Option<&str>) -> LlmResponse {
        let delta = match content {
            Some(c) => json!({"content": c}),
            None => json!({}),
        };
        let mut choice = json!({
            "index": 0,
            "delta": delta,
        });
        if let Some(f) = finish {
            choice["finish_reason"] = json!(f);
        }
        chunk_from_json(json!({
            "id": id,
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [choice],
        }))
    }

    #[test]
    fn converter_emits_start_then_text_lifecycle() -> Result<(), Box<dyn std::error::Error>> {
        let mut conv = AiSdkDataStreamConverter::new();
        // First chunk: start + start-step + text-start + text-delta.
        let evs = conv.convert_chunk(&delta_chunk("gen-1", Some("Hi"), None));
        assert_eq!(evs[0].event_type, "start");
        assert_eq!(evs[0].message_id.as_deref(), Some("gen-1"));
        assert_eq!(evs[1].event_type, "start-step");
        assert_eq!(evs[2].event_type, "text-start");
        assert!(evs[2].id.as_deref().unwrap_or("").starts_with("text_"));
        assert_eq!(evs[3].event_type, "text-delta");
        assert_eq!(evs[3].delta.as_deref(), Some("Hi"));
        // The text id must be consistent across start/delta.
        assert_eq!(evs[2].id, evs[3].id);

        // Second chunk: just text-delta (block already open).
        let evs2 = conv.convert_chunk(&delta_chunk("gen-1", Some(" there"), None));
        assert_eq!(evs2.len(), 1);
        assert_eq!(evs2[0].event_type, "text-delta");
        assert_eq!(evs2[0].id, evs[3].id);

        // Finish chunk: text-end + finish-step + finish.
        let evs3 = conv.convert_chunk(&delta_chunk("gen-1", None, Some("stop")));
        assert_eq!(evs3[0].event_type, "text-end");
        assert_eq!(evs3[1].event_type, "finish-step");
        assert_eq!(evs3[2].event_type, "finish");
        Ok(())
    }

    #[test]
    fn converter_tool_call_emits_start_then_delta() -> Result<(), Box<dyn std::error::Error>> {
        let mut conv = AiSdkDataStreamConverter::new();
        let chunk = chunk_from_json(json!({
            "id": "gen-2",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{\"q\":"}
                    }]
                }
            }]
        }));
        let evs = conv.convert_chunk(&chunk);
        // start, then tool-input-start (lookup), then tool-input-delta.
        let kinds: Vec<&str> = evs.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(kinds, vec!["start", "tool-input-start", "tool-input-delta"]);
        assert_eq!(evs[1].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(evs[1].tool_name.as_deref(), Some("lookup"));
        assert_eq!(evs[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(evs[2].input_text_delta.as_deref(), Some("{\"q\":"));
        Ok(())
    }

    #[test]
    fn converter_tool_input_available_from_message() -> Result<(), Box<dyn std::error::Error>> {
        let mut conv = AiSdkDataStreamConverter::new();
        let chunk = chunk_from_json(json!({
            "id": "gen-3",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{
                "index": 0,
                "message": {
                    "tool_calls": [{
                        "id": "call_9",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"sf\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }));
        let evs = conv.convert_chunk(&chunk);
        // start, tool-input-available, finish.
        let kinds: Vec<&str> = evs.iter().map(|e| e.event_type.as_str()).collect();
        assert!(kinds.contains(&"tool-input-available"));
        let avail = evs.iter().find(|e| e.event_type == "tool-input-available");
        let avail = avail.ok_or("missing tool-input-available")?;
        assert_eq!(avail.tool_call_id.as_deref(), Some("call_9"));
        assert_eq!(avail.tool_name.as_deref(), Some("get_weather"));
        assert_eq!(avail.input, Some(json!({"city": "sf"})));
        // Invocation id preserved (mirrors the S10 note).
        assert!(kinds.contains(&"finish"));
        Ok(())
    }

    // ---- S10 regression: tool invocation id preserved across the full
    // multi-shard lifecycle. Mirrors Go
    // `TestConvertStreamTransformer_TransformStream_ToolCalls`
    // (`conduit/llm/transformer/aisdk/convert_stream_test.go:273-421`): four
    // chunks feed one tool call whose id `tool_call_123` MUST appear on every
    // emitted `tool-input-*` event (start → delta → available), and the
    // accumulated argument fragments must recombine into the original JSON.
    // Pinned because losing the id mid-stream would orphan downstream
    // tool-result routing (the Go gateway keys results by `toolCallId`).
    #[test]
    fn converter_preserves_tool_invocation_id_across_multi_shard_lifecycle()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut conv = AiSdkDataStreamConverter::new();

        // Chunk 1 — delta.tool_calls: id + name + first argument fragment
        // (Go convert_stream.go:216-247).
        let chunk1 = chunk_from_json(json!({
            "id": "msg_789",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "id": "tool_call_123",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"location\":"}
                    }]
                }
            }]
        }));
        let evs1 = conv.convert_chunk(&chunk1);
        // start, tool-input-start, tool-input-delta.
        let kinds1: Vec<&str> = evs1.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(
            kinds1,
            vec!["start", "tool-input-start", "tool-input-delta"]
        );
        // The invocation id is preserved on BOTH tool-input events.
        let start_ev = evs1
            .iter()
            .find(|e| e.event_type == "tool-input-start")
            .ok_or("missing tool-input-start")?;
        assert_eq!(start_ev.tool_call_id.as_deref(), Some("tool_call_123"));
        assert_eq!(start_ev.tool_name.as_deref(), Some("get_weather"));
        let delta_ev = evs1
            .iter()
            .find(|e| e.event_type == "tool-input-delta")
            .ok_or("missing tool-input-delta")?;
        assert_eq!(delta_ev.tool_call_id.as_deref(), Some("tool_call_123"));
        assert_eq!(delta_ev.input_text_delta.as_deref(), Some("{\"location\":"));

        // Chunk 2 — same id, no name (continuation); only an argument fragment.
        // The converter MUST NOT emit a second tool-input-start for the same id
        // (Go activeToolCalls map guards this — convert_stream.go:223-248).
        let chunk2 = chunk_from_json(json!({
            "id": "msg_789",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "id": "tool_call_123",
                        "type": "function",
                        "function": {"arguments": "\"San Francisco\"}"}
                    }]
                }
            }]
        }));
        let evs2 = conv.convert_chunk(&chunk2);
        // Only the delta event — no second tool-input-start.
        assert_eq!(evs2.len(), 1, "expected one delta event, got {evs2:?}");
        assert_eq!(evs2[0].event_type, "tool-input-delta");
        assert_eq!(evs2[0].tool_call_id.as_deref(), Some("tool_call_123"));
        assert_eq!(
            evs2[0].input_text_delta.as_deref(),
            Some("\"San Francisco\"}")
        );

        // Chunk 3 — message.tool_calls carries the COMPLETE call (Go
        // convert_stream.go:269-296 emits tool-input-available). The id MUST be
        // preserved verbatim, and the parsed input JSON must reconstitute from
        // the complete arguments string.
        let chunk3 = chunk_from_json(json!({
            "id": "msg_789",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{
                "index": 0,
                "message": {
                    "tool_calls": [{
                        "id": "tool_call_123",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"San Francisco\"}"
                        }
                    }]
                }
            }]
        }));
        let evs3 = conv.convert_chunk(&chunk3);
        let avail = evs3
            .iter()
            .find(|e| e.event_type == "tool-input-available")
            .ok_or("missing tool-input-available")?;
        // Invocation id preserved on the available event.
        assert_eq!(avail.tool_call_id.as_deref(), Some("tool_call_123"));
        assert_eq!(avail.tool_name.as_deref(), Some("get_weather"));
        // Input JSON parsed from the complete arguments string.
        assert_eq!(avail.input, Some(json!({"location": "San Francisco"})));

        // Chunk 4 — finish_reason closes the lifecycle.
        let chunk4 = chunk_from_json(json!({
            "id": "msg_789",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{"index": 0, "finish_reason": "tool_calls"}]
        }));
        let evs4 = conv.convert_chunk(&chunk4);
        let kinds4: Vec<&str> = evs4.iter().map(|e| e.event_type.as_str()).collect();
        assert!(
            kinds4.contains(&"finish"),
            "expected finish event, got {kinds4:?}"
        );
        Ok(())
    }

    // ---- S10 additional parity tests: tool invocation id preservation ----
    //
    // Go contract: `conduit/llm/transformer/aisdk/convert_stream_test.go:273-421`
    // (`TestConvertStreamTransformer_TransformStream_ToolCalls`) is the only
    // Go tool-call stream test. The id literal `tool_call_123` comes from that
    // file (line 287); the secondary ids below (`tool_call_456`, `tool_call_789`)
    // follow the same convention so no Go contract is synthesised.

    /// Parallel tool calls in ONE chunk: each call must emit its own
    /// `tool-input-start` with its own id, in arrival order, and the converter
    /// must NOT collapse them or reuse the first id. Go emits one
    /// `tool-input-start` per distinct id (`convert_stream.go:216-248`).
    #[test]
    fn converter_parallel_tool_calls_in_one_chunk_keep_distinct_ids()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut conv = AiSdkDataStreamConverter::new();
        let chunk = chunk_from_json(json!({
            "id": "msg_parallel",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "id": "tool_call_123",
                            "type": "function",
                            "function": {"name": "get_weather", "arguments": "{\"q\":\"sf\"}"}
                        },
                        {
                            "id": "tool_call_456",
                            "type": "function",
                            "function": {"name": "get_time", "arguments": "{\"tz\":\"pst\"}"}
                        }
                    ]
                }
            }]
        }));
        let evs = conv.convert_chunk(&chunk);
        // start, tool-input-start(123), tool-input-delta(123),
        // tool-input-start(456), tool-input-delta(456).
        assert_eq!(evs.len(), 5, "expected 5 events, got {evs:?}");
        assert_eq!(evs[0].event_type, "start");
        assert_eq!(evs[1].event_type, "tool-input-start");
        assert_eq!(evs[1].tool_call_id.as_deref(), Some("tool_call_123"));
        assert_eq!(evs[1].tool_name.as_deref(), Some("get_weather"));
        assert_eq!(evs[2].event_type, "tool-input-delta");
        assert_eq!(evs[2].tool_call_id.as_deref(), Some("tool_call_123"));
        assert_eq!(evs[2].input_text_delta.as_deref(), Some("{\"q\":\"sf\"}"));
        assert_eq!(evs[3].event_type, "tool-input-start");
        assert_eq!(evs[3].tool_call_id.as_deref(), Some("tool_call_456"));
        assert_eq!(evs[3].tool_name.as_deref(), Some("get_time"));
        assert_eq!(evs[4].event_type, "tool-input-delta");
        assert_eq!(evs[4].tool_call_id.as_deref(), Some("tool_call_456"));
        assert_eq!(evs[4].input_text_delta.as_deref(), Some("{\"tz\":\"pst\"}"));
        Ok(())
    }

    /// Cross-chunk interleaved continuation: subsequent delta fragments for
    /// `tool_call_123` MUST NOT bleed into `tool_call_456`, and the converter
    /// MUST NOT re-emit `tool-input-start` for an id it has already announced
    /// (Go `activeToolCalls` map guard, `convert_stream.go:223-248`). Each id
    /// stays bound to its own accumulated argument fragments.
    #[test]
    fn converter_cross_chunk_delta_keeps_each_id_bound_to_own_args()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut conv = AiSdkDataStreamConverter::new();

        // Chunk A: announce both tool calls with their first arg fragments.
        let chunk_a = chunk_from_json(json!({
            "id": "msg_x",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "id": "tool_call_123",
                            "type": "function",
                            "function": {"name": "get_weather", "arguments": "{\"loc\":"}
                        },
                        {
                            "id": "tool_call_456",
                            "type": "function",
                            "function": {"name": "get_time", "arguments": "{\"tz\":"}
                        }
                    ]
                }
            }]
        }));
        let evs_a = conv.convert_chunk(&chunk_a);
        // start, start(123), delta(123), start(456), delta(456).
        assert_eq!(evs_a.len(), 5);
        let starts: Vec<&str> = evs_a
            .iter()
            .filter(|e| e.event_type == "tool-input-start")
            .map(|e| e.tool_call_id.as_deref().unwrap_or(""))
            .collect();
        assert_eq!(starts, vec!["tool_call_123", "tool_call_456"]);

        // Chunk B: continuation fragments ONLY (no `name`). Must route to the
        // correct id without re-announcing either call.
        let chunk_b = chunk_from_json(json!({
            "id": "msg_x",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [
                        {
                            "id": "tool_call_123",
                            "type": "function",
                            "function": {"arguments": "\"sf\"}"}
                        },
                        {
                            "id": "tool_call_456",
                            "type": "function",
                            "function": {"arguments": "\"pst\"}"}
                        }
                    ]
                }
            }]
        }));
        let evs_b = conv.convert_chunk(&chunk_b);
        // Two deltas only — NO tool-input-start (activeToolCalls guard).
        assert_eq!(
            evs_b.len(),
            2,
            "expected 2 deltas with no re-announce, got {evs_b:?}"
        );
        assert_eq!(evs_b[0].event_type, "tool-input-delta");
        assert_eq!(evs_b[0].tool_call_id.as_deref(), Some("tool_call_123"));
        assert_eq!(evs_b[0].input_text_delta.as_deref(), Some("\"sf\"}"));
        assert_eq!(evs_b[1].event_type, "tool-input-delta");
        assert_eq!(evs_b[1].tool_call_id.as_deref(), Some("tool_call_456"));
        assert_eq!(evs_b[1].input_text_delta.as_deref(), Some("\"pst\"}"));

        // Chunk C: message.tool_calls carries the COMPLETE calls. Each must
        // surface in its own tool-input-available event with its own id and
        // the recombined input JSON.
        let chunk_c = chunk_from_json(json!({
            "id": "msg_x",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{
                "index": 0,
                "message": {
                    "tool_calls": [
                        {
                            "id": "tool_call_123",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"loc\":\"sf\"}"
                            }
                        },
                        {
                            "id": "tool_call_456",
                            "type": "function",
                            "function": {
                                "name": "get_time",
                                "arguments": "{\"tz\":\"pst\"}"
                            }
                        }
                    ]
                }
            }]
        }));
        let evs_c = conv.convert_chunk(&chunk_c);
        let available: Vec<&StreamEvent> = evs_c
            .iter()
            .filter(|e| e.event_type == "tool-input-available")
            .collect();
        assert_eq!(available.len(), 2, "expected 2 available events");
        assert_eq!(available[0].tool_call_id.as_deref(), Some("tool_call_123"));
        assert_eq!(available[0].tool_name.as_deref(), Some("get_weather"));
        assert_eq!(available[0].input, Some(json!({"loc": "sf"})));
        assert_eq!(available[1].tool_call_id.as_deref(), Some("tool_call_456"));
        assert_eq!(available[1].tool_name.as_deref(), Some("get_time"));
        assert_eq!(available[1].input, Some(json!({"tz": "pst"})));
        Ok(())
    }

    /// S10 parity regression: Go's `StreamEvent.ToolCallID` field is
    /// `omitempty` (`steam.go:32`), so when `Message.ToolCalls[i].ID` is the
    /// zero string (`llm.ToolCall.ID`, `tools.go:66`), the wire JSON OMITS the
    /// field. The Rust `StreamEvent` uses `Option<String>` with
    /// `skip_serializing_if = "Option::is_none"`, which only skips `None` —
    /// `Some("")` would leak `"toolCallId":""`. This test pins the
    /// normalisation so the Rust wire output matches Go byte-for-byte.
    #[test]
    fn converter_tool_input_available_omits_empty_id_like_go_omitempty()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut conv = AiSdkDataStreamConverter::new();
        let chunk = chunk_from_json(json!({
            "id": "msg_omit",
            "object": "chat.completion.chunk",
            "model": "test",
            "choices": [{
                "index": 0,
                "message": {
                    "tool_calls": [
                        {
                            // No `id` field → Rust decodes `Option<String>` as
                            // `None`; Go decodes `string` as `""`. Both must
                            // produce a wire event with no `toolCallId`.
                            "type": "function",
                            "function": {
                                "name": "ping",
                                "arguments": "{}"
                            }
                        }
                    ]
                }
            }]
        }));
        let evs = conv.convert_chunk(&chunk);
        let avail = evs
            .iter()
            .find(|e| e.event_type == "tool-input-available")
            .ok_or("missing tool-input-available")?;
        // Option is None (matching Go omitempty) — never Some("").
        assert!(
            avail.tool_call_id.is_none(),
            "expected tool_call_id to be None (Go omitempty), got {:?}",
            avail.tool_call_id
        );
        // Wire serialisation: the `toolCallId` key MUST be absent.
        let wire = serde_json::to_value(avail)?;
        assert!(
            wire.get("toolCallId").is_none(),
            "expected no toolCallId field on wire, got {wire}"
        );
        assert_eq!(
            wire.get("type").and_then(Value::as_str),
            Some("tool-input-available")
        );
        assert_eq!(wire.get("toolName").and_then(Value::as_str), Some("ping"));
        assert_eq!(wire.get("input"), Some(&json!({})));
        Ok(())
    }

    #[test]
    fn converter_done_object_is_skipped() {
        let mut conv = AiSdkDataStreamConverter::new();
        let done = chunk_from_json(json!({
            "id": "",
            "object": "[DONE]",
            "model": "",
            "choices": []
        }));
        assert!(conv.convert_chunk(&done).is_empty());
    }

    #[test]
    fn converter_flush_emits_finish_if_not_already() {
        let mut conv = AiSdkDataStreamConverter::new();
        let evs = conv.flush();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event_type, "finish");
    }

    // ---- S05: aggregation (mirrors AggregateStreamChunks + datastream_test golden) ----

    #[test]
    fn aggregate_text_deltas_into_one_text_part() {
        let events = vec![
            StreamEvent::start("gen-1754577344-bfGaoVZhBY3iT78Psu02"),
            StreamEvent::start_step(),
            StreamEvent::text_start("text_1"),
            StreamEvent::text_delta("text_1", "Sure"),
            StreamEvent::text_delta("text_1", "!"),
            StreamEvent::text_end("text_1"),
            StreamEvent::finish_step(),
            StreamEvent::finish(),
        ];
        let agg = aggregate_stream_events(&events);
        assert_eq!(agg.message_id, "gen-1754577344-bfGaoVZhBY3iT78Psu02");
        assert_eq!(agg.message.role, "assistant");
        assert_eq!(
            agg.message.id.as_deref(),
            Some("gen-1754577344-bfGaoVZhBY3iT78Psu02")
        );
        assert_eq!(agg.message.parts.len(), 1);
        assert_eq!(agg.message.parts[0].part_type, "text");
        assert_eq!(agg.message.parts[0].text.as_deref(), Some("Sure!"));
    }

    #[test]
    fn aggregate_reasoning_deltas_into_one_reasoning_part() {
        let events = vec![
            StreamEvent {
                event_type: "reasoning-start".to_string(),
                id: Some("r_1".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                event_type: "reasoning-delta".to_string(),
                id: Some("r_1".to_string()),
                delta: Some("think".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                event_type: "reasoning-end".to_string(),
                id: Some("r_1".to_string()),
                ..StreamEvent::default()
            },
        ];
        let agg = aggregate_stream_events(&events);
        assert_eq!(agg.message.parts.len(), 1);
        assert_eq!(agg.message.parts[0].part_type, "reasoning");
        assert_eq!(agg.message.parts[0].text.as_deref(), Some("think"));
    }

    #[test]
    fn aggregate_dangling_text_block_is_flushed() {
        // Mirrors datastream.go:154-160 defensive flush.
        let events = vec![
            StreamEvent::text_start("text_1"),
            StreamEvent::text_delta("text_1", "no end"),
        ];
        let agg = aggregate_stream_events(&events);
        assert_eq!(agg.message.parts.len(), 1);
        assert_eq!(agg.message.parts[0].text.as_deref(), Some("no end"));
    }

    // ---- S06: convert_to_llm_request (mirrors convert_request_test.go) ----

    /// Build a Request with one user text message (the simplest golden case).
    fn user_text_request(text: &str) -> Request {
        Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "user".to_string(),
                parts: vec![UIMessagePart {
                    part_type: "text".to_string(),
                    text: Some(text.to_string()),
                    ..UIMessagePart::default()
                }],
                ..UIMessage::default()
            }],
            ..Request::default()
        }
    }

    #[test]
    fn convert_user_text_message_to_chat() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors convert_request_test.go:46 / TestConvertToLLMRequest_UserMessage_TextAndFile
        // (text-only branch).
        let req = user_text_request("Hello, AI!");
        let llm = convert_to_llm_request(&req)?;
        assert_eq!(llm.request_type, RequestType::Chat);
        assert_eq!(llm.api_format, ApiFormat::AiSdkDatastream);
        assert_eq!(llm.model.as_deref(), Some("gpt-4"));
        assert!(llm.stream);
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            other => return Err(format!("expected Chat, got {other:?}").into()),
        };
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "user");
        match &chat.messages[0].content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].part_type, "text");
                assert_eq!(parts[0].text.as_deref(), Some("Hello, AI!"));
            }
            other => return Err(format!("expected Parts, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn convert_user_content_string_fallback() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors convert_request_test.go:78-82.
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "user".to_string(),
                content: Some(json!("Hello")),
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 1);
        match &chat.messages[0].content {
            Some(MessageContent::Text(s)) => assert_eq!(s, "Hello"),
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn convert_system_message_concatenates_text_parts() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors TestConvertToLLMRequest_SystemMessage multiple text parts.
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "system".to_string(),
                parts: vec![
                    UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("Part 1".to_string()),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some(" Part 2".to_string()),
                        ..UIMessagePart::default()
                    },
                ],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 1);
        match &chat.messages[0].content {
            Some(MessageContent::Text(s)) => assert_eq!(s, "Part 1 Part 2"),
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn convert_top_level_system_field_prepended() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go convert_request.go:42-50 — top-level `system` field
        // becomes a leading system message.
        let req = Request {
            model: Some("gpt-4".to_string()),
            system: Some("You are helpful.".to_string()),
            messages: vec![UIMessage {
                role: "user".to_string(),
                content: Some(json!("Hi")),
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[0].role, "system");
        match &chat.messages[0].content {
            Some(MessageContent::Text(s)) => assert_eq!(s, "You are helpful."),
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn convert_assistant_text_message() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors TestConvertToLLMRequest_Assistant_SimpleText.
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                content: Some(json!("Hello, human!")),
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "assistant");
        match &chat.messages[0].content {
            Some(MessageContent::Text(s)) => assert_eq!(s, "Hello, human!"),
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn convert_assistant_with_tool_output_available() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors TestConvertToLLMRequest_Assistant_ToolCall_InputAvailable_ResultAvailable.
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                parts: vec![
                    UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("Let me calculate that".to_string()),
                        state: Some("done".to_string()),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "tool-screenshot".to_string(),
                        state: Some("output-available".to_string()),
                        tool_call_id: Some("call-1".to_string()),
                        input: Some(json!({"value": "value-1"})),
                        output: Some(json!("result-1")),
                        ..UIMessagePart::default()
                    },
                ],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        // Expect: assistant (text + tool call) + tool result.
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(chat.messages[0].role, "assistant");
        assert_eq!(chat.messages[0].tool_calls.len(), 1);
        let tc = &chat.messages[0].tool_calls[0];
        assert_eq!(tc.id.as_deref(), Some("call-1"));
        assert_eq!(tc.call_type, "function");
        assert_eq!(tc.function["name"], "screenshot");
        // Arguments are JSON-equal (key order may vary).
        let args: Value =
            serde_json::from_str(tc.function["arguments"].as_str().ok_or("no args")?)?;
        assert_eq!(args, json!({"value": "value-1"}));

        assert_eq!(chat.messages[1].role, "tool");
        assert_eq!(chat.messages[1].tool_call_id.as_deref(), Some("call-1"));
        match &chat.messages[1].content {
            Some(MessageContent::Text(s)) => assert_eq!(s, "result-1"),
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn convert_assistant_tool_output_error_uses_raw_input() -> Result<(), Box<dyn std::error::Error>>
    {
        // Mirrors TestConvertToLLMRequest_Assistant_ToolCall_OutputError_UsesRawInput.
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                parts: vec![
                    UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("Let me calculate that".to_string()),
                        state: Some("done".to_string()),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "tool-calculator".to_string(),
                        state: Some("output-error".to_string()),
                        tool_call_id: Some("call-err".to_string()),
                        error_text: Some("Error: Invalid input".to_string()),
                        raw_input: Some(json!({"operation": "add", "numbers": [1, 2]})),
                        ..UIMessagePart::default()
                    },
                ],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 2);
        let tc = &chat.messages[0].tool_calls[0];
        assert_eq!(tc.function["name"], "calculator");
        let args: Value =
            serde_json::from_str(tc.function["arguments"].as_str().ok_or("no args")?)?;
        assert_eq!(args, json!({"operation": "add", "numbers": [1, 2]}));
        // Tool result message carries the error text.
        match &chat.messages[1].content {
            Some(MessageContent::Text(s)) => assert_eq!(s, "Error: Invalid input"),
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn convert_assistant_step_blocks_split_into_multiple_messages()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors TestConvertToLLMRequest_Assistant_StepBlocks_Multiple.
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                parts: vec![
                    UIMessagePart {
                        part_type: "step-start".to_string(),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("response".to_string()),
                        state: Some("done".to_string()),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "tool-screenshot".to_string(),
                        state: Some("output-available".to_string()),
                        tool_call_id: Some("call-1".to_string()),
                        input: Some(json!({"value": "value-1"})),
                        output: Some(json!("result-1")),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "step-start".to_string(),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "tool-screenshot".to_string(),
                        state: Some("output-available".to_string()),
                        tool_call_id: Some("call-2".to_string()),
                        input: Some(json!({"value": "value-2"})),
                        output: Some(json!("result-2")),
                        ..UIMessagePart::default()
                    },
                ],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        // Expect: block1 assistant + tool, block2 assistant + tool.
        assert_eq!(chat.messages.len(), 4);
        assert_eq!(chat.messages[0].role, "assistant");
        assert_eq!(chat.messages[1].role, "tool");
        assert_eq!(chat.messages[2].role, "assistant");
        assert_eq!(chat.messages[3].role, "tool");
        Ok(())
    }

    #[test]
    fn convert_dynamic_tool_uses_tool_name() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors TestConvertToLLMRequestComprehensive_ToolCalls "dynamic tool".
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                parts: vec![
                    UIMessagePart {
                        part_type: "step-start".to_string(),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "dynamic-tool".to_string(),
                        tool_name: Some("custom-calculator".to_string()),
                        state: Some("output-available".to_string()),
                        tool_call_id: Some("call1".to_string()),
                        input: Some(json!({"value": 42})),
                        output: Some(json!("result")),
                        ..UIMessagePart::default()
                    },
                ],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        let tc = &chat.messages[0].tool_calls[0];
        assert_eq!(tc.id.as_deref(), Some("call1"));
        assert_eq!(tc.function["name"], "custom-calculator");
        Ok(())
    }

    #[test]
    fn convert_tools_mapping() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors TestConvertToLLMRequest_ToolsMapping.
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "user".to_string(),
                content: Some(json!("Hi")),
                ..UIMessage::default()
            }],
            tools: vec![Tool {
                tool_type: "function".to_string(),
                function: Function {
                    name: "get_weather".to_string(),
                    description: Some("Get current weather".to_string()),
                    parameters: Some(json!({
                        "type": "object",
                        "properties": {"location": {"type": "string"}},
                        "required": ["location"]
                    })),
                },
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.tools.len(), 1);
        assert_eq!(chat.tools[0].tool_type, "function");
        assert_eq!(chat.tools[0].name.as_deref(), Some("get_weather"));
        assert_eq!(
            chat.tools[0].description.as_deref(),
            Some("Get current weather")
        );
        assert!(chat.tools[0].parameters.is_some());
        Ok(())
    }

    #[test]
    fn convert_unsupported_role_errors() {
        // Mirrors TestConvertToLLMRequest_UnsupportedRole_Error.
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "unknown".to_string(),
                parts: vec![UIMessagePart {
                    part_type: "text".to_string(),
                    text: Some("msg".to_string()),
                    ..UIMessagePart::default()
                }],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        match convert_to_llm_request(&req) {
            Err(err) => assert!(err.message.contains("unsupported role: unknown")),
            Ok(_) => panic!("expected error for unsupported role"),
        }
    }

    #[test]
    fn convert_ignore_incomplete_tool_calls_filters_input_streaming()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors TestConvertToLLMRequestComprehensive_IgnoreIncompleteToolCalls.
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                parts: vec![
                    UIMessagePart {
                        part_type: "step-start".to_string(),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "tool-screenshot".to_string(),
                        state: Some("output-available".to_string()),
                        tool_call_id: Some("call-1".to_string()),
                        input: Some(json!({"value": "value-1"})),
                        output: Some(json!("result-1")),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "step-start".to_string(),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "tool-screenshot".to_string(),
                        state: Some("input-streaming".to_string()),
                        tool_call_id: Some("call-2".to_string()),
                        input: Some(json!({"value": "value-2"})),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "dynamic-tool".to_string(),
                        tool_name: Some("t2".to_string()),
                        state: Some("input-available".to_string()),
                        tool_call_id: Some("call-3".to_string()),
                        input: Some(json!({"value": "value-3"})),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("response".to_string()),
                        state: Some("done".to_string()),
                        ..UIMessagePart::default()
                    },
                ],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request_with_options(
            &req,
            &ConvertToLlmRequestOptions {
                ignore_incomplete_tool_calls: true,
            },
            ApiFormat::AiSdkDatastream,
        )?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        // The completed tool call must survive; the streaming ones must be gone.
        let completed = chat
            .messages
            .iter()
            .flat_map(|m| m.tool_calls.iter())
            .filter(|tc| tc.id.as_deref() == Some("call-1"))
            .count();
        assert_eq!(completed, 1);
        let streaming = chat
            .messages
            .iter()
            .flat_map(|m| m.tool_calls.iter())
            .filter(|tc| matches!(tc.id.as_deref(), Some("call-2") | Some("call-3")))
            .count();
        assert_eq!(streaming, 0);
        Ok(())
    }

    #[test]
    fn convert_user_file_part_becomes_image_url() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors convert_request_test.go file-part branch.
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "user".to_string(),
                parts: vec![
                    UIMessagePart {
                        part_type: "file".to_string(),
                        media_type: Some("image/jpeg".to_string()),
                        url: Some("https://example.com/image.jpg".to_string()),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("Check this image".to_string()),
                        ..UIMessagePart::default()
                    },
                ],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        match &chat.messages[0].content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0].part_type, "image_url");
                assert_eq!(
                    parts[0].image_url.as_ref().and_then(|v| v.get("url")),
                    Some(&json!("https://example.com/image.jpg"))
                );
                assert_eq!(parts[1].part_type, "text");
            }
            other => return Err(format!("expected Parts, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn convert_with_text_api_format_tag() -> Result<(), Box<dyn std::error::Error>> {
        // The text transformer passes AiSdkText; verify it's threaded through.
        let req = user_text_request("hi");
        let llm = convert_to_llm_request_with_format(&req, ApiFormat::AiSdkText)?;
        assert_eq!(llm.api_format, ApiFormat::AiSdkText);
        Ok(())
    }

    // ---- S06: Request model serde parity (mirrors model.go tags) ----

    #[test]
    fn request_max_tokens_uses_snake_case_tag() -> Result<(), serde_json::Error> {
        // Go model.go:15 — `MaxTokens *int64 \`json:"max_tokens,omitempty"\``.
        let req: Request = serde_json::from_value(json!({
            "messages": [],
            "model": "gpt-4",
            "max_tokens": 128
        }))?;
        assert_eq!(req.max_tokens, Some(128));
        let v = serde_json::to_value(&req)?;
        assert_eq!(v["max_tokens"], 128);
        Ok(())
    }

    #[test]
    fn ui_message_part_type_field_round_trips() -> Result<(), serde_json::Error> {
        // Confirm the `#[serde(rename = "type")]` on UIMessagePart works.
        let p: UIMessagePart = serde_json::from_value(json!({"type": "text", "text": "hi"}))?;
        assert_eq!(p.part_type, "text");
        assert_eq!(p.text.as_deref(), Some("hi"));
        let v = serde_json::to_value(&p)?;
        assert_eq!(v["type"], "text");
        Ok(())
    }

    // ---- S08: parse_datastream_frame (kept from prior skeleton) ----

    #[test]
    fn parse_go_testdata_start_chunk() -> Result<(), Box<dyn std::error::Error>> {
        let chunk = r#"{"type":"start","messageId":"gen-1754577344-bfGaoVZhBY3iT78Psu02"}"#;
        let frames = parse_datastream_frame(chunk);
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            DataStreamFrame::Other { kind, raw } => {
                assert_eq!(kind, "start");
                assert_eq!(
                    raw.get("messageId").and_then(|v| v.as_str()),
                    Some("gen-1754577344-bfGaoVZhBY3iT78Psu02")
                );
            }
            other => return Err(format!("expected Other(start), got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn parse_go_testdata_text_delta_chunk() -> Result<(), Box<dyn std::error::Error>> {
        let chunk =
            r#"{"type":"text-delta","id":"text_e71f3ea3e56e4141889c58e5807203ac","delta":"Sure"}"#;
        let frames = parse_datastream_frame(chunk);
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            DataStreamFrame::Text { id, text } => {
                assert_eq!(id.as_deref(), Some("text_e71f3ea3e56e4141889c58e5807203ac"));
                assert_eq!(text, "Sure");
            }
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn parse_go_testdata_finish_chunk() -> Result<(), Box<dyn std::error::Error>> {
        let chunk = r#"{"type":"finish"}"#;
        let frames = parse_datastream_frame(chunk);
        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0], DataStreamFrame::Finish));
        Ok(())
    }

    #[test]
    fn parse_bare_done_sentinel_drops_defensively() -> Result<(), Box<dyn std::error::Error>> {
        assert!(parse_datastream_frame("[DONE]").is_empty());
        Ok(())
    }

    #[test]
    fn parse_chunk_with_surrounding_whitespace_is_trimmed() -> Result<(), Box<dyn std::error::Error>>
    {
        let chunk = "  \r\n{\"type\":\"finish\"}\n\n";
        let frames = parse_datastream_frame(chunk);
        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0], DataStreamFrame::Finish));
        Ok(())
    }

    #[test]
    fn parse_malformed_json_drops_silently() -> Result<(), Box<dyn std::error::Error>> {
        assert!(parse_datastream_frame("{not json").is_empty());
        Ok(())
    }

    #[test]
    fn parse_tool_input_available_chunk_with_json_input() -> Result<(), Box<dyn std::error::Error>>
    {
        let chunk = r#"{"type":"tool-input-available","toolCallId":"call_9","toolName":"get_weather","input":{"city":"sf"}}"#;
        let frames = parse_datastream_frame(chunk);
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            DataStreamFrame::ToolCall {
                tool_call_id,
                tool_name,
                input,
            } => {
                assert_eq!(tool_call_id.as_deref(), Some("call_9"));
                assert_eq!(tool_name.as_deref(), Some("get_weather"));
                assert!(input.contains("\"city\""));
            }
            other => return Err(format!("expected ToolCall, got {other:?}").into()),
        }
        Ok(())
    }

    // ---- S09: datastream_text_frame ----

    #[test]
    fn text_frame_builder_carries_text_with_no_id() -> Result<(), Box<dyn std::error::Error>> {
        let frame = datastream_text_frame("hi there");
        match frame {
            DataStreamFrame::Text { id, text } => {
                assert!(id.is_none());
                assert_eq!(text, "hi there");
            }
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn text_frame_builder_serialises_with_text_field() -> Result<(), serde_json::Error> {
        let frame = datastream_text_frame("delta");
        let serialised = serde_json::to_value(&frame)?;
        assert_eq!(
            serialised.get("type").and_then(|v| v.as_str()),
            Some("text")
        );
        assert_eq!(
            serialised.get("text").and_then(|v| v.as_str()),
            Some("delta")
        );
        assert!(serialised.get("id").is_none());
        let _ = json!({"type": "text"});
        Ok(())
    }

    // -------------------------------------------------------------------------
    // RUST-P8-002 S07 follow-up — `aggregate_aisdk_stream_chunks` inbound
    // helper. Mirrors Go `DataStreamTransformer.AggregateStreamChunks`
    // (datastream.go:52-170) end-to-end: gateway-level SSE events (raw JSON
    // `data` payloads) → typed wire events → folded UIMessage → HTTP body.
    // -------------------------------------------------------------------------

    /// Build a gateway-level `conduit_llm::StreamEvent` from a typed
    /// AI-SDK wire event (mirrors Go's `enqueueEvent` which marshals the
    /// typed event to JSON bytes).
    fn gateway_event(typed: &StreamEvent) -> conduit_llm::StreamEvent {
        conduit_llm::StreamEvent {
            data: serde_json::to_string(typed).ok(),
            ..conduit_llm::StreamEvent::default()
        }
    }

    // Mirrors Go's happy-path: a full text-*/finish sequence produces a
    // UIMessage body whose single text part carries the concatenated text.
    #[test]
    fn aisdk_aggregate_text_stream_into_uimessage_body() -> Result<(), serde_json::Error> {
        let typed = vec![
            StreamEvent::start("msg_1"),
            StreamEvent::start_step(),
            StreamEvent::text_start("t1"),
            StreamEvent::text_delta("t1", "Hello"),
            StreamEvent::text_delta("t1", ", world!"),
            StreamEvent::text_end("t1"),
            StreamEvent::finish_step(),
            StreamEvent::finish(),
        ];
        let events: Vec<_> = typed.iter().map(gateway_event).collect();
        let response = match aggregate_aisdk_stream_chunks(events) {
            Ok(r) => r,
            Err(err) => panic!("aggregation failed: {err:?}"),
        };
        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers.get("Content-Type").map(|s| s.as_str()),
            Some("application/json")
        );
        assert_eq!(
            response.headers.get("Cache-Control").map(|s| s.as_str()),
            Some("no-cache")
        );
        let body = match response.body.as_deref() {
            Some(b) => b,
            None => panic!("missing body"),
        };
        let parsed: serde_json::Value = serde_json::from_slice(body)?;
        assert_eq!(
            parsed.get("role").and_then(Value::as_str),
            Some("assistant")
        );
        assert_eq!(parsed.get("id").and_then(Value::as_str), Some("msg_1"));
        let parts = match parsed.get("parts").and_then(Value::as_array) {
            Some(p) => p,
            None => panic!("missing parts"),
        };
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].get("type").and_then(Value::as_str), Some("text"));
        assert_eq!(
            parts[0].get("text").and_then(Value::as_str),
            Some("Hello, world!")
        );
        // Original events preserved on `stream`.
        assert_eq!(response.stream.len(), 8);
        Ok(())
    }

    // Mirrors Go reasoning aggregation: reasoning-start/delta/end produce a
    // reasoning part; text deltas accumulate into a separate text part.
    #[test]
    fn aisdk_aggregate_reasoning_then_text_into_two_parts() {
        let typed = vec![
            StreamEvent::start("m"),
            StreamEvent::start_step(),
            StreamEvent {
                event_type: "reasoning-start".to_string(),
                id: Some("r1".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                event_type: "reasoning-delta".to_string(),
                id: Some("r1".to_string()),
                delta: Some("thinking...".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                event_type: "reasoning-end".to_string(),
                id: Some("r1".to_string()),
                ..StreamEvent::default()
            },
            StreamEvent::text_start("t1"),
            StreamEvent::text_delta("t1", "answer"),
            StreamEvent::text_end("t1"),
            StreamEvent::finish_step(),
            StreamEvent::finish(),
        ];
        let events: Vec<_> = typed.iter().map(gateway_event).collect();
        let response = match aggregate_aisdk_stream_chunks(events) {
            Ok(r) => r,
            Err(err) => panic!("aggregation failed: {err:?}"),
        };
        let body = match response.body.as_deref() {
            Some(b) => b,
            None => panic!("missing body"),
        };
        let parsed: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => panic!("failed to parse body"),
        };
        let parts = match parsed.get("parts").and_then(Value::as_array) {
            Some(p) => p,
            None => panic!("missing parts"),
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(
            parts[0].get("type").and_then(Value::as_str),
            Some("reasoning")
        );
        assert_eq!(parts[1].get("type").and_then(Value::as_str), Some("text"));
    }

    // Empty input must surface the Go-shaped `"empty stream chunks"` error.
    #[test]
    fn aisdk_aggregate_rejects_empty_input() {
        let err = aggregate_aisdk_stream_chunks(Vec::new()).err();
        assert!(err.is_some(), "expected an error");
        assert!(
            err.map(|e| e.to_string().contains("empty stream chunks"))
                .unwrap_or(false),
            "expected empty-stream-chunks error"
        );
    }

    // Malformed JSON frames are silently skipped (matching Go's `continue`
    // on `json.Unmarshal` error).
    #[test]
    fn aisdk_aggregate_skips_malformed_json_frames() {
        let typed = vec![
            StreamEvent::start("m"),
            StreamEvent::text_start("t1"),
            StreamEvent::text_delta("t1", "Hi"),
            StreamEvent::text_end("t1"),
            StreamEvent::finish(),
        ];
        let mut events: Vec<_> = typed.iter().map(gateway_event).collect();
        // Inject a malformed frame in the middle.
        events.insert(
            2,
            conduit_llm::StreamEvent {
                data: Some("{invalid json}".to_string()),
                ..conduit_llm::StreamEvent::default()
            },
        );
        let response = match aggregate_aisdk_stream_chunks(events) {
            Ok(r) => r,
            Err(err) => panic!("aggregation failed: {err:?}"),
        };
        let body = match response.body.as_deref() {
            Some(b) => b,
            None => panic!("missing body"),
        };
        let parsed: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => panic!("failed to parse body"),
        };
        let parts = match parsed.get("parts").and_then(Value::as_array) {
            Some(p) => p,
            None => panic!("missing parts"),
        };
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].get("text").and_then(Value::as_str), Some("Hi"));
    }

    // The `[DONE]` sentinel (which gateway code may append as a tail marker)
    // is dropped before aggregation.
    #[test]
    fn aisdk_aggregate_drops_done_sentinel() {
        let typed = vec![
            StreamEvent::start("m"),
            StreamEvent::text_start("t1"),
            StreamEvent::text_delta("t1", "ok"),
            StreamEvent::text_end("t1"),
            StreamEvent::finish(),
        ];
        let mut events: Vec<_> = typed.iter().map(gateway_event).collect();
        events.push(conduit_llm::StreamEvent {
            data: Some("[DONE]".to_string()),
            ..conduit_llm::StreamEvent::default()
        });
        let response = match aggregate_aisdk_stream_chunks(events) {
            Ok(r) => r,
            Err(err) => panic!("aggregation failed: {err:?}"),
        };
        // Body still serializes correctly.
        let body = match response.body.as_deref() {
            Some(b) => b,
            None => panic!("missing body"),
        };
        let parsed: Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => panic!("failed to parse body"),
        };
        assert_eq!(
            parsed
                .get("parts")
                .and_then(Value::as_array)
                .and_then(|p| p.first())
                .and_then(|p| p.get("text"))
                .and_then(Value::as_str),
            Some("ok")
        );
    }

    // ---- RUST-P15-001: additional convert_request_test.go golden cases ----
    //
    // Mirrors the pure-logic subtests of
    // `conduit/llm/transformer/aisdk/convert_request_test.go` that were not
    // already covered by prior S06 tests. Each test cites the exact Go
    // subtest it mirrors (function name + line range).
    //
    // Go tests already covered (NOT duplicated here):
    // - TestConvertToLLMRequestComprehensive_SystemMessage "system message
    //   with multiple text parts" → `convert_system_message_concatenates_text_parts`
    // - TestConvertToLLMRequestComprehensive_SystemMessage "system message
    //   with provider metadata" → pending (unified LLM model lacks provider
    //   metadata support — Go convert_request.go:193 TODO)
    // - TestConvertToLLMRequestComprehensive_UserMessage "simple user message"
    //   → `convert_user_text_message_to_chat`
    // - TestConvertToLLMRequestComprehensive_UserMessage "user message with
    //   file part" → `convert_user_file_part_becomes_image_url`
    // - TestConvertToLLMRequestComprehensive_UserMessage "user message from
    //   content string" → `convert_user_content_string_fallback`
    // - TestConvertToLLMRequestComprehensive_AssistantMessage "assistant
    //   message from content string" → `convert_assistant_text_message`
    // - TestConvertToLLMRequestComprehensive_ToolCalls "assistant message
    //   with tool output error using raw input" →
    //   `convert_assistant_tool_output_error_uses_raw_input`
    // - TestConvertToLLMRequestComprehensive_ToolCalls "dynamic tool" →
    //   `convert_dynamic_tool_uses_tool_name`
    // - TestConvertToLLMRequestComprehensive_IgnoreIncompleteToolCalls →
    //   `convert_ignore_incomplete_tool_calls_filters_input_streaming`
    // - TestConvertToLLMRequestComprehensive_Tools "converts tools" →
    //   `convert_tools_mapping`
    // - TestConvertToLLMRequestComprehensive_ErrorHandling →
    //   `convert_unsupported_role_errors`
    //
    // pending subtests:
    // - TestConvertToLLMRequestComprehensive_SystemMessage "system message
    //   with provider metadata" (L41-72): unified ChatMessage has no
    //   provider-metadata field (Go convert_request.go:193 TODO).

    /// Mirrors `TestConvertToLLMRequestComprehensive_SystemMessage` /
    /// "simple system message" (convert_request_test.go:15-39): a single
    /// text-part system message maps to one system ChatMessage with Text
    /// content.
    #[test]
    fn convert_system_single_text_part() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "system".to_string(),
                parts: vec![UIMessagePart {
                    part_type: "text".to_string(),
                    text: Some("System message".to_string()),
                    ..UIMessagePart::default()
                }],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "system");
        match &chat.messages[0].content {
            Some(MessageContent::Text(s)) => assert_eq!(s, "System message"),
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    /// Mirrors `TestConvertToLLMRequestComprehensive_SystemMessage` /
    /// "system message from content string" (convert_request_test.go:96-113):
    /// when a system message has no parts but `content` is a string, the
    /// string becomes the system message text.
    #[test]
    fn convert_system_from_content_string() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "system".to_string(),
                content: Some(json!("System message from content")),
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "system");
        match &chat.messages[0].content {
            Some(MessageContent::Text(s)) => assert_eq!(s, "System message from content"),
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    /// Mirrors `TestConvertToLLMRequestComprehensive_UserMessage` /
    /// "user message with filename" (convert_request_test.go:177-206): a
    /// file part with `filename` set still maps to `image_url` — the filename
    /// is informational only and does not change the conversion.
    #[test]
    fn convert_user_file_part_with_filename() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "user".to_string(),
                parts: vec![UIMessagePart {
                    part_type: "file".to_string(),
                    media_type: Some("image/jpeg".to_string()),
                    url: Some("https://example.com/image.jpg".to_string()),
                    filename: Some("image.jpg".to_string()),
                    ..UIMessagePart::default()
                }],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 1);
        match &chat.messages[0].content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].part_type, "image_url");
                assert_eq!(
                    parts[0].image_url.as_ref().and_then(|v| v.get("url")),
                    Some(&json!("https://example.com/image.jpg"))
                );
            }
            other => return Err(format!("expected Parts, got {other:?}").into()),
        }
        Ok(())
    }

    /// Mirrors `TestConvertToLLMRequestComprehensive_AssistantMessage` /
    /// "simple assistant text message" (convert_request_test.go:229-250): an
    /// assistant message with a single text part (state="done") produces one
    /// assistant ChatMessage with Parts content containing the text.
    #[test]
    fn convert_assistant_text_part_via_parts() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                parts: vec![UIMessagePart {
                    part_type: "text".to_string(),
                    text: Some("Hello, human!".to_string()),
                    state: Some("done".to_string()),
                    ..UIMessagePart::default()
                }],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "assistant");
        match &chat.messages[0].content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].part_type, "text");
                assert_eq!(parts[0].text.as_deref(), Some("Hello, human!"));
            }
            other => return Err(format!("expected Parts, got {other:?}").into()),
        }
        Ok(())
    }

    /// Mirrors `TestConvertToLLMRequestComprehensive_AssistantMessage` /
    /// "assistant message with reasoning" (convert_request_test.go:252-282):
    /// reasoning parts map to text content parts (Go convert_request.go:275
    /// TODO note), preserving order: reasoning first, then text.
    #[test]
    fn convert_assistant_reasoning_then_text() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                parts: vec![
                    UIMessagePart {
                        part_type: "reasoning".to_string(),
                        text: Some("Thinking...".to_string()),
                        state: Some("done".to_string()),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("Hello, human!".to_string()),
                        state: Some("done".to_string()),
                        ..UIMessagePart::default()
                    },
                ],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "assistant");
        match &chat.messages[0].content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2);
                // Reasoning maps to text type (Go convert_request.go:275).
                assert_eq!(parts[0].part_type, "text");
                assert_eq!(parts[0].text.as_deref(), Some("Thinking..."));
                assert_eq!(parts[1].part_type, "text");
                assert_eq!(parts[1].text.as_deref(), Some("Hello, human!"));
            }
            other => return Err(format!("expected Parts, got {other:?}").into()),
        }
        Ok(())
    }

    /// Mirrors `TestConvertToLLMRequestComprehensive_AssistantMessage` /
    /// "assistant message with file parts" (convert_request_test.go:284-312):
    /// a file part with an image media type in an assistant message maps to
    /// `image_url` content, mirroring the user-message file path.
    #[test]
    fn convert_assistant_file_image_part() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                parts: vec![UIMessagePart {
                    part_type: "file".to_string(),
                    media_type: Some("image/png".to_string()),
                    url: Some("data:image/png;base64,dGVzdA==".to_string()),
                    ..UIMessagePart::default()
                }],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0].role, "assistant");
        match &chat.messages[0].content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].part_type, "image_url");
                assert_eq!(
                    parts[0].image_url.as_ref().and_then(|v| v.get("url")),
                    Some(&json!("data:image/png;base64,dGVzdA=="))
                );
            }
            other => return Err(format!("expected Parts, got {other:?}").into()),
        }
        Ok(())
    }

    /// Mirrors `TestConvertToLLMRequestComprehensive_ToolCalls` /
    /// "assistant message with tool output error" (convert_request_test.go:380-415):
    /// output-error state WITH the `input` field present — the args must come
    /// from `input` (not `raw_input`), and the tool result message carries
    /// the `errorText`. This complements the existing
    /// `convert_assistant_tool_output_error_uses_raw_input` which tests the
    /// raw_input fallback.
    #[test]
    fn convert_tool_output_error_with_input_field() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                parts: vec![
                    UIMessagePart {
                        part_type: "step-start".to_string(),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("Let me calculate that for you.".to_string()),
                        state: Some("done".to_string()),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "tool-calculator".to_string(),
                        state: Some("output-error".to_string()),
                        tool_call_id: Some("call1".to_string()),
                        input: Some(json!({"operation": "add", "numbers": [1, 2]})),
                        error_text: Some("Error: Invalid input".to_string()),
                        ..UIMessagePart::default()
                    },
                ],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        // assistant + tool result
        assert_eq!(chat.messages.len(), 2);
        // Assistant tool call args come from Input (not RawInput).
        assert_eq!(chat.messages[0].tool_calls.len(), 1);
        let tc = &chat.messages[0].tool_calls[0];
        assert_eq!(tc.id.as_deref(), Some("call1"));
        assert_eq!(tc.function["name"], "calculator");
        let args: Value =
            serde_json::from_str(tc.function["arguments"].as_str().ok_or("no args")?)?;
        assert_eq!(args, json!({"operation": "add", "numbers": [1, 2]}));
        // Tool result carries the error text.
        assert_eq!(chat.messages[1].role, "tool");
        assert_eq!(chat.messages[1].tool_call_id.as_deref(), Some("call1"));
        match &chat.messages[1].content {
            Some(MessageContent::Text(s)) => assert_eq!(s, "Error: Invalid input"),
            other => return Err(format!("expected Text, got {other:?}").into()),
        }
        Ok(())
    }

    /// Mirrors `TestConvertToLLMRequestComprehensive_ToolCalls` /
    /// "multiple tool invocations with step information"
    /// (convert_request_test.go:489-554): two step-start blocks produce
    /// distinct assistant messages. Block 1 = text + tool(call-1); block 2 =
    /// tool(call-2) + tool(call-3). Verifies the exact 5-message layout and
    /// tool-call count per block.
    #[test]
    fn convert_multiple_step_blocks_three_tools() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![UIMessage {
                role: "assistant".to_string(),
                parts: vec![
                    UIMessagePart {
                        part_type: "step-start".to_string(),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("response".to_string()),
                        state: Some("done".to_string()),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "tool-screenshot".to_string(),
                        state: Some("output-available".to_string()),
                        tool_call_id: Some("call-1".to_string()),
                        input: Some(json!({"value": "value-1"})),
                        output: Some(json!("result-1")),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "step-start".to_string(),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "tool-screenshot".to_string(),
                        state: Some("output-available".to_string()),
                        tool_call_id: Some("call-2".to_string()),
                        input: Some(json!({"value": "value-2"})),
                        output: Some(json!("result-2")),
                        ..UIMessagePart::default()
                    },
                    UIMessagePart {
                        part_type: "tool-screenshot".to_string(),
                        state: Some("output-available".to_string()),
                        tool_call_id: Some("call-3".to_string()),
                        input: Some(json!({"value": "value-3"})),
                        output: Some(json!("result-3")),
                        ..UIMessagePart::default()
                    },
                ],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        // Expected layout: assistant(text+call-1), tool(call-1),
        //                   assistant(call-2+call-3), tool(call-2), tool(call-3)
        assert_eq!(chat.messages.len(), 5);

        // Block 1: text + call-1
        assert_eq!(chat.messages[0].role, "assistant");
        assert_eq!(chat.messages[0].tool_calls.len(), 1);
        assert_eq!(chat.messages[0].tool_calls[0].id.as_deref(), Some("call-1"));

        assert_eq!(chat.messages[1].role, "tool");
        assert_eq!(chat.messages[1].tool_call_id.as_deref(), Some("call-1"));

        // Block 2: call-2 + call-3
        assert_eq!(chat.messages[2].role, "assistant");
        assert_eq!(chat.messages[2].tool_calls.len(), 2);
        assert_eq!(chat.messages[2].tool_calls[0].id.as_deref(), Some("call-2"));
        assert_eq!(chat.messages[2].tool_calls[1].id.as_deref(), Some("call-3"));

        // Remaining messages are tool results for call-2 and call-3.
        let tool_msgs: Vec<&ChatMessage> = chat.messages[3..]
            .iter()
            .filter(|m| m.role == "tool")
            .collect();
        assert_eq!(tool_msgs.len(), 2);
        Ok(())
    }

    /// Mirrors `TestConvertToLLMRequestComprehensive_MultipleMessages` /
    /// "handles conversation with multiple messages"
    /// (convert_request_test.go:684-718): a user→assistant→user conversation
    /// preserves all three messages in order.
    #[test]
    fn convert_multi_turn_user_assistant_user() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4".to_string()),
            messages: vec![
                UIMessage {
                    role: "user".to_string(),
                    parts: vec![UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("What's the weather like?".to_string()),
                        ..UIMessagePart::default()
                    }],
                    ..UIMessage::default()
                },
                UIMessage {
                    role: "assistant".to_string(),
                    parts: vec![UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("I'll check that for you.".to_string()),
                        state: Some("done".to_string()),
                        ..UIMessagePart::default()
                    }],
                    ..UIMessage::default()
                },
                UIMessage {
                    role: "user".to_string(),
                    parts: vec![UIMessagePart {
                        part_type: "text".to_string(),
                        text: Some("Thanks!".to_string()),
                        ..UIMessagePart::default()
                    }],
                    ..UIMessage::default()
                },
            ],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.messages.len(), 3);
        assert_eq!(chat.messages[0].role, "user");
        assert_eq!(chat.messages[1].role, "assistant");
        assert_eq!(chat.messages[2].role, "user");
        Ok(())
    }

    /// Mirrors `TestConvertToLLMRequestComprehensive_PreservesRequestFields` /
    /// "preserves model and stream settings" (convert_request_test.go:740-762):
    /// the model field is threaded through to the LlmRequest. The Go code
    /// hardcodes `Stream: lo.ToPtr(true)` (convert_request.go:37), so the
    /// Rust `stream: true` is always true regardless of input.
    #[test]
    fn convert_preserves_model_and_stream() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4-turbo".to_string()),
            stream: Some(true),
            messages: vec![UIMessage {
                role: "user".to_string(),
                parts: vec![UIMessagePart {
                    part_type: "text".to_string(),
                    text: Some("Hello".to_string()),
                    ..UIMessagePart::default()
                }],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        assert_eq!(llm.model.as_deref(), Some("gpt-4-turbo"));
        // Go convert_request.go:37 always sets Stream to true.
        assert!(llm.stream);
        Ok(())
    }

    /// Parity companion to `convert_preserves_model_and_stream`: verifies
    /// that `temperature` and `max_tokens` from the AI-SDK request are
    /// threaded through to the unified ChatRequest payload.
    #[test]
    fn convert_preserves_temperature_and_max_tokens() -> Result<(), Box<dyn std::error::Error>> {
        let req = Request {
            model: Some("gpt-4".to_string()),
            temperature: Some(0.7),
            max_tokens: Some(256),
            messages: vec![UIMessage {
                role: "user".to_string(),
                parts: vec![UIMessagePart {
                    part_type: "text".to_string(),
                    text: Some("Hello".to_string()),
                    ..UIMessagePart::default()
                }],
                ..UIMessage::default()
            }],
            ..Request::default()
        };
        let llm = convert_to_llm_request(&req)?;
        let chat = match &llm.payload {
            LlmRequestPayload::Chat(c) => c,
            _ => return Err("expected Chat".into()),
        };
        assert_eq!(chat.temperature, Some(0.7));
        assert_eq!(chat.max_tokens, Some(256));
        Ok(())
    }

    // ---- RUST-P15-001: factory_test.go coverage catalogue ----
    //
    // Go `conduit/llm/transformer/aisdk/factory_test.go` (107 lines, 3 test
    // functions) coverage status:
    //
    // COVERED (by existing tests in this module):
    // - `TestNewTransformer` (L10-55): the header-based dispatch on
    //   `X-Vercel-Ai-Ui-Message-Stream == "v1"` is fully covered by
    //   `select_format_data_stream_header_v1_is_datastream`,
    //   `select_format_data_stream_header_other_value_is_text`, and
    //   `select_format_no_header_or_other_headers_is_text` (all 4 Go
    //   table-cases map to these 3 Rust tests).
    //
    // N/A — NO DIRECT RUST EQUIVALENT (design difference, not a gap):
    // - `TestNewTransformerByType` (L57-90): Go's `NewTransformerByType`
    //   takes a `TransformerType` string enum and returns a concrete
    //   transformer struct. The Rust design uses a header-only factory
    //   (`select_aisdk_format`) + the `AiSdkFormat` enum — there is no
    //   type-string-to-transformer dispatch function. The semantics (DataStream
    //   vs Text) are tested via the header dispatch tests above.
    // - `TestTransformerTypeConstants` (L92-95): Go's constants are typed
    //   strings (`TransformerTypeText = "text"`,
    //   `TransformerTypeDataStream = "datastream"`). The Rust `AiSdkFormat`
    //   is a plain enum without string serialization, so the constant-value
    //   assertion has no Rust analogue. The dispatch semantics are tested.
}
