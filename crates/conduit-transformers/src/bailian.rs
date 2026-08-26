//! Bailian (Alibaba Cloud) OpenAI-compatible wrapper (RUST-P7-008 S11/S12,
//! round 2).
//!
//! Mirrors Go `conduit/llm/transformer/bailian/outbound.go`. Bailian is a
//! thin OpenAI-compatible wrapper whose only request-side delta is the
//! `mergeConsecutiveToolCallMessages` normalization: Bailian's API rejects
//! consecutive assistant messages that each carry tool_calls, so the wrapper
//! merges runs of such messages into a single assistant message whose
//! `tool_calls` is the concatenation. Everything else (URL normalization,
//! bearer auth, JSON headers, request-type gating) is handled by the
//! wrapped OpenAI transformer / shared base.
//!
//! The Go wrapper *also* installs a `bailianStreamFilter` over the response
//! stream (outbound.go:143-155, stream_filter.go). That streaming
//! normalization is a separate concern from the request-shape wrapper pattern
//! established here and the existing `openai_stream.rs` doesn't yet expose a
//! stream-filter abstraction — left as a documented TODO.
//!
//! Go tests mirrored: `bailian/outbound_test.go` —
//! `TestBailianTransformRequest_MergeConsecutiveToolCalls`.

use crate::openai_compatible::OpenAiCompatibleConfig;

/// Bailian provider configuration. Mirrors Go `bailian.Config`
/// (outbound.go:16-20) — the OpenAI-compatible shape (no extra fields).
pub type Config = OpenAiCompatibleConfig;

/// Merge consecutive *mergeable* assistant tool-call messages into a single
/// assistant message, concatenating their `tool_calls` vectors in order.
/// Mirrors Go `mergeConsecutiveToolCallMessages` (outbound.go:68-113).
///
/// ## Merge rules (Go `isMergeableToolCallMessage`, outbound.go:115-133)
///
/// A message is mergeable iff:
/// * `role` is `"assistant"` (case-insensitive — Go uses `strings.EqualFold`),
/// * `tool_calls` is non-empty,
/// * none of the disqualifying fields are set: `tool_call_id`, `name`,
///   `reasoning_content`, `reasoning_signature`, `redacted_reasoning_content`,
///   `cache_control`, `refusal`, `message_index`, `tool_call_name`,
///   `tool_call_is_error`. In Rust the named struct fields are checked
///   directly (`tool_call_id`, `name`); the Go-only fields live in the
///   `extra` map and are checked by key.
/// * content is "empty" (Go `isEmptyMessageContent`: `Content == nil ||
///   Content == ""` AND `MultipleContent` is empty).
///
/// When a run of mergeable messages is found, they collapse into the first
/// one (its `tool_calls` is extended with every subsequent run member's
/// `tool_calls`). Non-mergeable messages pass through unchanged. Returns the
/// (possibly-rebuilt) message vector; when no merge occurred, the input is
/// returned as-is to mirror the Go short-circuit (`if !changed { return req }`).
pub fn merge_consecutive_tool_call_messages(
    messages: Vec<conduit_llm::ChatMessage>,
) -> Vec<conduit_llm::ChatMessage> {
    if messages.len() < 2 {
        return messages;
    }

    let mut out: Vec<conduit_llm::ChatMessage> = Vec::with_capacity(messages.len());
    let mut pending: Option<conduit_llm::ChatMessage> = None;
    let mut changed = false;

    for msg in messages {
        if is_mergeable_tool_call_message(&msg) {
            match pending.as_mut() {
                Some(p) => {
                    p.tool_calls.extend(msg.tool_calls);
                    changed = true;
                }
                None => {
                    pending = Some(msg);
                }
            }
            continue;
        }
        if let Some(p) = pending.take() {
            out.push(p);
        }
        out.push(msg);
    }
    if let Some(p) = pending {
        out.push(p);
    }

    if !changed {
        // Mirror Go `if !changed { return req }`: hand back the original
        // vector so callers can detect "no rewrite needed" by pointer/length
        // equality if they wish. We can't return the literal input (it was
        // consumed), so we return the rebuilt vector which is content-equal.
        return out;
    }
    out
}

/// Whether a message is a mergeable assistant tool-call message. Mirrors Go
/// `isMergeableToolCallMessage` (outbound.go:115-133). See the doc on
/// [`merge_consecutive_tool_call_messages`] for the full rule.
pub fn is_mergeable_tool_call_message(msg: &conduit_llm::ChatMessage) -> bool {
    if !msg.role.eq_ignore_ascii_case("assistant") {
        return false;
    }
    if msg.tool_calls.is_empty() {
        return false;
    }
    // Disqualifying named fields.
    if msg.tool_call_id.is_some() || msg.name.is_some() {
        return false;
    }
    // Disqualifying Go-only fields that live in the Rust `extra` map.
    const DISQUALIFYING_EXTRA_KEYS: &[&str] = &[
        "refusal",
        "message_index",
        "tool_call_name",
        "tool_call_is_error",
        "reasoning_content",
        "reasoning_signature",
        "redacted_reasoning_content",
        "cache_control",
    ];
    for key in DISQUALIFYING_EXTRA_KEYS {
        if msg.extra.get(*key).is_some_and(|v| !v.is_null()) {
            return false;
        }
    }
    is_empty_message_content(&msg.content)
}

/// Whether a message's content counts as "empty" for the merge rule. Mirrors
/// Go `isEmptyMessageContent` (outbound.go:135-141): `Content == nil ||
/// Content == ""` AND `MultipleContent` is empty.
///
/// In the Rust [`conduit_llm::MessageContent`] enum:
/// * `None` → empty,
/// * `Text("")` → empty, any other `Text` → non-empty,
/// * `Parts(vec![])` → empty, any other `Parts` → non-empty,
/// * `Json(_)` → non-empty (no equivalent in Go's `MessageContent`, and a
///   JSON value is never "empty").
pub fn is_empty_message_content(content: &Option<conduit_llm::MessageContent>) -> bool {
    match content {
        None => true,
        Some(conduit_llm::MessageContent::Text(s)) => s.is_empty(),
        Some(conduit_llm::MessageContent::Parts(parts)) => parts.is_empty(),
        Some(conduit_llm::MessageContent::Json(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{ChatMessage, MessageContent, ToolCall};
    use serde_json::json;

    // ---- helpers ----

    fn assistant_with_tool_calls(call_ids: &[&str]) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            name: None,
            content: None,
            tool_calls: call_ids
                .iter()
                .map(|id| ToolCall {
                    id: Some(id.to_string()),
                    call_type: "function".to_string(),
                    function: json!({"name": format!("tool_{id}"), "arguments": "{}"}),
                    extra: Default::default(),
                })
                .collect(),
            tool_call_id: None,
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

    fn tool_result_msg(call_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            name: None,
            content: Some(MessageContent::Text(content.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.to_string()),
            extra: Default::default(),
        }
    }

    // =======================================================================
    // TestBailianTransformRequest_MergeConsecutiveToolCalls
    // (mirrors bailian/outbound_test.go:15-90)
    // =======================================================================
    #[test]
    fn merge_consecutive_tool_call_messages_mirrors_go_test() {
        // Build the exact message sequence from the Go test:
        // user, assistant[call_1], assistant[call_2], tool(call_1, out1),
        // tool(call_2, out2) → expected merged length 4 with the two
        // assistant messages collapsed into one carrying both calls.
        let messages = vec![
            user_msg("hi"),
            assistant_with_tool_calls(&["call_1"]),
            assistant_with_tool_calls(&["call_2"]),
            tool_result_msg("call_1", "out1"),
            tool_result_msg("call_2", "out2"),
        ];

        let merged = merge_consecutive_tool_call_messages(messages);

        assert_eq!(merged.len(), 4, "merged message count");
        assert_eq!(merged[0].role, "user");
        assert_eq!(merged[1].role, "assistant");
        assert_eq!(
            merged[1].tool_calls.len(),
            2,
            "assistant should carry both tool calls"
        );
        assert_eq!(merged[1].tool_calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(merged[1].tool_calls[1].id.as_deref(), Some("call_2"));
        assert_eq!(merged[2].role, "tool");
        assert_eq!(merged[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(merged[3].role, "tool");
        assert_eq!(merged[3].tool_call_id.as_deref(), Some("call_2"));
    }

    // =======================================================================
    // is_mergeable_tool_call_message rule coverage (outbound.go:115-133)
    // =======================================================================
    #[test]
    fn mergeable_for_plain_assistant_with_tool_calls_and_empty_content() {
        let msg = assistant_with_tool_calls(&["c1"]);
        assert!(is_mergeable_tool_call_message(&msg));
    }

    #[test]
    fn not_mergeable_for_non_assistant_role() {
        let mut msg = assistant_with_tool_calls(&["c1"]);
        msg.role = "user".to_string();
        assert!(!is_mergeable_tool_call_message(&msg));
    }

    #[test]
    fn not_mergeable_when_role_is_assistant_but_case_varies_still_mergeable() {
        // mirrors strings.EqualFold — "Assistant" should still match.
        let mut msg = assistant_with_tool_calls(&["c1"]);
        msg.role = "Assistant".to_string();
        assert!(is_mergeable_tool_call_message(&msg));
    }

    #[test]
    fn not_mergeable_when_tool_calls_empty() {
        let mut msg = assistant_with_tool_calls(&["c1"]);
        msg.tool_calls.clear();
        assert!(!is_mergeable_tool_call_message(&msg));
    }

    #[test]
    fn not_mergeable_when_tool_call_id_set() {
        let mut msg = assistant_with_tool_calls(&["c1"]);
        msg.tool_call_id = Some("call_x".to_string());
        assert!(!is_mergeable_tool_call_message(&msg));
    }

    #[test]
    fn not_mergeable_when_name_set() {
        let mut msg = assistant_with_tool_calls(&["c1"]);
        msg.name = Some("named".to_string());
        assert!(!is_mergeable_tool_call_message(&msg));
    }

    #[test]
    fn not_mergeable_when_disqualifying_extra_key_present() {
        for key in [
            "refusal",
            "message_index",
            "tool_call_name",
            "tool_call_is_error",
            "reasoning_content",
            "reasoning_signature",
            "redacted_reasoning_content",
            "cache_control",
        ] {
            let mut msg = assistant_with_tool_calls(&["c1"]);
            msg.extra.insert(key.to_string(), json!("value"));
            assert!(
                !is_mergeable_tool_call_message(&msg),
                "should not be mergeable when {key} is set"
            );
        }
    }

    #[test]
    fn mergeable_when_disqualifying_extra_key_present_but_null() {
        // Go's nil check is "field != nil"; a JSON null in extra should not
        // disqualify (mirrors how the Rust helper uses !v.is_null()).
        let mut msg = assistant_with_tool_calls(&["c1"]);
        msg.extra
            .insert("reasoning_content".to_string(), serde_json::Value::Null);
        assert!(is_mergeable_tool_call_message(&msg));
    }

    #[test]
    fn not_mergeable_when_content_is_non_empty_text() {
        let mut msg = assistant_with_tool_calls(&["c1"]);
        msg.content = Some(MessageContent::Text("hello".to_string()));
        assert!(!is_mergeable_tool_call_message(&msg));
    }

    #[test]
    fn mergeable_when_content_is_empty_text() {
        let mut msg = assistant_with_tool_calls(&["c1"]);
        msg.content = Some(MessageContent::Text(String::new()));
        assert!(is_mergeable_tool_call_message(&msg));
    }

    #[test]
    fn not_mergeable_when_content_is_non_empty_parts() {
        let mut msg = assistant_with_tool_calls(&["c1"]);
        msg.content = Some(MessageContent::Parts(vec![conduit_llm::ContentPart {
            part_type: "text".to_string(),
            text: Some("hi".to_string()),
            image_url: None,
            input_audio: None,
            extra: Default::default(),
        }]));
        assert!(!is_mergeable_tool_call_message(&msg));
    }

    #[test]
    fn mergeable_when_content_is_empty_parts() {
        let mut msg = assistant_with_tool_calls(&["c1"]);
        msg.content = Some(MessageContent::Parts(Vec::new()));
        assert!(is_mergeable_tool_call_message(&msg));
    }

    // =======================================================================
    // merge edge cases (outbound.go:68-113)
    // =======================================================================
    #[test]
    fn single_message_returned_unchanged() {
        let messages = vec![user_msg("hi")];
        let merged = merge_consecutive_tool_call_messages(messages);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn empty_input_returns_empty() {
        let merged = merge_consecutive_tool_call_messages(Vec::new());
        assert!(merged.is_empty());
    }

    #[test]
    fn no_merge_when_only_one_tool_call_assistant() {
        // A single assistant-with-tool-calls message followed by non-assistant
        // messages: nothing to merge with, length unchanged.
        let messages = vec![
            user_msg("hi"),
            assistant_with_tool_calls(&["c1"]),
            tool_result_msg("c1", "out"),
        ];
        let merged = merge_consecutive_tool_call_messages(messages);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[1].tool_calls.len(), 1);
    }

    #[test]
    fn merge_run_of_three_assistant_tool_call_messages() {
        let messages = vec![
            user_msg("hi"),
            assistant_with_tool_calls(&["c1"]),
            assistant_with_tool_calls(&["c2"]),
            assistant_with_tool_calls(&["c3"]),
            tool_result_msg("c1", "o1"),
            tool_result_msg("c2", "o2"),
            tool_result_msg("c3", "o3"),
        ];
        let merged = merge_consecutive_tool_call_messages(messages);
        assert_eq!(merged.len(), 5);
        assert_eq!(merged[1].role, "assistant");
        assert_eq!(merged[1].tool_calls.len(), 3);
        assert_eq!(merged[1].tool_calls[0].id.as_deref(), Some("c1"));
        assert_eq!(merged[1].tool_calls[1].id.as_deref(), Some("c2"));
        assert_eq!(merged[1].tool_calls[2].id.as_deref(), Some("c3"));
    }

    #[test]
    fn merge_is_interrupted_by_non_mergeable_message() {
        // assistant(c1), assistant(c2), user, assistant(c3), assistant(c4)
        // → two separate runs: [c1,c2] and [c3,c4].
        let messages = vec![
            assistant_with_tool_calls(&["c1"]),
            assistant_with_tool_calls(&["c2"]),
            user_msg("interrupt"),
            assistant_with_tool_calls(&["c3"]),
            assistant_with_tool_calls(&["c4"]),
        ];
        let merged = merge_consecutive_tool_call_messages(messages);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].role, "assistant");
        assert_eq!(merged[0].tool_calls.len(), 2);
        assert_eq!(merged[1].role, "user");
        assert_eq!(merged[2].role, "assistant");
        assert_eq!(merged[2].tool_calls.len(), 2);
    }

    #[test]
    fn trailing_run_of_tool_call_messages_is_merged() {
        // The Go loop flushes `pending` after the for-loop; verify the final
        // pending message is emitted.
        let messages = vec![
            user_msg("hi"),
            assistant_with_tool_calls(&["c1"]),
            assistant_with_tool_calls(&["c2"]),
        ];
        let merged = merge_consecutive_tool_call_messages(messages);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[1].tool_calls.len(), 2);
    }

    // =======================================================================
    // is_empty_message_content direct coverage (outbound.go:135-141)
    // =======================================================================
    #[test]
    fn is_empty_message_content_handles_all_variants() {
        assert!(is_empty_message_content(&None));
        assert!(is_empty_message_content(&Some(MessageContent::Text(
            String::new()
        ))));
        assert!(!is_empty_message_content(&Some(MessageContent::Text(
            "x".to_string()
        ))));
        assert!(is_empty_message_content(&Some(MessageContent::Parts(
            Vec::new()
        ))));
        assert!(!is_empty_message_content(&Some(MessageContent::Parts(
            vec![conduit_llm::ContentPart::default(),]
        ))));
        assert!(!is_empty_message_content(&Some(MessageContent::Json(
            json!({})
        ))));
    }
}
