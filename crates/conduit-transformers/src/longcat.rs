//! Longcat (Meituan) OpenAI-compatible wrapper (RUST-P7-008 S11/S12, round 3).
//!
//! Mirrors Go `conduit/llm/transformer/longcat/outbound.go` +
//! `longcat/model.go`. Longcat is a thin OpenAI-compatible wrapper whose only
//! request-side delta is that **every message's content must marshal as an
//! array of content parts**, never a plain string. Longcat models (e.g.
//! LongCat-Flash-Omni) reject plain string content with a "json format error",
//! so the wrapper rewrites each message's content to the multiple-content
//! (array) shape before delegating to the OpenAI transformer.
//!
//! Everything else (URL normalization, bearer auth, JSON headers, request-type
//! gating) comes from [`crate::openai_compatible`].
//!
//! Go tests mirrored: `longcat/outbound_test.go` —
//! `TestOutboundTransformer_TransformRequest_ForceMultipleContent`,
//! `TestOutboundTransformer_TransformRequest_MultipleContentPreserved`.

use crate::openai_compatible::OpenAiCompatibleConfig;
use conduit_llm::{ChatMessage, MessageContent};
use serde_json::{Value, json};

/// Longcat provider configuration. Mirrors Go `longcat.Config`
/// (outbound.go:32-35) — the OpenAI-compatible shape.
pub type Config = OpenAiCompatibleConfig;

/// Ensure every message in `messages` has non-empty content, mirroring Go
/// outbound.go:64-69:
///
/// ```go
/// for i := range chatReq.Messages {
///     if chatReq.Messages[i].Content.Content == nil && len(chatReq.Messages[i].Content.MultipleContent) == 0 {
///         chatReq.Messages[i].Content.Content = lo.ToPtr("")
///     }
/// }
/// ```
///
/// Messages whose content is `None` or empty-parts get a `Text("")` content
/// so they survive the array conversion without null gaps. Returns the count
/// of messages filled (handy for tests).
pub fn ensure_non_empty_content(messages: &mut [ChatMessage]) -> usize {
    let mut filled = 0usize;
    for msg in messages.iter_mut() {
        let empty = match &msg.content {
            None => true,
            Some(MessageContent::Text(_)) => false, // Go treats any *string as non-nil
            Some(MessageContent::Parts(parts)) => parts.is_empty(),
            Some(MessageContent::Json(_)) => false,
        };
        if empty {
            msg.content = Some(MessageContent::Text(String::new()));
            filled += 1;
        }
    }
    filled
}

/// Convert a message's content into the Longcat multiple-content (array)
/// shape. Mirrors Go `MessageContent.MarshalJSON` (model.go:31-45):
///
/// * if the content is already `Parts(parts)` with `!parts.is_empty()` →
///   keep the array as-is,
/// * if the content is `Text(s)` → wrap as `[{"type":"text","text":s}]`,
/// * otherwise (None / empty parts) → `[{"type":"text","text":""}]`.
///
/// Returns the JSON array value the Longcat API expects.
pub fn content_as_array(content: &Option<MessageContent>) -> Value {
    match content {
        Some(MessageContent::Parts(parts)) if !parts.is_empty() => {
            serde_json::to_value(parts).unwrap_or_else(|_| json!([{"type":"text","text":""}]))
        }
        Some(MessageContent::Text(s)) => json!([{"type":"text","text":s}]),
        _ => json!([{"type":"text","text":""}]),
    }
}

/// Rewrite a message's content in place so that it serializes as the Longcat
/// array shape. This is the per-message form of the body rewrite performed by
/// Go `OutboundTransformer.TransformRequest` (outbound.go:76-94) — there the
/// request body is round-tripped through a longcat-specific `Message` whose
/// `MessageContent.MarshalJSON` always emits an array. Here we work on the
/// unified [`ChatMessage`] directly: when the content would otherwise serialize
/// to a plain string (or null), we replace it with a one-element `Parts` array
/// so the standard OpenAI serialization produces the array Longcat expects.
///
/// Returns `true` if the content was rewritten.
pub fn force_array_content(msg: &mut ChatMessage) -> bool {
    let already_array = matches!(
        msg.content,
        Some(MessageContent::Parts(ref p)) if !p.is_empty()
    );
    if already_array {
        return false;
    }
    let text = match msg.content.take() {
        Some(MessageContent::Text(s)) => s,
        _ => String::new(),
    };
    msg.content = Some(MessageContent::Parts(vec![conduit_llm::ContentPart {
        part_type: "text".to_string(),
        text: Some(text),
        image_url: None,
        input_audio: None,
        extra: Default::default(),
    }]));
    true
}

/// Apply [`force_array_content`] to every message. Mirrors the loop body of
/// Go `OutboundTransformer.TransformRequest` (outbound.go:84-89, where each
/// OpenAI `Message` is re-wrapped as a longcat `Message` whose content always
/// serializes as an array). Returns the count of messages rewritten.
pub fn force_all_array_content(messages: &mut [ChatMessage]) -> usize {
    let mut rewrites = 0usize;
    for msg in messages.iter_mut() {
        if force_array_content(msg) {
            rewrites += 1;
        }
    }
    rewrites
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::{ContentPart, MessageContent};

    // ---- helpers ----

    fn user_msg_with_content(content: Option<MessageContent>) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            name: None,
            content,
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: Default::default(),
        }
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_ForceMultipleContent
    // (mirrors longcat/outbound_test.go:14-71)
    // =======================================================================
    #[test]
    fn plain_string_content_is_converted_to_array() {
        // mirrors "plain string content is converted to array" with wantText "Hello!"
        let mut msg = user_msg_with_content(Some(MessageContent::Text("Hello!".to_string())));
        assert!(force_array_content(&mut msg));
        match &msg.content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].part_type, "text");
                assert_eq!(parts[0].text.as_deref(), Some("Hello!"));
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    #[test]
    fn empty_string_content_is_converted_to_array() {
        // mirrors "empty string content is converted to array" with wantText ""
        let mut msg = user_msg_with_content(Some(MessageContent::Text(String::new())));
        assert!(force_array_content(&mut msg));
        match &msg.content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].text.as_deref(), Some(""));
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    #[test]
    fn none_content_gets_empty_text_array() {
        // mirrors "nil content gets empty text array" with wantText ""
        let mut msg = user_msg_with_content(None);
        assert!(force_array_content(&mut msg));
        match &msg.content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].text.as_deref(), Some(""));
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    // =======================================================================
    // TestOutboundTransformer_TransformRequest_MultipleContentPreserved
    // (mirrors longcat/outbound_test.go:73-106)
    // =======================================================================
    #[test]
    fn multiple_content_is_preserved_unchanged() {
        // mirrors the test: a two-part (text + image_url) content stays as-is.
        let parts = vec![
            ContentPart {
                part_type: "text".to_string(),
                text: Some("What is this?".to_string()),
                image_url: None,
                input_audio: None,
                extra: Default::default(),
            },
            ContentPart {
                part_type: "image_url".to_string(),
                text: None,
                image_url: Some(json!({"url": "https://example.com/img.png"})),
                input_audio: None,
                extra: Default::default(),
            },
        ];
        let mut msg = user_msg_with_content(Some(MessageContent::Parts(parts.clone())));
        assert!(!force_array_content(&mut msg), "should not rewrite");
        match &msg.content {
            Some(MessageContent::Parts(kept)) => assert_eq!(kept.len(), 2),
            other => panic!("expected Parts preserved, got {other:?}"),
        }
    }

    // =======================================================================
    // content_as_array direct coverage (mirrors Go MessageContent.MarshalJSON)
    // =======================================================================
    #[test]
    fn content_as_array_handles_text() {
        let v = content_as_array(&Some(MessageContent::Text("hi".to_string())));
        assert_eq!(v, json!([{"type":"text","text":"hi"}]));
    }

    #[test]
    fn content_as_array_handles_empty_text() {
        let v = content_as_array(&Some(MessageContent::Text(String::new())));
        assert_eq!(v, json!([{"type":"text","text":""}]));
    }

    #[test]
    fn content_as_array_handles_none() {
        let v = content_as_array(&None);
        assert_eq!(v, json!([{"type":"text","text":""}]));
    }

    #[test]
    fn content_as_array_preserves_non_empty_parts() {
        let parts = vec![ContentPart {
            part_type: "text".to_string(),
            text: Some("x".to_string()),
            image_url: None,
            input_audio: None,
            extra: Default::default(),
        }];
        let v = content_as_array(&Some(MessageContent::Parts(parts)));
        assert_eq!(v, json!([{"type":"text","text":"x"}]));
    }

    #[test]
    fn content_as_array_treats_empty_parts_as_empty_text() {
        // mirrors the Go MarshalJSON fallback when MultipleContent is empty
        let v = content_as_array(&Some(MessageContent::Parts(Vec::new())));
        assert_eq!(v, json!([{"type":"text","text":""}]));
    }

    // =======================================================================
    // ensure_non_empty_content (mirrors Go outbound.go:64-69)
    // =======================================================================
    #[test]
    fn ensure_non_empty_content_fills_none_messages() {
        let mut msgs = vec![
            user_msg_with_content(None),
            user_msg_with_content(Some(MessageContent::Text("hi".to_string()))),
            user_msg_with_content(Some(MessageContent::Parts(Vec::new()))),
        ];
        let filled = ensure_non_empty_content(&mut msgs);
        assert_eq!(filled, 2);
        assert!(matches!(msgs[0].content, Some(MessageContent::Text(_))));
        assert!(matches!(msgs[2].content, Some(MessageContent::Text(_))));
    }

    #[test]
    fn ensure_non_empty_content_leaves_non_empty_alone() {
        let mut msgs = vec![
            user_msg_with_content(Some(MessageContent::Text("a".to_string()))),
            user_msg_with_content(Some(MessageContent::Parts(vec![ContentPart::default()]))),
        ];
        assert_eq!(ensure_non_empty_content(&mut msgs), 0);
    }

    // =======================================================================
    // force_all_array_content loop coverage (mirrors Go outbound.go:84-89)
    // =======================================================================
    #[test]
    fn force_all_array_content_rewrites_mixed_messages() {
        let mut msgs = vec![
            user_msg_with_content(Some(MessageContent::Text("a".to_string()))),
            user_msg_with_content(Some(MessageContent::Parts(vec![ContentPart {
                part_type: "text".to_string(),
                text: Some("b".to_string()),
                image_url: None,
                input_audio: None,
                extra: Default::default(),
            }]))),
            user_msg_with_content(None),
        ];
        let rewrites = force_all_array_content(&mut msgs);
        assert_eq!(rewrites, 2);
        // all messages now hold Parts
        for msg in &msgs {
            assert!(matches!(msg.content, Some(MessageContent::Parts(_))));
        }
    }
}
