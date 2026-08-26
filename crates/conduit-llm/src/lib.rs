#![forbid(unsafe_code)]

pub mod constants;
pub mod http;
pub mod model;
pub mod usage;

pub use constants::{ApiFormat, RequestType};
pub use http::{
    BodyDecoder, ClientBuildError, DecodedBody, HttpClientBuilder, HttpDecodeError,
    MAX_ERROR_BODY_BYTES, MAX_SSE_EVENT_SIZE, MaskSensitiveHeaders, ProxyConfig, ProxyMode,
    SseFrame, SseParseResult, TransportConfig, UpstreamError, decode_response_body,
    mask_sensitive_headers,
};
pub use model::{
    Annotation, AudioRequest, ChatMessage, ChatRequest, Choice, CompletionRequest, ContentPart,
    EmbeddingRequest, ErrorDetail, HttpAuth, HttpRequest, HttpResponse, ImageRequest,
    InlineToolResult, LlmMessage, LlmRequest, LlmRequestPayload, LlmResponse, MessageContent,
    OutputAudio, RerankRequest, ResponseError, ResponsesRequest, StreamEvent, ToolCall,
    UnifiedTool, UrlCitation, VideoRequest,
};
pub use usage::{TokenDetails, Usage};

pub const CRATE_NAME: &str = "conduit-llm";
