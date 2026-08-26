#![forbid(unsafe_code)]

pub mod aisdk;
pub mod anthropic;
pub mod antigravity;
pub mod bailian;
pub mod cerebras;
pub mod claudecode;
pub mod codex;
pub mod copilot;
pub mod deepseek;
pub mod doubao;
pub mod doubao_transformer;
pub mod errors;
pub mod fireworks;
pub mod gemini;
pub mod jina;
pub mod jina_transformer;
pub mod longcat;
pub mod modelscope;
pub mod moonshot;
pub mod nanogpt;
pub mod openai;
pub mod openai_compatible;
pub mod openai_legacy;
pub mod openai_outbound;
pub mod openai_responses_outbound;
pub mod openai_stream;
pub mod openrouter;
pub mod registry;
pub mod traits;
pub mod zai;

pub use anthropic::{
    AnthropicCountTokensInboundTransformer, AnthropicInboundTransformer, AnthropicOutboundConfig,
    AnthropicOutboundTransformer, normalize_messages_body as normalize_anthropic_messages_body,
    transform_messages_request as transform_anthropic_messages_request,
};

pub use gemini::{GeminiInboundTransformer, GeminiOutboundTransformer};

pub use doubao_transformer::DoubaoVideoInbound;
pub use jina_transformer::JinaInbound;
pub use openai::{
    OpenAiChatInbound, OpenAiEmbeddingInbound, OpenAiImageGenerationInbound,
    OpenAiResponsesInbound, OpenAiSpeechInbound, OpenAiVideoInbound,
    normalize_chat_completions_body, normalize_completions_body, normalize_openai_body,
    normalize_openai_request, normalize_responses_body,
};
pub use openai_legacy::{OpenAiCompletionInbound, OpenAiCompletionOutbound};
pub use openai_outbound::{
    AudioResponseMode, CONDUIT_CLEAR_SENTINEL, Config as OpenAiOutboundConfig,
    ModelMapping as OpenAiModelMapping, OverrideHeaderKind as OpenAiOverrideHeaderKind,
    OverrideHeaderOp as OpenAiOverrideHeaderOp, PlatformType as OpenAiPlatformType,
    apply_outbound_base_url as apply_openai_outbound_base_url,
    apply_outbound_transport as apply_openai_outbound_transport,
    apply_override_header_op as apply_openai_override_header_op,
    audio_json_mode_is_non_streaming as audio_json_mode_is_non_streaming_openai,
    audio_request_accept_header as audio_request_accept_header_openai,
    build_auth_header as build_openai_auth_header,
    classify_audio_response_format as classify_audio_response_format_openai,
    extract_usage as extract_openai_usage, map_model as map_openai_model,
    normalize_base_url as normalize_openai_base_url,
    resolve_outbound_path as resolve_openai_outbound_path,
    resolve_outbound_url as resolve_openai_outbound_url,
    resolve_outbound_url_for_endpoint as resolve_openai_outbound_url_for_endpoint,
    speech_response_content_type as speech_response_content_type_openai,
};
pub use openai_responses_outbound::{
    DEFAULT_RESPONSES_COMPACT_PATH, DEFAULT_RESPONSES_PATH, OpenAiResponsesOutbound,
    ResponsesOutboundConfig,
};
pub use traits::{
    InboundTransformer, OutboundTransformer, TransformerKey, TransformerRegistry,
    TransformerResult, VideoTaskOutbound,
};

pub const CRATE_NAME: &str = "conduit-transformers";
