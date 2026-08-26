//! HTTP client, transport/decoder, and SSE helpers for the LLM layer.
//! Mirrors the Go `llm/httpclient` package.

pub mod client;
pub mod decoder;

pub use client::{
    ClientBuildError, HttpClientBuilder, MAX_ERROR_BODY_BYTES, ProxyConfig, ProxyMode,
    TransportConfig, UpstreamError,
};
pub use decoder::MAX_SSE_EVENT_SIZE;
pub use decoder::{
    BodyDecoder, DecodedBody, HttpDecodeError, MaskSensitiveHeaders, SseFrame, SseParseResult,
    decode_response_body, mask_sensitive_headers, parse_sse_frames,
    parse_sse_frames_with_max_event_size,
};
