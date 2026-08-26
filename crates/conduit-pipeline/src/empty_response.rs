//! Empty-response detection (RUST-P8-002 S11).
//!
//! Ports Go `conduit/llm/pipeline/empty_response.go` and the pre-read logic of
//! `conduit/llm/pipeline/stream.go::preReadLlmStream`/`hasFinishReason` into
//! pure, testable Rust. The detector is consulted:
//!
//! - **Non-stream** after `Outbound.TransformResponse` — mirrors Go
//!   `non_streaming.go` line 55: `if p.emptyResponseDetection && !hasResponseContent(llmResp)`.
//! - **Stream** by pre-reading the first events of the LLM stream — mirrors Go
//!   `stream.go::preReadLlmStream` (lines 153-217): up to 3 events when empty-
//!   response detection is enabled, otherwise exactly 1 (for the first-event
//!   timeout guard).
//!
//! # Pre-read count (Go `stream.go:158-162`)
//!
//! ```text
//! const maxPreReadEvents = 3
//! preReadLimit := maxPreReadEvents
//! if !p.emptyResponseDetection {
//!     preReadLimit = 1
//! }
//! ```
//!
//! So: **3 events when detection is on, 1 when it is off** (the 1-event case is
//! only for the first-event-timeout guard, not for emptiness checks).
//!
//! # "Meaningful content" (Go `empty_response.go:25-130`)
//!
//! A response is non-empty if any of these hold:
//! - `embedding.data` is non-empty;
//! - `rerank.results` is non-empty;
//! - `image.data` is non-empty;
//! - `video` has `id`/`status`/`video_url`/`error`;
//! - `compact.output` is non-empty;
//! - `speech.audio` is non-empty;
//! - `transcription` has `text` or non-empty `raw`;
//! - `speech_stream_event.audio_base64 != ""` (delta audio only — a bare
//!   `speech.audio.done` event counts as empty, see Go comment lines 100-105);
//! - `speech_audio_chunk.audio` is non-empty;
//! - `transcription_stream_event` has `delta`/`text`/`type`;
//! - `completion.choices[*].text != ""`;
//! - any `choices[*].delta` or `choices[*].message` has message content.
//!
//! Message content (Go `hasMessageContent`, lines 25-63): non-empty `content`
//! string, non-empty `multiple_content`, non-empty `tool_calls`, non-empty
//! `reasoning_content`/`reasoning`/`reasoning_signature`, non-empty `refusal`,
//! or a present `audio`.
//!
//! The shared `llm.DoneResponse` sentinel (Go) and any freshly-constructed
//! `Object:"[DONE]"` terminator are both treated as *not* content (Go line 67).
//!
//! # Retry signal (Go `non_streaming.go:55-59`, `stream.go:193-202`)
//!
//! When the verdict is "empty" the pipeline returns [`ErrEmptyResponse`]; that
//! error is what the retry cursor's `CanRetry` predicate pattern-matches
//! (`errors.Is(err, ErrEmptyResponse)`), so a transformer that wants empty-
//! response retries just classifies this error as retryable. The other errors
//! here (`ErrStreamFirstEventTimeout`, `ErrNonStreamResponseTimeout`,
//! `ErrEmptyStreamChunks`, `ErrEmptyAggregatedBody`) are surfaced for the same
//! reason — they are the distinct retryable signals the rest of the pipeline
//! emits.
//!
//! Note on the `LlmResponse` shape: the unified Rust model
//! (`conduit_llm::LlmResponse`) keeps the provider-typed sub-responses
//! (`embedding`, `rerank`, `image`, `video`, `compact`, `completion`,
//! `speech`, `transcription`, `speech_stream_event`,
//! `transcription_stream_event`) as `Option<serde_json::Value>` until the
//! dedicated transformer ports land (see `crates/conduit-llm/src/model.rs`
//! doc comment lines 494-501). The detector therefore inspects them via small
//! `Value` accessor helpers that mirror the Go struct-field checks. The
//! chat-path fields it also needs (`choices[*].{message,delta,finish_reason}`,
//! `object`) are typed directly on `LlmResponse`/`Choice`/`LlmMessage` and are
//! used directly. When the structured sub-response types are ported later, the
//! `Value` accessors can be swapped for typed matches without changing call
//! sites.

use conduit_llm::{LlmMessage, LlmResponse, MessageContent};
use serde_json::Value;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error sentinels (Go `empty_response.go:11-23`).
// ---------------------------------------------------------------------------

/// Indicates the response contains no meaningful content. Triggers channel
/// retry when empty-response detection is enabled.
///
/// Mirrors Go `ErrEmptyResponse` (`empty_response.go:11`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("empty response detected")]
pub struct ErrEmptyResponse;

/// Indicates a streaming response did not produce the first event in time.
/// Mirrors Go `ErrStreamFirstEventTimeout` (`empty_response.go:14`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("stream first event timeout")]
pub struct ErrStreamFirstEventTimeout;

/// Indicates a non-streaming response did not complete in time.
/// Mirrors Go `ErrNonStreamResponseTimeout` (`empty_response.go:17`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("non-stream response timeout")]
pub struct ErrNonStreamResponseTimeout;

/// Indicates an auto-upgraded streaming request produced no inbound chunks.
/// Mirrors Go `ErrEmptyStreamChunks` (`empty_response.go:20`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("empty stream chunks")]
pub struct ErrEmptyStreamChunks;

/// Indicates inbound chunk aggregation produced an empty body.
/// Mirrors Go `ErrEmptyAggregatedBody` (`empty_response.go:23`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("empty aggregated body")]
pub struct ErrEmptyAggregatedBody;

/// The `[DONE]` terminator object value used by SSE chat streams (Go
/// `llm.DoneResponse.Object == "[DONE]"`).
const DONE_OBJECT: &str = "[DONE]";

// ---------------------------------------------------------------------------
// hasMessageContent (Go `empty_response.go:25-63`).
// ---------------------------------------------------------------------------

/// Whether an assistant [`LlmMessage`] carries any meaningful content.
///
/// Mirrors Go `hasMessageContent` (`empty_response.go:25-63`). Field-by-field:
///
/// | Go field                              | Rust field                |
/// |---------------------------------------|---------------------------|
/// | `Content.Content *string`             | `content: Option<MessageContent>` (non-empty text form) |
/// | `Content.MultipleContent []ContentPart` | (non-empty `Parts` form) |
/// | `ToolCalls []ToolCall`                | `tool_calls: Vec<ToolCall>` (non-empty) |
/// | `ReasoningContent *string`            | `reasoning_content`       |
/// | `Reasoning *string`                   | `reasoning`               |
/// | `ReasoningSignature *string`          | `reasoning_signature`     |
/// | `Refusal string`                      | `refusal` (non-empty)     |
/// | `Audio *Audio`                        | `audio` (present)         |
pub fn has_message_content(msg: Option<&LlmMessage>) -> bool {
    let Some(msg) = msg else { return false };

    // Content: string or parts. Go: `Content.Content != nil && *Content.Content != ""`
    // OR `len(Content.MultipleContent) > 0`. In Rust both map onto the
    // `MessageContent` enum (Text / Parts / Json) — a non-empty Text or a
    // non-empty Parts slice counts. Json falls back to the "non-empty content"
    // path only if the JSON object is non-null (mirrors Go treating
    // `MultipleContent` as the structured form); a bare `null` is empty.
    if let Some(content) = &msg.content {
        match content {
            MessageContent::Text(s) if !s.is_empty() => return true,
            MessageContent::Parts(parts) if !parts.is_empty() => return true,
            _ => {}
        }
    }

    // Tool calls.
    if !msg.tool_calls.is_empty() {
        return true;
    }

    // Reasoning fields (Go: pointer + non-empty string).
    if msg
        .reasoning_content
        .as_ref()
        .is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    if msg.reasoning.as_ref().is_some_and(|s| !s.is_empty()) {
        return true;
    }
    if msg
        .reasoning_signature
        .as_ref()
        .is_some_and(|s| !s.is_empty())
    {
        return true;
    }

    // Refusal (Go `Refusal string` — non-empty plain string).
    if msg.refusal.as_ref().is_some_and(|s| !s.is_empty()) {
        return true;
    }

    // Audio (Go `Audio != nil` — pointer presence alone is enough).
    if msg.audio.is_some() {
        return true;
    }

    false
}

// ---------------------------------------------------------------------------
// hasResponseContent (Go `empty_response.go:65-130`).
// ---------------------------------------------------------------------------

/// Whether an [`LlmResponse`] contains any meaningful content.
///
/// Mirrors Go `hasResponseContent` (`empty_response.go:65-130`). Returns
/// `false` for `None`, the `[DONE]` terminator object, or a response with no
/// populated content fields.
pub fn has_response_content(resp: Option<&LlmResponse>) -> bool {
    let Some(resp) = resp else { return false };

    // Go: `resp == llm.DoneResponse || resp.Object == "[DONE]"`. Rust has no
    // shared sentinel pointer, so the `Object == "[DONE]"` check covers both
    // the sentinel and freshly-constructed terminators (Go line 67 + comment
    // in `stream.go:189-192`).
    if resp.object == DONE_OBJECT {
        return false;
    }

    // Provider sub-responses are `Option<Value>` until their typed ports
    // land. Each helper below mirrors one Go struct-field presence/length
    // check against the equivalent JSON shape.
    if embedding_has_data(&resp.embedding) {
        return true;
    }
    if rerank_has_results(&resp.rerank) {
        return true;
    }
    if image_has_data(&resp.image) {
        return true;
    }
    if video_has_meaningful_fields(&resp.video) {
        return true;
    }
    if compact_has_output(&resp.compact) {
        return true;
    }
    if speech_has_audio(&resp.speech) {
        return true;
    }
    if transcription_has_content(&resp.transcription) {
        return true;
    }
    // Only audio deltas count as content — a bare "speech.audio.done" event
    // with no audio chunks must still be treated as empty so empty-response
    // detection can retry (Go comment lines 100-105).
    if speech_stream_event_has_audio(&resp.speech_stream_event) {
        return true;
    }
    // Go `SpeechAudioChunk` is `json:"-"` so it is not modeled on
    // `LlmResponse`; the dedicated binary-speech port will plumb it through
    // `extra` or a typed field. We surface the same intent by consulting the
    // flattened `extra` map for a `speech_audio_chunk` entry with non-empty
    // `audio` (best-effort parity; the typed path wins once ported).
    if speech_audio_chunk_extra_has_audio(&resp.extra) {
        return true;
    }
    if transcription_stream_event_has_content(&resp.transcription_stream_event) {
        return true;
    }
    if completion_has_text(&resp.completion) {
        return true;
    }

    // Chat choices: any delta or message with content (Go lines 123-127).
    for choice in &resp.choices {
        if has_message_content(choice.delta.as_ref())
            || has_message_content(choice.message.as_ref())
        {
            return true;
        }
    }

    false
}

/// Whether any choice on the response carries a `finish_reason` (Go
/// `hasFinishReason`, `stream.go:135-147`). Used by [`pre_read_llm_stream`]
/// to recognize end-of-stream without content.
pub fn has_finish_reason(resp: Option<&LlmResponse>) -> bool {
    let Some(resp) = resp else { return false };
    resp.choices
        .iter()
        .any(|choice| choice.finish_reason.is_some())
}

// ---------------------------------------------------------------------------
// Provider sub-response Value accessors (mirror Go struct-field checks).
// ---------------------------------------------------------------------------

/// Mirrors Go `resp.Embedding != nil && len(resp.Embedding.Data) > 0`
/// (`empty_response.go:71-73`). The OpenAI list shape is
/// `{"object":"list","data":[...]}`.
fn embedding_has_data(embedding: &Option<Value>) -> bool {
    match embedding {
        Some(Value::Object(map)) => match map.get("data") {
            Some(Value::Array(arr)) => !arr.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

/// Mirrors Go `resp.Rerank != nil && len(resp.Rerank.Results) > 0`
/// (`empty_response.go:75-77`).
fn rerank_has_results(rerank: &Option<Value>) -> bool {
    match rerank {
        Some(Value::Object(map)) => match map.get("results") {
            Some(Value::Array(arr)) => !arr.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

/// Mirrors Go `resp.Image != nil && len(resp.Image.Data) > 0`
/// (`empty_response.go:79-81`).
fn image_has_data(image: &Option<Value>) -> bool {
    match image {
        Some(Value::Object(map)) => match map.get("data") {
            Some(Value::Array(arr)) => !arr.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

/// Mirrors Go `resp.Video != nil && (ID != "" || Status != "" || VideoURL != "" || Error != nil)`
/// (`empty_response.go:83-86`). Field tags are json `id`, `status`,
/// `video_url`, `error`.
fn video_has_meaningful_fields(video: &Option<Value>) -> bool {
    let Some(Value::Object(map)) = video else {
        return false;
    };
    let non_empty_str = |key: &str| match map.get(key) {
        Some(Value::String(s)) => !s.is_empty(),
        _ => false,
    };
    non_empty_str("id")
        || non_empty_str("status")
        || non_empty_str("video_url")
        || map.contains_key("error")
}

/// Mirrors Go `resp.Compact != nil && len(resp.Compact.Output) > 0`
/// (`empty_response.go:88-90`).
fn compact_has_output(compact: &Option<Value>) -> bool {
    match compact {
        Some(Value::Object(map)) => match map.get("output") {
            Some(Value::Array(arr)) => !arr.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

/// Mirrors Go `resp.Speech != nil && len(resp.Speech.Audio) > 0`
/// (`empty_response.go:92-94`). `Audio` is `[]byte` (a JSON string in the
/// unified model).
fn speech_has_audio(speech: &Option<Value>) -> bool {
    match speech {
        Some(Value::Object(map)) => match map.get("audio") {
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Array(arr)) => !arr.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

/// Mirrors Go `resp.Transcription != nil && (Text != "" || len(Raw) > 0)`
/// (`empty_response.go:96-98`).
fn transcription_has_content(transcription: &Option<Value>) -> bool {
    let Some(Value::Object(map)) = transcription else {
        return false;
    };
    let non_empty_text = matches!(map.get("text"), Some(Value::String(s)) if !s.is_empty());
    let non_empty_raw = matches!(map.get("raw"), Some(Value::Array(arr)) if !arr.is_empty())
        || matches!(map.get("raw"), Some(Value::String(s)) if !s.is_empty());
    non_empty_text || non_empty_raw
}

/// Mirrors Go `resp.SpeechStreamEvent != nil && resp.SpeechStreamEvent.AudioBase64 != ""`
/// (`empty_response.go:103-105`). The "audio.delta" event carries the base64
/// payload; a bare "audio.done" event has empty `AudioBase64` and must NOT
/// count as content.
fn speech_stream_event_has_audio(event: &Option<Value>) -> bool {
    let Some(Value::Object(map)) = event else {
        return false;
    };
    matches!(map.get("audio_base64"), Some(Value::String(s)) if !s.is_empty())
}

/// Best-effort port of Go `resp.SpeechAudioChunk != nil && len(Audio) > 0`
/// (`empty_response.go:107-109`). The Go struct has `json:"-"` so it is not
/// present on `LlmResponse`; transformers may stash it in `extra` under
/// `speech_audio_chunk`. Once the typed binary-speech port lands, swap this
/// for a direct field check.
fn speech_audio_chunk_extra_has_audio(extra: &std::collections::BTreeMap<String, Value>) -> bool {
    match extra.get("speech_audio_chunk") {
        Some(Value::Object(map)) => match map.get("audio") {
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Array(arr)) => !arr.is_empty(),
            _ => false,
        },
        _ => false,
    }
}

/// Mirrors Go `resp.TranscriptionStreamEvent != nil && (Delta != "" || Text != "" || Type != "")`
/// (`empty_response.go:111-113`).
fn transcription_stream_event_has_content(event: &Option<Value>) -> bool {
    let Some(Value::Object(map)) = event else {
        return false;
    };
    let non_empty_str = |key: &str| matches!(map.get(key), Some(Value::String(s)) if !s.is_empty());
    non_empty_str("delta") || non_empty_str("text") || non_empty_str("type")
}

/// Mirrors Go `resp.Completion != nil` over `Completion.Choices[*].Text`
/// (`empty_response.go:115-121`). Any non-empty text counts.
fn completion_has_text(completion: &Option<Value>) -> bool {
    let Some(Value::Object(map)) = completion else {
        return false;
    };
    if let Some(Value::Array(arr)) = map.get("choices") {
        for choice in arr {
            if let Some(choice_map) = choice.as_object()
                && matches!(choice_map.get("text"), Some(Value::String(s)) if !s.is_empty())
            {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// pre_read_llm_stream (Go `stream.go:153-217`).
// ---------------------------------------------------------------------------

/// Maximum number of events the pre-read phase ever buffers (Go
/// `stream.go:158`).
pub const MAX_PRE_READ_EVENTS: usize = 3;

/// Outcome of [`pre_read_llm_stream`].
#[derive(Debug, PartialEq)]
pub enum PreReadOutcome<T> {
    /// The stream had content or did not reach a definitive terminator within
    /// the pre-read window; proceed with the stream composed of the buffered
    /// events followed by the remaining events produced by `next_event`.
    Continue {
        /// Buffered events already consumed from the stream, to be replayed
        /// before any further events.
        buffered: Vec<T>,
    },
    /// The stream reached end-of-stream without producing meaningful content
    /// (Go `preReadLlmStream` returns `ErrEmptyResponse`). Caller should
    /// surface [`ErrEmptyResponse`] to trigger retry.
    Empty,
}

/// Pre-read up to `MAX_PRE_READ_EVENTS` events from an LLM stream and decide
/// whether it is empty.
///
/// Mirrors Go `pipeline.preReadLlmStream` (`stream.go:153-217`). The contract:
///
/// - `empty_response_detection == false`: pre-read exactly **1** event (Go
///   `preReadLimit = 1`) and return `Continue` regardless of content — the
///   pre-read exists only to drive the first-event timeout guard.
/// - `empty_response_detection == true`: pre-read up to **3** events. For
///   each event:
///   - if `has_response_content(event)` → `Continue` (Go line 184-187);
///   - else if the event is the `[DONE]` terminator (sentinel or freshly
///     constructed) or carries a `finish_reason` → `Empty` (Go line 193-202);
///   - else buffer and keep reading.
/// - If after `3` events no content and no terminator was found, return
///   `Continue` (Go "safe default", lines 211-214).
///
/// `next_event` is a closure the caller supplies; it returns:
/// - `Ok(Some(event))` when the stream produced an event;
/// - `Ok(None)` when the stream ended cleanly;
/// - `Err(e)` when the stream errored (propagated as `Err`).
///
/// The buffered events are returned with the outcome so the caller can
/// prepend them to the remaining stream (Go `streams.PrependStream`).
pub fn pre_read_llm_stream<E, F>(
    empty_response_detection: bool,
    mut next_event: F,
) -> Result<PreReadOutcome<LlmResponse>, E>
where
    F: FnMut() -> Result<Option<LlmResponse>, E>,
{
    // Go lines 159-162: detection off -> pre-read limit is 1, no content check.
    let pre_read_limit = if empty_response_detection {
        MAX_PRE_READ_EVENTS
    } else {
        1
    };

    let mut buffered: Vec<LlmResponse> = Vec::with_capacity(pre_read_limit);

    for i in 0..pre_read_limit {
        let event = match next_event()? {
            Some(event) => event,
            None => break, // stream ended cleanly before the limit
        };
        buffered.push(event);

        if !empty_response_detection {
            // Detection off: we only needed the first event for the timeout
            // guard. Replay it and continue (Go line 180-182).
            return Ok(PreReadOutcome::Continue { buffered });
        }

        let current = &buffered[i];
        if has_response_content(Some(current)) {
            // Has content, not empty — replay buffered events and continue
            // (Go line 184-187).
            return Ok(PreReadOutcome::Continue { buffered });
        }

        // Recognize both the shared sentinel and a freshly-constructed
        // "[DONE]" terminator, plus a finish_reason-bearing event, as
        // end-of-stream. With no content seen, this is an empty response.
        // (Go line 193-202.)
        let is_done = current.object == DONE_OBJECT;
        if is_done || has_finish_reason(Some(current)) {
            return Ok(PreReadOutcome::Empty);
        }
    }

    // Didn't find content or finish in `pre_read_limit` events — treat as
    // non-empty (Go safe-default, lines 211-214).
    Ok(PreReadOutcome::Continue { buffered })
}

#[cfg(test)]
mod tests {
    use super::*;
    // `Choice`/`ToolCall` are test-only (the lib code reads them through
    // `LlmResponse` field types), so import them here to keep the lib build
    // warning-free.
    use conduit_llm::{Choice, ToolCall};
    use serde_json::json;

    // -- has_message_content (Go hasMessageContent) -------------------------

    fn msg_with_text(s: &str) -> LlmMessage {
        LlmMessage {
            content: Some(MessageContent::Text(s.to_string())),
            ..LlmMessage::default()
        }
    }

    #[test]
    fn none_message_has_no_content() {
        assert!(!has_message_content(None));
    }

    #[test]
    fn empty_message_has_no_content() {
        assert!(!has_message_content(Some(&LlmMessage::default())));
    }

    #[test]
    fn text_content_counts() {
        assert!(has_message_content(Some(&msg_with_text("hello"))));
        assert!(!has_message_content(Some(&msg_with_text(""))));
    }

    #[test]
    fn parts_content_counts_when_non_empty() {
        let msg = LlmMessage {
            content: Some(MessageContent::Parts(vec![])),
            ..LlmMessage::default()
        };
        assert!(!has_message_content(Some(&msg)));

        let msg = LlmMessage {
            content: Some(MessageContent::Parts(vec![conduit_llm::ContentPart {
                part_type: "text".to_string(),
                text: Some("hi".to_string()),
                ..Default::default()
            }])),
            ..LlmMessage::default()
        };
        assert!(has_message_content(Some(&msg)));
    }

    #[test]
    fn tool_calls_count() {
        let msg = LlmMessage {
            tool_calls: vec![ToolCall {
                id: Some("call_1".to_string()),
                call_type: "function".to_string(),
                function: json!({}),
                ..Default::default()
            }],
            ..LlmMessage::default()
        };
        assert!(has_message_content(Some(&msg)));
    }

    #[test]
    fn reasoning_fields_count() {
        let cases = [
            LlmMessage {
                reasoning_content: Some("thinking".to_string()),
                ..Default::default()
            },
            LlmMessage {
                reasoning: Some("thinking".to_string()),
                ..Default::default()
            },
            LlmMessage {
                reasoning_signature: Some("gAAAA_reasoning".to_string()),
                ..Default::default()
            },
        ];
        for msg in &cases {
            assert!(has_message_content(Some(msg)), "should count reasoning");
        }
        // Empty reasoning strings do not count.
        let empty = LlmMessage {
            reasoning_content: Some(String::new()),
            ..Default::default()
        };
        assert!(!has_message_content(Some(&empty)));
    }

    #[test]
    fn refusal_and_audio_count() {
        let refusal = LlmMessage {
            refusal: Some("declined".to_string()),
            ..Default::default()
        };
        assert!(has_message_content(Some(&refusal)));

        let audio = LlmMessage {
            audio: Some(conduit_llm::OutputAudio::default()),
            ..Default::default()
        };
        assert!(has_message_content(Some(&audio)));
    }

    // -- has_response_content (Go hasResponseContent) -----------------------

    fn resp_with_message(msg: LlmMessage) -> LlmResponse {
        LlmResponse {
            choices: vec![Choice {
                message: Some(msg),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn resp_with_delta(msg: LlmMessage) -> LlmResponse {
        LlmResponse {
            choices: vec![Choice {
                delta: Some(msg),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn none_response_has_no_content() {
        assert!(!has_response_content(None));
    }

    #[test]
    fn empty_response_has_no_content() {
        // Mirrors Go "empty response" subtest.
        assert!(!has_response_content(Some(&LlmResponse::default())));
    }

    #[test]
    fn done_object_is_not_content() {
        // Mirrors Go `resp.Object == "[DONE]"` short-circuit.
        let resp = LlmResponse {
            object: DONE_OBJECT.to_string(),
            ..Default::default()
        };
        assert!(!has_response_content(Some(&resp)));
    }

    #[test]
    fn message_text_content_counts() {
        // Mirrors Go "message text content" subtest.
        let resp = resp_with_message(msg_with_text("hello"));
        assert!(has_response_content(Some(&resp)));
    }

    #[test]
    fn tool_calls_in_message_count() {
        // Mirrors Go "tool calls" subtest.
        let msg = LlmMessage {
            tool_calls: vec![ToolCall {
                id: Some("call_1".to_string()),
                call_type: "function".to_string(),
                function: json!({}),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(has_response_content(Some(&resp_with_message(msg))));
    }

    #[test]
    fn reasoning_content_in_message_counts() {
        // Mirrors Go "reasoning content" subtest.
        let msg = LlmMessage {
            reasoning_content: Some("thinking".to_string()),
            ..Default::default()
        };
        assert!(has_response_content(Some(&resp_with_message(msg))));
    }

    #[test]
    fn reasoning_signature_in_delta_counts() {
        // Mirrors Go TestHasResponseContent_ReasoningSignature: a delta chunk
        // carrying only `ReasoningSignature` is meaningful content.
        let msg = LlmMessage {
            reasoning_signature: Some("gAAAA_reasoning".to_string()),
            ..Default::default()
        };
        let resp = LlmResponse {
            object: "chat.completion.chunk".to_string(),
            choices: vec![Choice {
                delta: Some(msg),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(has_response_content(Some(&resp)));
    }

    #[test]
    fn speech_response_audio_counts_empty_does_not() {
        // Mirrors Go "speech response audio" + "empty speech response".
        let with_audio = LlmResponse {
            speech: Some(json!({"audio": [1, 2]})),
            ..Default::default()
        };
        assert!(has_response_content(Some(&with_audio)));

        let empty_speech = LlmResponse {
            speech: Some(json!({})),
            ..Default::default()
        };
        assert!(!has_response_content(Some(&empty_speech)));
    }

    #[test]
    fn transcription_text_and_raw_count_empty_does_not() {
        // Mirrors Go "transcription response text"/"raw"/"empty".
        let with_text = LlmResponse {
            transcription: Some(json!({"text": "hello"})),
            ..Default::default()
        };
        assert!(has_response_content(Some(&with_text)));

        let with_raw = LlmResponse {
            transcription: Some(json!({"raw": "1\n00:00:00,000 --> 00:00:01,000\nhi\n"})),
            ..Default::default()
        };
        assert!(has_response_content(Some(&with_raw)));

        let empty_tr = LlmResponse {
            transcription: Some(json!({})),
            ..Default::default()
        };
        assert!(!has_response_content(Some(&empty_tr)));
    }

    #[test]
    fn embedding_data_counts() {
        // Mirrors Go "embedding response data" subtest.
        let resp = LlmResponse {
            embedding: Some(json!({
                "object": "list",
                "data": [{
                    "object": "embedding",
                    "embedding": [0.1, 0.2, 0.3],
                    "index": 0
                }]
            })),
            ..Default::default()
        };
        assert!(has_response_content(Some(&resp)));
    }

    #[test]
    fn speech_audio_done_event_is_not_content() {
        // A bare "speech.audio.done" event with no audio_base64 must NOT
        // count (Go comment lines 100-105).
        let resp = LlmResponse {
            speech_stream_event: Some(json!({"type": "speech.audio.done"})),
            ..Default::default()
        };
        assert!(!has_response_content(Some(&resp)));
    }

    #[test]
    fn speech_audio_delta_event_is_content() {
        let resp = LlmResponse {
            speech_stream_event: Some(json!({
                "type": "speech.audio.delta",
                "audio_base64": "YWJjZA=="
            })),
            ..Default::default()
        };
        assert!(has_response_content(Some(&resp)));
    }

    #[test]
    fn transcription_stream_event_counts() {
        let resp = LlmResponse {
            transcription_stream_event: Some(json!({"delta": "hi"})),
            ..Default::default()
        };
        assert!(has_response_content(Some(&resp)));

        let resp2 = LlmResponse {
            transcription_stream_event: Some(json!({"type": "delta.created"})),
            ..Default::default()
        };
        assert!(has_response_content(Some(&resp2)));
    }

    #[test]
    fn completion_text_counts() {
        let resp = LlmResponse {
            completion: Some(json!({
                "choices": [{"text": "hello"}, {"text": ""}]
            })),
            ..Default::default()
        };
        assert!(has_response_content(Some(&resp)));

        let empty = LlmResponse {
            completion: Some(json!({"choices": [{"text": ""}]})),
            ..Default::default()
        };
        assert!(!has_response_content(Some(&empty)));
    }

    #[test]
    fn video_meaningful_fields_count() {
        let resp = LlmResponse {
            video: Some(json!({"id": "vid_1"})),
            ..Default::default()
        };
        assert!(has_response_content(Some(&resp)));

        let empty = LlmResponse {
            video: Some(json!({})),
            ..Default::default()
        };
        assert!(!has_response_content(Some(&empty)));
    }

    #[test]
    fn rerank_and_image_and_compact_count() {
        let rerank = LlmResponse {
            rerank: Some(json!({"results": [{"index": 0}]})),
            ..Default::default()
        };
        assert!(has_response_content(Some(&rerank)));

        let image = LlmResponse {
            image: Some(json!({"data": [{"url": "x"}]})),
            ..Default::default()
        };
        assert!(has_response_content(Some(&image)));

        let compact = LlmResponse {
            compact: Some(json!({"output": ["a"]})),
            ..Default::default()
        };
        assert!(has_response_content(Some(&compact)));
    }

    #[test]
    fn delta_message_content_also_counts() {
        let resp = resp_with_delta(msg_with_text("chunk"));
        assert!(has_response_content(Some(&resp)));
    }

    // -- has_finish_reason (Go hasFinishReason) -----------------------------

    #[test]
    fn has_finish_reason_detects_present_reason() {
        let resp = LlmResponse {
            choices: vec![Choice {
                finish_reason: Some("stop".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(has_finish_reason(Some(&resp)));

        let empty = LlmResponse::default();
        assert!(!has_finish_reason(Some(&empty)));
    }

    // -- pre_read_llm_stream (Go preReadLlmStream) --------------------------

    fn run_pre_read(
        empty_detection: bool,
        events: Vec<LlmResponse>,
    ) -> Result<PreReadOutcome<LlmResponse>, std::convert::Infallible> {
        let mut source = events;
        let mut idx = 0;
        // Drain from `source` sequentially. Infallible here; the empty-response
        // logic never inspects stream errors (those propagate as `Err` in
        // production via the closure).
        let outcome = pre_read_llm_stream::<std::convert::Infallible, _>(empty_detection, || {
            if idx < source.len() {
                let e = std::mem::take(&mut source[idx]);
                idx += 1;
                Ok(Some(e))
            } else {
                Ok(None)
            }
        })?;
        Ok(outcome)
    }

    #[test]
    fn pre_read_detection_off_returns_after_one_event() {
        // Detection off: pre-read exactly 1 event regardless of content.
        let events = vec![LlmResponse::default(), LlmResponse::default()];
        let outcome = run_pre_read(false, events).ok();
        assert!(
            matches!(outcome, Some(PreReadOutcome::Continue { buffered }) if buffered.len() == 1)
        );
    }

    #[test]
    fn pre_read_returns_continue_when_content_found() {
        // First event has content -> Continue with one buffered event.
        let events = vec![resp_with_message(msg_with_text("hi"))];
        let outcome = run_pre_read(true, events).ok();
        match outcome {
            Some(PreReadOutcome::Continue { buffered }) => {
                assert_eq!(buffered.len(), 1);
            }
            _ => panic!("expected Continue"),
        }
    }

    #[test]
    fn pre_read_returns_empty_for_finish_reason_without_content() {
        // Mirrors Go's empty-stream test case: a single event with
        // `finish_reason: "stop"` and an empty message is empty.
        let events = vec![LlmResponse {
            choices: vec![Choice {
                finish_reason: Some("stop".to_string()),
                message: Some(LlmMessage::default()),
                ..Default::default()
            }],
            ..Default::default()
        }];
        let outcome = run_pre_read(true, events).ok();
        assert!(matches!(outcome, Some(PreReadOutcome::Empty)));
    }

    #[test]
    fn pre_read_returns_empty_for_done_object_without_content() {
        // Mirrors Go's empty binary TTS case: terminal `[DONE]` object with no
        // audio chunks.
        let events = vec![LlmResponse {
            object: DONE_OBJECT.to_string(),
            ..Default::default()
        }];
        let outcome = run_pre_read(true, events).ok();
        assert!(matches!(outcome, Some(PreReadOutcome::Empty)));
    }

    #[test]
    fn pre_read_returns_empty_for_speech_audio_done_only() {
        // Mirrors Go's empty TTS SSE case: only "speech.audio.done" (no delta)
        // followed by [DONE]. After two events we should detect empty.
        let events = vec![
            LlmResponse {
                speech_stream_event: Some(json!({"type": "speech.audio.done"})),
                ..Default::default()
            },
            LlmResponse {
                object: DONE_OBJECT.to_string(),
                ..Default::default()
            },
        ];
        let outcome = run_pre_read(true, events).ok();
        assert!(matches!(outcome, Some(PreReadOutcome::Empty)));
    }

    #[test]
    fn pre_read_safe_default_after_three_non_terminating_events() {
        // Three events, none with content, none with finish_reason/[DONE] ->
        // Continue (Go safe default, lines 211-214).
        let events = vec![
            LlmResponse::default(),
            LlmResponse::default(),
            LlmResponse::default(),
        ];
        let outcome = run_pre_read(true, events).ok();
        match outcome {
            Some(PreReadOutcome::Continue { buffered }) => {
                assert_eq!(buffered.len(), 3);
            }
            _ => panic!("expected Continue (safe default)"),
        }
    }

    #[test]
    fn pre_read_returns_continue_when_content_appears_on_event_3() {
        // First two events empty, third has content -> Continue with 3 buffered.
        let events = vec![
            LlmResponse::default(),
            LlmResponse::default(),
            resp_with_message(msg_with_text("late content")),
        ];
        let outcome = run_pre_read(true, events).ok();
        match outcome {
            Some(PreReadOutcome::Continue { buffered }) => {
                assert_eq!(buffered.len(), 3);
            }
            _ => panic!("expected Continue"),
        }
    }

    #[test]
    fn pre_read_clean_end_before_limit_continues_with_buffered() {
        // Stream ends after 1 event with no content/terminator -> Continue
        // (treated as non-empty per Go safe default for short streams).
        let events = vec![LlmResponse::default()];
        let outcome = run_pre_read(true, events).ok();
        assert!(matches!(
            outcome,
            Some(PreReadOutcome::Continue { buffered }) if buffered.len() == 1
        ));
    }

    #[test]
    fn pre_read_streams_with_no_events_continue_empty_buffered() {
        // No events at all -> Continue with empty buffer (Go lines 215-216).
        let outcome = run_pre_read(true, vec![]).ok();
        assert!(matches!(
            outcome,
            Some(PreReadOutcome::Continue { buffered }) if buffered.is_empty()
        ));
    }

    // -- Error sentinels ----------------------------------------------------

    #[test]
    fn err_empty_response_message_shape() {
        // Just confirm the sentinel stringifies the way Go's errors.New does.
        let err = ErrEmptyResponse;
        assert_eq!(format!("{err}"), "empty response detected");
    }
}
