use std::str::FromStr;

use conduit_core::error::ConduitError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestType {
    Chat,
    Embedding,
    Rerank,
    Image,
    Video,
    Compact,
    Completion,
    Speech,
    Transcription,
    Translation,
}

impl RequestType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embedding => "embedding",
            Self::Rerank => "rerank",
            Self::Image => "image",
            Self::Video => "video",
            Self::Compact => "compact",
            Self::Completion => "completion",
            Self::Speech => "speech",
            Self::Transcription => "transcription",
            Self::Translation => "translation",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ConduitError> {
        value.parse()
    }
}

impl FromStr for RequestType {
    type Err = ConduitError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "chat" => Ok(Self::Chat),
            "embedding" => Ok(Self::Embedding),
            "rerank" => Ok(Self::Rerank),
            "image" => Ok(Self::Image),
            "video" => Ok(Self::Video),
            "compact" => Ok(Self::Compact),
            "completion" => Ok(Self::Completion),
            "speech" => Ok(Self::Speech),
            "transcription" => Ok(Self::Transcription),
            "translation" => Ok(Self::Translation),
            other => Err(ConduitError::invalid_request(format!(
                "unknown request type: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApiFormat {
    #[serde(rename = "openai/chat_completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai/completions")]
    OpenAiCompletions,
    #[serde(rename = "openai/responses")]
    OpenAiResponses,
    #[serde(rename = "openai/responses_compact")]
    OpenAiResponsesCompact,
    #[serde(rename = "openai/image_generation")]
    OpenAiImageGeneration,
    #[serde(rename = "openai/image_edit")]
    OpenAiImageEdit,
    #[serde(rename = "openai/image_variation")]
    OpenAiImageVariation,
    #[serde(rename = "openai/embeddings")]
    OpenAiEmbeddings,
    #[serde(rename = "openai/video")]
    OpenAiVideo,
    #[serde(rename = "openai/audio_speech")]
    OpenAiAudioSpeech,
    #[serde(rename = "openai/audio_transcriptions")]
    OpenAiAudioTranscriptions,
    #[serde(rename = "openai/audio_translations")]
    OpenAiAudioTranslations,
    #[serde(rename = "gemini/contents")]
    GeminiContents,
    #[serde(rename = "gemini/embeddings")]
    GeminiEmbeddings,
    #[serde(rename = "anthropic/messages")]
    AnthropicMessages,
    #[serde(rename = "aisdk/text")]
    AiSdkText,
    #[serde(rename = "aisdk/datastream")]
    AiSdkDatastream,
    #[serde(rename = "jina/rerank")]
    JinaRerank,
    #[serde(rename = "jina/embeddings")]
    JinaEmbeddings,
    #[serde(rename = "ollama/chat")]
    OllamaChat,
    #[serde(rename = "seedance/video")]
    SeedanceVideo,
}

impl ApiFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai/chat_completions",
            Self::OpenAiCompletions => "openai/completions",
            Self::OpenAiResponses => "openai/responses",
            Self::OpenAiResponsesCompact => "openai/responses_compact",
            Self::OpenAiImageGeneration => "openai/image_generation",
            Self::OpenAiImageEdit => "openai/image_edit",
            Self::OpenAiImageVariation => "openai/image_variation",
            Self::OpenAiEmbeddings => "openai/embeddings",
            Self::OpenAiVideo => "openai/video",
            Self::OpenAiAudioSpeech => "openai/audio_speech",
            Self::OpenAiAudioTranscriptions => "openai/audio_transcriptions",
            Self::OpenAiAudioTranslations => "openai/audio_translations",
            Self::GeminiContents => "gemini/contents",
            Self::GeminiEmbeddings => "gemini/embeddings",
            Self::AnthropicMessages => "anthropic/messages",
            Self::AiSdkText => "aisdk/text",
            Self::AiSdkDatastream => "aisdk/datastream",
            Self::JinaRerank => "jina/rerank",
            Self::JinaEmbeddings => "jina/embeddings",
            Self::OllamaChat => "ollama/chat",
            Self::SeedanceVideo => "seedance/video",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ConduitError> {
        value.parse()
    }
}

impl FromStr for ApiFormat {
    type Err = ConduitError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "openai/chat_completions" => Ok(Self::OpenAiChatCompletions),
            "openai/completions" => Ok(Self::OpenAiCompletions),
            "openai/responses" => Ok(Self::OpenAiResponses),
            "openai/responses_compact" => Ok(Self::OpenAiResponsesCompact),
            "openai/image_generation" => Ok(Self::OpenAiImageGeneration),
            "openai/image_edit" => Ok(Self::OpenAiImageEdit),
            "openai/image_variation" => Ok(Self::OpenAiImageVariation),
            "openai/embeddings" => Ok(Self::OpenAiEmbeddings),
            "openai/video" => Ok(Self::OpenAiVideo),
            "openai/audio_speech" => Ok(Self::OpenAiAudioSpeech),
            "openai/audio_transcriptions" => Ok(Self::OpenAiAudioTranscriptions),
            "openai/audio_translations" => Ok(Self::OpenAiAudioTranslations),
            "gemini/contents" => Ok(Self::GeminiContents),
            "gemini/embeddings" => Ok(Self::GeminiEmbeddings),
            "anthropic/messages" => Ok(Self::AnthropicMessages),
            "aisdk/text" => Ok(Self::AiSdkText),
            "aisdk/datastream" => Ok(Self::AiSdkDatastream),
            "jina/rerank" => Ok(Self::JinaRerank),
            "jina/embeddings" => Ok(Self::JinaEmbeddings),
            "ollama/chat" => Ok(Self::OllamaChat),
            "seedance/video" => Ok(Self::SeedanceVideo),
            other => Err(ConduitError::invalid_request(format!(
                "unknown API format: {other}"
            ))),
        }
    }
}

/// Tool type constants ported from `conduit/llm/constants.go` (`ToolType*`).
///
/// Go leaves these as untyped string constants; they are exposed here as a
/// `&str` constants module (no newtype) so unknown provider tool types keep
/// round-tripping without a deserialization error.
pub mod tool_type {
    /// Function grounding tool type for OpenAI.
    pub const FUNCTION: &str = "function";
    /// Image generation grounding tool type for OpenAI.
    pub const IMAGE_GENERATION: &str = "image_generation";
    /// Web search grounding tool type.
    pub const WEB_SEARCH: &str = "web_search";
    /// Google Search grounding tool type for Gemini.
    pub const GOOGLE_SEARCH: &str = "google_search";
    /// Code execution tool type for Gemini.
    pub const GOOGLE_CODE_EXECUTION: &str = "google_code_execution";
    /// URL context grounding tool type for Gemini 2.0+.
    pub const GOOGLE_URL_CONTEXT: &str = "google_url_context";
    /// Custom tool type for OpenAI Responses API (freeform input, grammar-based
    /// format definition, not JSON).
    pub const RESPONSES_CUSTOM_TOOL: &str = "responses_custom_tool";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_type_round_trips_compatible_strings() -> Result<(), serde_json::Error> {
        let value = serde_json::to_value(RequestType::Chat)?;
        assert_eq!(value, serde_json::json!("chat"));
        assert_eq!(
            serde_json::from_value::<RequestType>(value)?,
            RequestType::Chat
        );
        Ok(())
    }

    #[test]
    fn api_format_round_trips_compatible_strings() -> Result<(), serde_json::Error> {
        let value = serde_json::to_value(ApiFormat::OpenAiChatCompletions)?;
        assert_eq!(value, serde_json::json!("openai/chat_completions"));
        assert_eq!(
            serde_json::from_value::<ApiFormat>(value)?,
            ApiFormat::OpenAiChatCompletions
        );
        Ok(())
    }

    #[test]
    fn unknown_api_format_maps_to_invalid_request() {
        let err = ApiFormat::parse("unknown/provider").err();
        assert_eq!(
            err.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );
    }

    #[test]
    fn tool_type_constants_match_go() {
        assert_eq!(tool_type::FUNCTION, "function");
        assert_eq!(tool_type::IMAGE_GENERATION, "image_generation");
        assert_eq!(tool_type::WEB_SEARCH, "web_search");
        assert_eq!(tool_type::GOOGLE_SEARCH, "google_search");
        assert_eq!(tool_type::GOOGLE_CODE_EXECUTION, "google_code_execution");
        assert_eq!(tool_type::GOOGLE_URL_CONTEXT, "google_url_context");
        assert_eq!(tool_type::RESPONSES_CUSTOM_TOOL, "responses_custom_tool");
    }
}
