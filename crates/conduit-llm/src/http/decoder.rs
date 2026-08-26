//! Response body decoder: selects JSON / SSE / Binary by content type, parses
//! SSE frames, and masks sensitive headers. Mirrors the Go
//! `llm/httpclient/decoder.go` semantics plus the `MaskSensitiveHeaders`
//! helper from `llm/httpclient/utils.go`.

use std::fmt;

use serde_json::Value;
use thiserror::Error;

use crate::model::{HeaderMap, StreamEvent};

/// Maximum SSE event size (32 MiB), matching the Go default.
pub const MAX_SSE_EVENT_SIZE: usize = 32 * 1024 * 1024;
const MASKED_HEADER_VALUE: &str = "[redacted]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyDecoder {
    Json,
    Sse,
    Binary,
}

impl BodyDecoder {
    pub fn for_content_type(content_type: Option<&str>) -> Self {
        let Some(media_type) = normalized_media_type(content_type) else {
            return Self::Binary;
        };

        if media_type == "text/event-stream" {
            return Self::Sse;
        }

        if is_binary_media_type(&media_type) {
            return Self::Binary;
        }

        if is_json_media_type(&media_type) {
            return Self::Json;
        }

        Self::Binary
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecodedBody {
    Json(Value),
    Sse(SseParseResult),
    Binary(Vec<u8>),
}

#[derive(Debug, Error)]
pub enum HttpDecodeError {
    #[error("failed to decode JSON response body")]
    Json(#[from] serde_json::Error),
    #[error("SSE event exceeded maximum size of {max} bytes")]
    SseEventTooLarge { max: usize },
}

pub fn decode_response_body(
    content_type: Option<&str>,
    body: &[u8],
) -> Result<DecodedBody, HttpDecodeError> {
    match BodyDecoder::for_content_type(content_type) {
        BodyDecoder::Json => Ok(DecodedBody::Json(serde_json::from_slice(body)?)),
        BodyDecoder::Sse => Ok(DecodedBody::Sse(parse_sse_frames(body)?)),
        BodyDecoder::Binary => Ok(DecodedBody::Binary(body.to_vec())),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseFrame {
    pub last_event_id: Option<String>,
    pub event_type: Option<String>,
    pub data: String,
    pub retry: Option<u64>,
}

impl From<SseFrame> for StreamEvent {
    fn from(frame: SseFrame) -> Self {
        let size = frame.data.len();

        Self {
            last_event_id: frame.last_event_id,
            event_type: frame.event_type,
            data: Some(frame.data),
            size: Some(size),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseParseResult {
    pub frames: Vec<SseFrame>,
    pub incomplete: Vec<u8>,
}

pub fn parse_sse_frames(input: &[u8]) -> Result<SseParseResult, HttpDecodeError> {
    parse_sse_frames_with_max_event_size(input, MAX_SSE_EVENT_SIZE)
}

pub fn parse_sse_frames_with_max_event_size(
    input: &[u8],
    max_event_size: usize,
) -> Result<SseParseResult, HttpDecodeError> {
    let mut frames = Vec::new();
    let mut cursor = 0;
    let mut event_start = 0;
    let mut pending = PendingSseFrame::default();

    while let Some((line, next_cursor)) = read_sse_line(input, cursor) {
        if next_cursor - event_start > max_event_size {
            return Err(HttpDecodeError::SseEventTooLarge {
                max: max_event_size,
            });
        }

        cursor = next_cursor;

        if line.is_empty() {
            if let Some(frame) = pending.take_frame() {
                frames.push(frame);
            }
            event_start = cursor;
            continue;
        }

        if line.first() == Some(&b':') {
            continue;
        }

        pending.apply_line(line);
    }

    if input.len() - event_start > max_event_size {
        return Err(HttpDecodeError::SseEventTooLarge {
            max: max_event_size,
        });
    }

    // A final event without a blank line is not dispatched; callers can prepend it to the next read.
    let incomplete = if event_start < input.len() {
        input[event_start..].to_vec()
    } else {
        Vec::new()
    };

    Ok(SseParseResult { frames, incomplete })
}

#[derive(Debug, Default)]
struct PendingSseFrame {
    last_event_id: Option<String>,
    event_type: Option<String>,
    data_lines: Vec<String>,
    retry: Option<u64>,
}

impl PendingSseFrame {
    fn apply_line(&mut self, line: &[u8]) {
        let (field, value) = split_sse_field(line);

        match field {
            b"data" => self.data_lines.push(sse_value_to_string(value)),
            b"event" => self.event_type = Some(sse_value_to_string(value)),
            b"id" => {
                if !value.contains(&0) {
                    self.last_event_id = Some(sse_value_to_string(value));
                }
            }
            b"retry" => {
                if let Ok(retry) = sse_value_to_string(value).parse::<u64>() {
                    self.retry = Some(retry);
                }
            }
            _ => {}
        }
    }

    fn take_frame(&mut self) -> Option<SseFrame> {
        if self.data_lines.is_empty()
            && self.last_event_id.is_none()
            && self.event_type.is_none()
            && self.retry.is_none()
        {
            return None;
        }

        let frame = SseFrame {
            last_event_id: self.last_event_id.take(),
            event_type: self.event_type.take(),
            data: self.data_lines.join("\n"),
            retry: self.retry.take(),
        };
        self.data_lines.clear();

        Some(frame)
    }
}

pub fn mask_sensitive_headers(headers: &HeaderMap) -> HeaderMap {
    MaskSensitiveHeaders::new(headers).masked()
}

#[derive(Debug, Clone, Copy)]
pub struct MaskSensitiveHeaders<'a> {
    headers: &'a HeaderMap,
}

impl<'a> MaskSensitiveHeaders<'a> {
    pub fn new(headers: &'a HeaderMap) -> Self {
        Self { headers }
    }

    pub fn masked(self) -> HeaderMap {
        self.headers
            .iter()
            .map(|(key, value)| {
                let masked_value = if is_sensitive_header(key) {
                    MASKED_HEADER_VALUE.to_string()
                } else {
                    value.clone()
                };
                (key.clone(), masked_value)
            })
            .collect()
    }
}

impl fmt::Display for MaskSensitiveHeaders<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_map().entries(self.masked().iter()).finish()
    }
}

fn normalized_media_type(content_type: Option<&str>) -> Option<String> {
    content_type?
        .split(';')
        .next()
        .map(str::trim)
        .filter(|media_type| !media_type.is_empty())
        .map(str::to_ascii_lowercase)
}

fn is_json_media_type(media_type: &str) -> bool {
    media_type == "application/json" || media_type == "text/json" || media_type.ends_with("+json")
}

fn is_binary_media_type(media_type: &str) -> bool {
    matches!(
        media_type,
        "application/octet-stream"
            | "application/pdf"
            | "application/zip"
            | "application/gzip"
            | "application/x-tar"
            | "application/wasm"
            | "application/x-protobuf"
            | "application/protobuf"
            | "application/msgpack"
    ) || media_type.starts_with("image/")
        || media_type.starts_with("audio/")
        || media_type.starts_with("video/")
        || media_type.starts_with("font/")
}

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "x-api-key" | "x-goog-api-key" | "cookie"
    )
}

fn read_sse_line(input: &[u8], cursor: usize) -> Option<(&[u8], usize)> {
    let mut index = cursor;

    while index < input.len() {
        match input[index] {
            b'\n' => return Some((&input[cursor..index], index + 1)),
            b'\r' if index + 1 < input.len() && input[index + 1] == b'\n' => {
                return Some((&input[cursor..index], index + 2));
            }
            b'\r' => return Some((&input[cursor..index], index + 1)),
            _ => index += 1,
        }
    }

    None
}

fn split_sse_field(line: &[u8]) -> (&[u8], &[u8]) {
    let Some(colon_index) = line.iter().position(|byte| *byte == b':') else {
        return (line, b"");
    };

    let mut value = &line[colon_index + 1..];
    if value.first() == Some(&b' ') {
        value = &value[1..];
    }

    (&line[..colon_index], value)
}

fn sse_value_to_string(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_selector_uses_content_type_media_type() {
        assert_eq!(
            BodyDecoder::for_content_type(Some("application/json; charset=utf-8")),
            BodyDecoder::Json
        );
        assert_eq!(
            BodyDecoder::for_content_type(Some("application/problem+json")),
            BodyDecoder::Json
        );
        assert_eq!(
            BodyDecoder::for_content_type(Some("text/event-stream")),
            BodyDecoder::Sse
        );
        assert_eq!(
            BodyDecoder::for_content_type(Some("IMAGE/PNG")),
            BodyDecoder::Binary
        );
        assert_eq!(BodyDecoder::for_content_type(None), BodyDecoder::Binary);
    }

    #[test]
    fn binary_body_does_not_enter_json_parser() -> Result<(), Box<dyn std::error::Error>> {
        let decoded = decode_response_body(Some("application/octet-stream"), b"{not valid json")?;

        assert_eq!(decoded, DecodedBody::Binary(b"{not valid json".to_vec()));
        Ok(())
    }

    #[test]
    fn sse_parser_handles_crlf_comments_and_multiline_data()
    -> Result<(), Box<dyn std::error::Error>> {
        let parsed = parse_sse_frames(
            b": keep-alive\r\nid: 42\r\nevent: message.delta\r\ndata: hello\r\ndata: world\r\n\r\n",
        )?;

        assert_eq!(parsed.incomplete, b"");
        assert_eq!(parsed.frames.len(), 1);
        assert_eq!(parsed.frames[0].last_event_id.as_deref(), Some("42"));
        assert_eq!(
            parsed.frames[0].event_type.as_deref(),
            Some("message.delta")
        );
        assert_eq!(parsed.frames[0].data, "hello\nworld");
        Ok(())
    }

    #[test]
    fn sse_parser_preserves_empty_data_event() -> Result<(), Box<dyn std::error::Error>> {
        let parsed = parse_sse_frames(b"event: ping\ndata:\n\n")?;

        assert_eq!(parsed.frames.len(), 1);
        assert_eq!(parsed.frames[0].event_type.as_deref(), Some("ping"));
        assert_eq!(parsed.frames[0].data, "");
        Ok(())
    }

    #[test]
    fn sse_parser_keeps_partial_eof_as_incomplete() -> Result<(), Box<dyn std::error::Error>> {
        let parsed = parse_sse_frames(b"data: complete\n\ndata: partial")?;

        assert_eq!(parsed.frames.len(), 1);
        assert_eq!(parsed.frames[0].data, "complete");
        assert_eq!(parsed.incomplete, b"data: partial");
        Ok(())
    }

    #[test]
    fn sse_parser_rejects_oversized_event() {
        let result = parse_sse_frames_with_max_event_size(b"data: too large\n\n", 8);

        assert!(matches!(
            result,
            Err(HttpDecodeError::SseEventTooLarge { max: 8 })
        ));
    }

    #[test]
    fn masks_sensitive_headers_case_insensitively() {
        let headers = HeaderMap::from([
            ("Authorization".to_string(), "Bearer secret".to_string()),
            ("X-API-Key".to_string(), "secret".to_string()),
            ("X-Goog-Api-Key".to_string(), "goog-secret".to_string()),
            ("Cookie".to_string(), "session=secret".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]);

        let masked = mask_sensitive_headers(&headers);

        assert_eq!(masked["Authorization"], MASKED_HEADER_VALUE);
        assert_eq!(masked["X-API-Key"], MASKED_HEADER_VALUE);
        assert_eq!(masked["X-Goog-Api-Key"], MASKED_HEADER_VALUE);
        assert_eq!(masked["Cookie"], MASKED_HEADER_VALUE);
        assert_eq!(masked["Content-Type"], "application/json");
    }
}
