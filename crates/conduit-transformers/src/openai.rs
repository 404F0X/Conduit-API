use conduit_core::{ConduitError, ErrorKind};
use conduit_llm::{
    ApiFormat, AudioRequest, ChatRequest, CompletionRequest, ContentPart, EmbeddingRequest,
    HttpRequest, HttpResponse, ImageRequest, LlmMessage, LlmRequest, LlmRequestPayload,
    LlmResponse, MessageContent, RequestType, ResponsesRequest, StreamEvent, ToolCall, UnifiedTool,
    VideoRequest,
};
use serde::de::{DeserializeOwned, Error as _};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

use crate::TransformerResult;
use crate::traits::InboundTransformer;

const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";
const COMPLETIONS_PATH: &str = "/v1/completions";
const RESPONSES_PATH: &str = "/v1/responses";
const RESPONSES_COMPACT_PATH: &str = "/v1/responses/compact";
const EMBEDDINGS_PATH: &str = "/v1/embeddings";
const AUDIO_SPEECH_PATH: &str = "/v1/audio/speech";
const AUDIO_TRANSCRIPTIONS_PATH: &str = "/v1/audio/transcriptions";
const AUDIO_TRANSLATIONS_PATH: &str = "/v1/audio/translations";
const IMAGES_GENERATIONS_PATH: &str = "/v1/images/generations";
const IMAGES_EDITS_PATH: &str = "/v1/images/edits";
const VIDEOS_PATH: &str = "/v1/videos";

pub fn normalize_openai_request(request: HttpRequest) -> TransformerResult<LlmRequest> {
    let api_format = request
        .api_format
        .or_else(|| api_format_from_path(&request.path))
        .ok_or_else(|| {
            ConduitError::invalid_request(format!(
                "unsupported OpenAI inbound request path: {}",
                request.path
            ))
        })?;
    let body = request_json_body(&request)?;
    let mut llm_request = normalize_openai_body(api_format, body)?;

    llm_request.extra_headers = request.headers;
    llm_request.metadata = request.metadata;

    if let Some(request_id) = request.request_id {
        llm_request
            .metadata
            .insert("request_id".to_string(), Value::String(request_id));
    }
    if let Some(client_ip) = request.client_ip {
        llm_request
            .metadata
            .insert("client_ip".to_string(), Value::String(client_ip));
    }

    Ok(llm_request)
}

pub fn normalize_openai_body(api_format: ApiFormat, body: Value) -> TransformerResult<LlmRequest> {
    match api_format {
        ApiFormat::OpenAiChatCompletions => normalize_chat_completions_body(body),
        ApiFormat::OpenAiCompletions => normalize_completions_body(body),
        ApiFormat::OpenAiResponses => normalize_responses_body(body, false),
        ApiFormat::OpenAiResponsesCompact => normalize_responses_body(body, true),
        ApiFormat::OpenAiEmbeddings => normalize_embeddings_body(body),
        ApiFormat::OpenAiAudioSpeech => normalize_audio_body(body, RequestType::Speech),
        ApiFormat::OpenAiAudioTranscriptions => {
            normalize_audio_body(body, RequestType::Transcription)
        }
        ApiFormat::OpenAiAudioTranslations => normalize_audio_body(body, RequestType::Translation),
        ApiFormat::OpenAiImageGeneration => {
            normalize_image_body_with_format(body, RequestType::Image, api_format)
        }
        ApiFormat::OpenAiImageEdit => {
            normalize_image_body_with_format(body, RequestType::Image, api_format)
        }
        ApiFormat::OpenAiVideo => normalize_video_body(body),
        _ => Err(ConduitError::invalid_request(format!(
            "unsupported OpenAI inbound API format: {}",
            api_format.as_str()
        ))),
    }
}

pub fn normalize_chat_completions_body(body: Value) -> TransformerResult<LlmRequest> {
    // Match Go `InboundTransformer.TransformRequest` validation (inbound.go):
    // `model` is required and `messages` must contain at least one entry.
    let mut object = body_object(body)?;
    let model = take_optional_string(&mut object, "model")?
        .ok_or_else(|| ConduitError::invalid_request("model is required"))?;
    // Take `messages` out so we can validate length before deserializing the
    // rest of the payload (Go rejects empty slices; serde would default it).
    let messages = object.remove("messages");
    validate_messages_present(&messages)?;
    // `validate_messages_present` guarantees `messages` is a non-empty array,
    // so it is always safe to put it back as a concrete `Value`.
    if let Some(messages) = messages {
        object.insert("messages".to_string(), messages);
    }
    let stream = take_optional_bool(&mut object, "stream")?.unwrap_or(false);

    // Validate `stream_options` shape (Go decodes it into the typed
    // `StreamOptions{IncludeUsage bool}` struct in openai/model.go; a
    // type-mismatch would fail JSON decode there). See
    // `validate_stream_options` for the parity rationale.
    let stream_options = object.get("stream_options").cloned();
    validate_stream_options(&stream_options)?;

    // Extract chat-completions `tools[]` (nested `{type, function:{...}}`) and
    // convert each to the flat `UnifiedTool` shape the Rust unified model uses:
    // `type` → typed `tool_type`, and the entire `function` sub-object's keys
    // (`name`, `description`, `parameters`, `strict`, …) are flattened onto the
    // tool's `extra` bag. This mirrors Go's `openai.Tool.ToLLMTool()`
    // (inbound_convert.go) which copies `Function.{Name,Description,Parameters,
    // Strict}` onto the unified `llm.Tool.Function` typed sub-struct — in Rust
    // the unified model is flat with a catch-all `extra`, so the function
    // fields ride in `extra` (preserving them losslessly for downstream
    // providers and round-tripping).
    let raw_tools = object.remove("tools");
    let mut payload = deserialize_payload::<ChatRequest>(object)?;
    payload.tools = build_unified_tools(raw_tools)?;

    Ok(LlmRequest {
        request_type: RequestType::Chat,
        api_format: ApiFormat::OpenAiChatCompletions,
        model: Some(model),
        stream,
        payload: LlmRequestPayload::Chat(payload),
        extra_body: Default::default(),
        extra_headers: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    })
}

// Mirror Go's typed `StreamOptions` decode (openai/model.go):
// `StreamOptions struct { IncludeUsage bool }`. Go unmarshals
// `stream_options` into this typed struct inside `Request`, so any non-object
// value (string/number/array) or a non-boolean `include_usage` fails JSON
// decode in Go and surfaces as a 400 `invalid_request` /
// `failed to decode openai request` error. To match that contract without
// weakening the catch-all `Option<Value>` slot the Rust unified model uses,
// we explicitly validate the shape here. An absent `stream_options` is fine
// (it's `omitempty` on the Go side too); an object with only `include_usage:
// bool` (and any other provider-extension keys, since Go's struct ignores
// unknown fields by default) passes through.
fn validate_stream_options(stream_options: &Option<Value>) -> TransformerResult<()> {
    let Some(value) = stream_options else {
        return Ok(());
    };
    let Value::Object(obj) = value else {
        return Err(ConduitError::invalid_request(
            "OpenAI inbound field `stream_options` must be an object",
        )
        .with_source(serde_json::Error::custom(format!("got {value}"))));
    };
    if let Some(include_usage) = obj.get("include_usage") {
        if !include_usage.is_boolean() {
            return Err(ConduitError::invalid_request(
                "OpenAI inbound field `stream_options.include_usage` must be a boolean",
            )
            .with_source(serde_json::Error::custom(format!("got {include_usage}"))));
        }
    }
    Ok(())
}

// Mirrors Go `if len(oaiReq.Messages) == 0` (inbound.go): reject both absent
// and empty message arrays. A non-array `messages` is left for the payload
// deserializer to surface as a typed error.
fn validate_messages_present(messages: &Option<Value>) -> TransformerResult<()> {
    let is_empty = match messages {
        None | Some(Value::Null) => true,
        Some(Value::Array(items)) => items.is_empty(),
        _ => false,
    };
    if is_empty {
        return Err(ConduitError::invalid_request("messages are required"));
    }
    Ok(())
}

// Convert the raw chat-completions `tools[]` JSON array into the flat
// `UnifiedTool` model. `type` becomes the typed `tool_type`; for function tools
// the nested `function` sub-object's keys (`name`, `description`, `parameters`,
// `strict`, …) are flattened onto the tool's `extra` bag, mirroring Go's
// `openai.Tool.ToLLMTool()` which lifts `Function.{Name,Description,Parameters,
// Strict}` onto the unified `llm.Tool.Function` typed sub-struct. Non-function
// tools (`web_search_preview`, `image_generation`, …) carry all of their
// non-`type` keys in `extra` as well, preserving provider extensions
// losslessly. A non-array `tools` value is rejected; an absent `tools` yields
// an empty vec (matching `ChatRequest.tools`'s `#[serde(default)]`).
fn build_unified_tools(raw: Option<Value>) -> TransformerResult<Vec<UnifiedTool>> {
    let Some(Value::Array(items)) = raw else {
        if let Some(value) = raw {
            return Err(ConduitError::invalid_request(
                "OpenAI inbound field `tools` must be an array",
            )
            .with_source(serde_json::Error::custom(format!("got {value}"))));
        }
        return Ok(Vec::new());
    };

    let mut tools = Vec::with_capacity(items.len());
    for item in items {
        let Value::Object(mut obj) = item else {
            return Err(ConduitError::invalid_request(
                "OpenAI inbound field `tools[*]` must be an object",
            ));
        };
        let tool_type = obj
            .remove("type")
            .map(serde_json::from_value::<String>)
            .transpose()
            .map_err(|err| {
                ConduitError::invalid_request(
                    "OpenAI inbound field `tools[*].type` must be a string",
                )
                .with_source(err)
            })?
            .unwrap_or_default();
        // For a chat-completions function tool, lift the `function` sub-object's
        // keys onto the tool top level so they all land in `extra` (the unified
        // tool's catch-all bag). Any top-level keys already present on the tool
        // (other than `type`/`function`) are also preserved in `extra`. First
        // write wins so an explicit top-level field is authoritative.
        let mut extra: BTreeMap<String, Value> = BTreeMap::new();
        if let Some(function) = obj.remove("function") {
            if let Value::Object(function_obj) = function {
                merge_keys(&mut extra, function_obj);
            } else {
                // Non-object `function`: preserve verbatim in extra.
                extra.insert("function".to_string(), function);
            }
        }
        merge_keys(&mut extra, obj);
        tools.push(UnifiedTool {
            tool_type,
            name: None,
            description: None,
            parameters: None,
            extra,
        });
    }
    Ok(tools)
}

// Insert every `(key, value)` from `src` into `dst` without overwriting any
// existing entry — first write wins, matching the chat-completions convention
// that an explicitly top-level field is authoritative. `src` is the serde_json
// map shape (from `Value::Object`); `dst` is the `ExtensionMap` (BTreeMap)
// shape the unified tool model uses.
fn merge_keys(dst: &mut BTreeMap<String, Value>, src: Map<String, Value>) {
    for (key, value) in src {
        dst.entry(key).or_insert(value);
    }
}

pub fn normalize_completions_body(body: Value) -> TransformerResult<LlmRequest> {
    let mut object = body_object(body)?;
    let model = take_optional_string(&mut object, "model")?;
    let stream = take_optional_bool(&mut object, "stream")?.unwrap_or(false);
    let payload = deserialize_payload::<CompletionRequest>(object)?;

    Ok(LlmRequest {
        request_type: RequestType::Completion,
        api_format: ApiFormat::OpenAiCompletions,
        model,
        stream,
        payload: LlmRequestPayload::Completion(payload),
        extra_body: Default::default(),
        extra_headers: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    })
}

pub fn normalize_responses_body(body: Value, compact: bool) -> TransformerResult<LlmRequest> {
    let mut object = body_object(body)?;
    let model = take_optional_string(&mut object, "model")?;
    let stream = take_optional_bool(&mut object, "stream")?.unwrap_or(false);
    let mut payload = deserialize_payload::<ResponsesRequest>(object)?;
    payload.compact = compact || payload.compact;

    Ok(LlmRequest {
        request_type: if payload.compact {
            RequestType::Compact
        } else {
            RequestType::Chat
        },
        api_format: if payload.compact {
            ApiFormat::OpenAiResponsesCompact
        } else {
            ApiFormat::OpenAiResponses
        },
        model,
        stream,
        payload: LlmRequestPayload::Responses(payload),
        extra_body: Default::default(),
        extra_headers: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    })
}

// Mirrors Go `EmbeddingInboundTransformer.TransformRequest`
// (embedding_inbound.go) for the `/v1/embeddings` path: decode the body into
// the typed `EmbeddingRequest`, enforce `model` is non-empty, validate `input`
// (string / `[]string` / `[]int64` / `[][]int64`, mirroring Go's
// `validateEmbeddingInput`) and lift `encoding_format` / `dimensions` / `user`
// onto the unified `EmbeddingRequest`. Embeddings never stream — `stream` is
// hard-false, matching Go which leaves `Stream = nil`.
//
// Input validation intentionally mirrors Go's `validateEmbeddingInput` rather
// than relying on typed deserialize: Go decodes `input` into a custom
// `EmbeddingInput` whose `UnmarshalJSON` picks one of `String` /
// `StringArray` / `IntArray` / `IntArrayArray` based on JSON shape. We keep
// the raw `Value` and dispatch on its JSON kind to reproduce the exact error
// messages (`"input cannot be empty string"` /
// `"input cannot be empty array"` / `"input[N] cannot be empty string"` /
// `"input[N] cannot be empty array"`) so inbound 400 responses stay
// byte-compatible with the Go gateway.
pub fn normalize_embeddings_body(body: Value) -> TransformerResult<LlmRequest> {
    let mut object = body_object(body)?;
    let model = take_optional_string(&mut object, "model")?
        .ok_or_else(|| ConduitError::invalid_request("model is required"))?;
    // Take `input` out so we can run the Go-shaped validation against the raw
    // JSON value before handing the typed payload to serde (which only stores
    // it as an opaque `Option<Value>`).
    let input = object.remove("input");
    validate_embedding_input(&input)?;

    // Re-insert `input` so the typed `EmbeddingRequest` deserializer picks it
    // up alongside `encoding_format` / `dimensions` / `user` and any unknown
    // provider extension fields (preserved via `EmbeddingRequest.extra`).
    if let Some(input) = input {
        object.insert("input".to_string(), input);
    }

    let payload = deserialize_payload::<EmbeddingRequest>(object)?;

    Ok(LlmRequest {
        request_type: RequestType::Embedding,
        api_format: ApiFormat::OpenAiEmbeddings,
        model: Some(model),
        // Embeddings do not stream (Go leaves `Stream = nil`).
        stream: false,
        payload: LlmRequestPayload::Embedding(payload),
        extra_body: Default::default(),
        extra_headers: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    })
}

// Mirrors Go `AudioInboundTransformer.TransformRequest`
// (audio_inbound.go) for the three OpenAI audio endpoints:
//
// - `/v1/audio/speech`           (TTS, `RequestType::Speech`)            — JSON body
// - `/v1/audio/transcriptions`   (STT, `RequestType::Transcription`)     — multipart form
// - `/v1/audio/translations`     (STT, `RequestType::Translation`)       — multipart form
//
// `body` is the request body as a JSON `Value`. For the multipart endpoints
// the gateway is expected to have already parsed the multipart form and
// assembled a JSON representation of it (mirroring Go's
// `buildAudioJSONBody`, which replaces the binary `file` part with a size
// placeholder) before calling this normalizer; this keeps the transformer
// free of a multipart parser dependency and matches the Go convention where
// `httpReq.JSONBody` carries the same shape downstream.
//
// The unified `AudioRequest` is a single flat struct covering all three
// audio kinds; per-endpoint typed slots the Go side exposes (`Speech`,
// `Transcription`, `Translation` pointers) collapse onto the same fields:
// `input` / `file` / `voice` / `language` / `response_format` /
// `temperature` are first-class; everything else (`stream_format`,
// `instructions`, `speed`, `prompt`, `file_name`, `timestamp_granularities[]`,
// …) round-trips via `AudioRequest.extra`.
pub fn normalize_audio_body(
    body: Value,
    request_type: RequestType,
) -> TransformerResult<LlmRequest> {
    let api_format = match request_type {
        RequestType::Speech => ApiFormat::OpenAiAudioSpeech,
        RequestType::Transcription => ApiFormat::OpenAiAudioTranscriptions,
        RequestType::Translation => ApiFormat::OpenAiAudioTranslations,
        // Caller invariant: `normalize_openai_body` only dispatches the three
        // audio kinds above. Any other value is a programming error.
        other => {
            return Err(ConduitError::invalid_request(format!(
                "unsupported audio request type: {}",
                other.as_str()
            )));
        }
    };

    let mut object = body_object(body)?;
    let model = take_optional_string(&mut object, "model")?
        .ok_or_else(|| ConduitError::invalid_request("model is required"))?;

    // Endpoint-specific validation, mirroring Go's
    // `transformSpeechRequest` / `transformTranscriptionRequest` /
    // `transformTranslationRequest`. Speech additionally requires `input`
    // and `voice` and validates `stream_format`; the two STT endpoints
    // require a `file` part.
    let stream = match request_type {
        RequestType::Speech => validate_speech_fields(&mut object)?,
        RequestType::Transcription | RequestType::Translation => {
            validate_stt_fields(&object, request_type)?;
            false
        }
        // Unreachable: guarded above.
        _ => false,
    };

    let payload = deserialize_payload::<AudioRequest>(object)?;

    Ok(LlmRequest {
        request_type,
        api_format,
        model: Some(model),
        stream,
        payload: LlmRequestPayload::Audio(payload),
        extra_body: Default::default(),
        extra_headers: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    })
}

// Apply Go `transformSpeechRequest` body-layer validation: `input` and
// `voice` are required (non-empty), and `stream_format` (if present) must be
// one of `"sse"` / `"audio"`. Returns the resulting stream flag — `true`
// exactly when `stream_format` is non-empty (Go: `isStream := streamFormat
// != ""`). The lowercased `stream_format` is written back into `object` so
// the typed payload deserializer picks up the canonicalized value, matching
// Go which lowercases+trims `body.StreamFormat` before storing it on
// `SpeechRequest.StreamFormat`.
fn validate_speech_fields(object: &mut Map<String, Value>) -> TransformerResult<bool> {
    let input = object
        .remove("input")
        .map(serde_json::from_value::<String>)
        .transpose()
        .map_err(|err| {
            ConduitError::invalid_request("OpenAI inbound field `input` must be a string")
                .with_source(err)
        })?
        .unwrap_or_default();
    if input.trim().is_empty() {
        return Err(ConduitError::invalid_request("input is required"));
    }
    // Put `input` back as a plain string so the typed deserializer can pick
    // it up (matches Go storing `body.Input` on `SpeechRequest.Input`).
    object.insert("input".to_string(), Value::String(input));

    let voice = take_optional_string(object, "voice")?.unwrap_or_default();
    if voice.is_empty() {
        return Err(ConduitError::invalid_request("voice is required"));
    }
    // Put `voice` back so it deserializes onto `AudioRequest.voice`.
    object.insert("voice".to_string(), Value::String(voice));

    // Canonicalize `stream_format` (trim + ASCII-lowercase) and reject
    // anything outside `""` / `"sse"` / `"audio"`, mirroring Go's
    // `transformSpeechRequest` guard. The canonical value is written back
    // so it round-trips via `AudioRequest.extra` (stream_format has no
    // first-class slot on the Rust unified audio model).
    let stream_format = object
        .remove("stream_format")
        .map(serde_json::from_value::<String>)
        .transpose()
        .map_err(|err| {
            ConduitError::invalid_request("OpenAI inbound field `stream_format` must be a string")
                .with_source(err)
        })?
        .unwrap_or_default();
    let canonical = stream_format.trim().to_ascii_lowercase();
    if !canonical.is_empty() && canonical != "sse" && canonical != "audio" {
        return Err(ConduitError::invalid_request(format!(
            "unsupported stream_format: {:?} (only \"sse\" and \"audio\" are supported)",
            stream_format
        )));
    }
    let is_stream = !canonical.is_empty();
    if is_stream {
        object.insert("stream_format".to_string(), Value::String(canonical));
    }
    Ok(is_stream)
}

// Apply Go `transformTranscriptionRequest` /
// `transformTranslationRequest` body-layer validation on the JSON view of a
// multipart form: `model` is already enforced by the caller, and a `file`
// part must be present (Go: `len(form.File) == 0` -> `"file is required for
// transcription"` / `"file is required for translation"`). The multipart
// parser upstream is responsible for placing the file metadata under the
// `file` key (Go's `buildAudioJSONBody` writes a `<audio bytes: N,
// filename: …>` placeholder there) so its presence is enough to satisfy the
// Go-shaped guard at this layer.
fn validate_stt_fields(
    object: &Map<String, Value>,
    request_type: RequestType,
) -> TransformerResult<()> {
    if object.get("file").map(Value::is_null).unwrap_or(true) {
        let label = match request_type {
            RequestType::Transcription => "transcription",
            RequestType::Translation => "translation",
            // Unreachable: only the two STT kinds reach here.
            other => other.as_str(),
        };
        return Err(ConduitError::invalid_request(format!(
            "file is required for {label}"
        )));
    }
    Ok(())
}

// Mirrors Go `ImageInboundTransformer.TransformRequest`
// (image_inbound.go) for the OpenAI image endpoints:
//
// - `/v1/images/generations` (`ApiFormat::OpenAiImageGeneration`) — JSON body
// - `/v1/images/edits`       (`ApiFormat::OpenAiImageEdit`)        — multipart form
//
// `body` is the request body as a JSON `Value`. For the multipart `/edits`
// endpoint the gateway is expected to have already parsed the multipart form
// and assembled a JSON representation of it (mirroring Go's
// `buildMultipartJSONBody`, which replaces each binary `image`/`mask` part
// with a `data:<content-type>;base64,<…>` data URL and preserves the scalar
// `prompt` / `model` / `size` / `quality` / `response_format` / `n` / `user`
// fields) before calling this normalizer. This keeps the transformer free of
// a multipart parser dependency and matches the Go convention where
// `httpReq.JSONBody` carries the same shape downstream. Under that contract
// the body-layer validation (prompt required, model default, image-part
// presence for edits, stream rejection) reproduces the Go error strings
// exactly.
//
// The unified `ImageRequest` is a single flat struct; per-endpoint typed
// slots the Go side exposes (`Prompt`, `N`, `Size`, `Quality`,
// `ResponseFormat`, `Image`, `Mask`, …) map onto the matching Rust fields,
// while everything else (`background`, `output_format`, `output_compression`,
// `moderation`, `partial_images`, `style`, `user`, `input_fidelity`,
// `images[]`, …) round-trips via `ImageRequest.extra`.
//
// Images never stream — `stream` is hard-`false`, matching Go's
// `lo.ToPtr(false)`. Go additionally rejects an inbound `stream:true` flag
// (per-endpoint error strings); we reproduce that guard here.
pub fn normalize_image_body(
    body: Value,
    request_type: RequestType,
) -> TransformerResult<LlmRequest> {
    let api_format = match request_type {
        // Both `/v1/images/generations` and `/v1/images/edits` carry the
        // same `RequestType::Image`; the caller selects the `ApiFormat` via
        // `normalize_openai_body`. We resolve it here from the request type
        // alone is insufficient — both image endpoints share
        // `RequestType::Image`. Instead, the dispatch in
        // `normalize_openai_body` passes `RequestType::Image` for both and
        // relies on the caller-supplied `ApiFormat` having already routed
        // to the correct branch. To preserve a single-arg signature we
        // default to `OpenAiImageGeneration` and let the dedicated
        // `normalize_image_body_with_format` (below) carry the exact format
        // for the dispatcher. (Kept public for symmetry with the other
        // endpoint normalizers; the dispatcher uses the `_with_format`
        // variant.)
        RequestType::Image => ApiFormat::OpenAiImageGeneration,
        other => {
            return Err(ConduitError::invalid_request(format!(
                "unsupported image request type: {}",
                other.as_str()
            )));
        }
    };
    normalize_image_body_with_format(body, request_type, api_format)
}

// Like `normalize_image_body` but takes the exact `ApiFormat` selected by
// `normalize_openai_body`. This is the entry point the dispatcher actually
// uses; the public `normalize_image_body(body, request_type)` above is a
// convenience wrapper that defaults the format and exists for symmetry with
// `normalize_audio_body` / `normalize_embeddings_body`.
pub fn normalize_image_body_with_format(
    body: Value,
    request_type: RequestType,
    api_format: ApiFormat,
) -> TransformerResult<LlmRequest> {
    let mut object = body_object(body)?;

    // Go rejects an inbound `stream:true` flag with endpoint-specific error
    // strings (image_inbound.go): "image generation does not support
    // streaming" / "image edit does not support streaming" /
    // "image variation does not support streaming". We surface the same
    // guard at the body layer (the typed `LlmRequest.stream` is hard-false).
    if let Some(stream_value) = object.remove("stream") {
        let wants_stream = match stream_value {
            Value::Bool(b) => b,
            // A non-bool `stream` is left for the typed deserializer to
            // surface as a typed error elsewhere; here we only enforce the
            // Go guard for the boolean case (the only shape Go's
            // `multipart`/JSON `stream` field carries).
            other => {
                object.insert("stream".to_string(), other);
                false
            }
        };
        if wants_stream {
            return Err(ConduitError::invalid_request(format!(
                "{} does not support streaming",
                match api_format {
                    ApiFormat::OpenAiImageGeneration => "image generation",
                    ApiFormat::OpenAiImageEdit => "image edit",
                    ApiFormat::OpenAiImageVariation => "image variation",
                    // Unreachable: only image formats reach here.
                    other => other.as_str(),
                }
            )));
        }
    }

    // `model` is optional; Go defaults it to `"dall-e-2"` for both
    // generations and edits (image_inbound.go `transformGenerationRequest` /
    // `transformEditRequest`).
    let model =
        take_optional_string(&mut object, "model")?.unwrap_or_else(|| "dall-e-2".to_string());

    // `prompt` handling differs per endpoint (image_inbound.go):
    // - generations: required (`"prompt is required"`).
    // - edits:       required, trimmed (`"prompt is required for image edits"`).
    // We trim and require non-empty for both; the per-endpoint error label
    // preserves the Go string.
    let prompt = take_optional_string(&mut object, "prompt")?
        .unwrap_or_default()
        .trim()
        .to_string();
    if prompt.is_empty() {
        return Err(ConduitError::invalid_request(match api_format {
            ApiFormat::OpenAiImageEdit => "prompt is required for image edits",
            // Generations (and the out-of-S09 variation endpoint, for
            // completeness) use the plain "prompt is required" string.
            _ => "prompt is required",
        }));
    }
    // Put the trimmed prompt back so the typed deserializer can land it on
    // `ImageRequest.prompt`.
    object.insert("prompt".to_string(), Value::String(prompt));

    // Edits additionally require at least one `image` part (Go:
    // `len(formData.Images) == 0` -> `"at least one image is required for
    // edits"`). Under the JSON-view contract the gateway surfaces the parsed
    // image(s) under the `image` key (data URL or array of data URLs for the
    // multi-image `image[]` form), so presence is enough to satisfy the
    // Go-shaped guard at this layer.
    if matches!(api_format, ApiFormat::OpenAiImageEdit)
        && object.get("image").map(Value::is_null).unwrap_or(true)
    {
        return Err(ConduitError::invalid_request(
            "at least one image is required for edits",
        ));
    }

    let payload = deserialize_payload::<ImageRequest>(object)?;

    Ok(LlmRequest {
        request_type,
        api_format,
        model: Some(model),
        // Image endpoints never engage the streaming pipeline.
        stream: false,
        payload: LlmRequestPayload::Image(payload),
        extra_body: Default::default(),
        extra_headers: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    })
}

// Mirrors Go `VideoInboundTransformer.TransformRequest` (video_inbound.go)
// for `POST /v1/videos` (`ApiFormat::OpenAiVideo`). The Go endpoint accepts
// either `application/json` (typed `VideoCreateRequest`) or `multipart/
// form-data` (parsed by `parseVideoMultipartRequest`); under the JSON-view
// contract used by this transformer crate, the gateway supplies the body as
// a JSON `Value` in either case — for multipart the gateway pre-parses the
// form (mirroring Go's `parseVideoMultipartRequest`, which base64-encodes
// the `input_reference` file part into a `data:<ct>;base64,…` URL and trims
// every scalar field) before calling this normalizer.
//
// Validation reproduces Go's body-layer guards exactly (video_inbound.go
// `TransformRequest`): `model` is required (`"model is required"`), `prompt`
// is required (`"prompt is required"`). Go additionally lifts the optional
// `input_reference` / `seconds` / `size` fields onto a typed `VideoRequest`
// carrying a `content` array (`text` part + optional `image_url`
// `first_frame` part). The Rust unified `VideoRequest` is a flat struct
// (`prompt` / `image` / `duration` / `size` + `extra`); the `content` array
// collapses onto `prompt` (the text part) and `image` (the `image_url` part,
// which carries the `input_reference` value as an opaque JSON value), so we
// map the fields directly without rebuilding the content array.
//
// Videos never stream — `stream` is hard-`false`, matching Go's
// `lo.ToPtr(false)`. Go's `VideoCreateRequest` has no `stream` field, so
// there is no inbound `stream:true` to reject (unlike the image endpoint).
pub fn normalize_video_body(body: Value) -> TransformerResult<LlmRequest> {
    let mut object = body_object(body)?;

    let model = take_optional_string(&mut object, "model")?
        .ok_or_else(|| ConduitError::invalid_request("model is required"))?;
    if model.trim().is_empty() {
        return Err(ConduitError::invalid_request("model is required"));
    }

    let prompt = take_optional_string(&mut object, "prompt")?
        .unwrap_or_default()
        .trim()
        .to_string();
    if prompt.is_empty() {
        return Err(ConduitError::invalid_request("prompt is required"));
    }

    // `seconds` → Go's `Seconds *string` → unified `VideoRequest.duration`.
    // Go only stores a non-nil pointer when the field is non-empty after
    // trim; we mirror that by only re-inserting the trimmed value when it is
    // non-empty so the typed deserializer lands a `None` slot otherwise.
    let seconds = take_optional_string(&mut object, "seconds")?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(ref seconds) = seconds {
        object.insert("seconds".to_string(), Value::String(seconds.clone()));
    } else {
        // Remove a present-but-empty `seconds` so the typed deserializer
        // does not surface it on `VideoRequest.extra`.
        object.remove("seconds");
    }

    // `size` → Go's `Size` (trimmed) → unified `VideoRequest.size`.
    let size = take_optional_string(&mut object, "size")?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(ref size) = size {
        object.insert("size".to_string(), Value::String(size.clone()));
    }

    // `input_reference` → Go's `InputReference` (trimmed; or a data URL when
    // a multipart file part was supplied) → unified `VideoRequest.image`
    // (opaque JSON value, preserving both URL and data-URL forms). Go only
    // appends the `image_url` content part when the trimmed value is
    // non-empty; we mirror that by only carrying it forward when present.
    let input_reference = take_optional_string(&mut object, "input_reference")?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // Re-assemble the body the typed deserializer will see: put `prompt` back
    // (trimmed), and (when present) put `image` (the input_reference) and
    // `duration` (seconds) under the keys the unified `VideoRequest` expects.
    // Everything else unknown to the unified model rides via `VideoRequest.extra`.
    object.insert("prompt".to_string(), Value::String(prompt));
    if let Some(input_reference) = input_reference {
        object.insert("image".to_string(), Value::String(input_reference));
    }
    if let Some(seconds) = seconds {
        object.insert("duration".to_string(), Value::String(seconds));
    }

    let payload = deserialize_payload::<VideoRequest>(object)?;

    Ok(LlmRequest {
        request_type: RequestType::Video,
        api_format: ApiFormat::OpenAiVideo,
        model: Some(model),
        // Videos never stream.
        stream: false,
        payload: LlmRequestPayload::Video(payload),
        extra_body: Default::default(),
        extra_headers: Default::default(),
        metadata: Default::default(),
        extra: Default::default(),
    })
}

// JSON `input` value. The Go side decodes `input` into a custom
// `llm.EmbeddingInput` whose `UnmarshalJSON` sets exactly one of
// `String`/`StringArray`/`IntArray`/`IntArrayArray` based on JSON kind. An
// absent `input` yields the zero value, which falls through to the
// "empty string" branch. We dispatch on JSON kind to land on the same error
// message for every shape, including the per-element messages
// (`"input[N] cannot be empty string"` / `"input[N] cannot be empty array"`).
fn validate_embedding_input(input: &Option<Value>) -> TransformerResult<()> {
    match input {
        None | Some(Value::Null) => Err(ConduitError::invalid_request(
            "input cannot be empty string",
        )),
        // String input: Go's default branch checks `strings.TrimSpace(String)`.
        Some(Value::String(s)) => {
            if s.trim().is_empty() {
                return Err(ConduitError::invalid_request(
                    "input cannot be empty string",
                ));
            }
            Ok(())
        }
        // `[]string` or `[]int64` or `[][]int64`: dispatch by inner element
        // shape, matching Go's ordered `UnmarshalJSON` (string-array, then
        // int-array, then int-array-array). An empty array errors at the same
        // spot regardless of element type.
        Some(Value::Array(items)) => {
            if items.is_empty() {
                return Err(ConduitError::invalid_request("input cannot be empty array"));
            }
            // Detect a nested-array shape: any element is itself an array.
            // (Go first tries `[][]int64`; if every inner array is composed of
            // integers this succeeds. If the top-level elements are strings or
            // integers it falls back to `[]string` / `[]int64`. We mirror the
            // resulting per-element validation.)
            let is_nested_array = items.iter().any(|item| matches!(item, Value::Array(_)));
            if is_nested_array {
                for (i, inner) in items.iter().enumerate() {
                    let Value::Array(inner_items) = inner else {
                        // Mixed nested/non-nested: Go's `[][]int64` decode
                        // would fail entirely. Treat as a shape error and
                        // surface a parity-style message.
                        return Err(ConduitError::invalid_request(format!(
                            "input[{i}] cannot be empty array"
                        )));
                    };
                    if inner_items.is_empty() {
                        return Err(ConduitError::invalid_request(format!(
                            "input[{i}] cannot be empty array"
                        )));
                    }
                }
                return Ok(());
            }
            // Flat array: Go validates per-string emptiness for `[]string`;
            // for `[]int64` there is no per-element check (any integer is
            // fine). For mixed shapes Go's `[]string` decode would fail on a
            // non-string element. We apply the per-string trim check only when
            // the element is a JSON string, which matches Go's `[]string`
            // branch and is a no-op for the integer branch.
            for (i, item) in items.iter().enumerate() {
                if let Value::String(s) = item {
                    if s.trim().is_empty() {
                        return Err(ConduitError::invalid_request(format!(
                            "input[{i}] cannot be empty string"
                        )));
                    }
                }
            }
            Ok(())
        }
        // Any other JSON shape (number/object/bool) fails Go's
        // `EmbeddingInput.UnmarshalJSON` with "invalid embedding input type"
        // before `validateEmbeddingInput` is called. We surface a parity-style
        // error here so the inbound 400 stays stable.
        Some(other) => Err(ConduitError::invalid_request(format!(
            "invalid embedding input type: {other}"
        ))),
    }
}

fn request_json_body(request: &HttpRequest) -> TransformerResult<Value> {
    if let Some(json_body) = &request.json_body {
        return Ok(json_body.clone());
    }

    let body = request
        .body
        .as_deref()
        .ok_or_else(|| ConduitError::invalid_request("OpenAI inbound request body is required"))?;

    serde_json::from_slice(body).map_err(|err| {
        ConduitError::invalid_request("OpenAI inbound request body must be valid JSON")
            .with_source(err)
    })
}

fn api_format_from_path(path: &str) -> Option<ApiFormat> {
    match path {
        CHAT_COMPLETIONS_PATH => Some(ApiFormat::OpenAiChatCompletions),
        COMPLETIONS_PATH => Some(ApiFormat::OpenAiCompletions),
        RESPONSES_PATH => Some(ApiFormat::OpenAiResponses),
        RESPONSES_COMPACT_PATH => Some(ApiFormat::OpenAiResponsesCompact),
        EMBEDDINGS_PATH => Some(ApiFormat::OpenAiEmbeddings),
        AUDIO_SPEECH_PATH => Some(ApiFormat::OpenAiAudioSpeech),
        AUDIO_TRANSCRIPTIONS_PATH => Some(ApiFormat::OpenAiAudioTranscriptions),
        AUDIO_TRANSLATIONS_PATH => Some(ApiFormat::OpenAiAudioTranslations),
        IMAGES_GENERATIONS_PATH => Some(ApiFormat::OpenAiImageGeneration),
        IMAGES_EDITS_PATH => Some(ApiFormat::OpenAiImageEdit),
        VIDEOS_PATH => Some(ApiFormat::OpenAiVideo),
        _ => None,
    }
}

fn body_object(body: Value) -> TransformerResult<Map<String, Value>> {
    match body {
        Value::Object(object) => Ok(object),
        _ => Err(ConduitError::invalid_request(
            "OpenAI inbound request body must be a JSON object",
        )),
    }
}

fn take_optional_string(
    object: &mut Map<String, Value>,
    key: &'static str,
) -> TransformerResult<Option<String>> {
    object
        .remove(key)
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| {
            ConduitError::invalid_request(format!("OpenAI inbound field `{key}` must be a string"))
                .with_source(err)
        })
}

fn take_optional_bool(
    object: &mut Map<String, Value>,
    key: &'static str,
) -> TransformerResult<Option<bool>> {
    object
        .remove(key)
        .map(serde_json::from_value)
        .transpose()
        .map_err(|err| {
            ConduitError::invalid_request(format!("OpenAI inbound field `{key}` must be a boolean"))
                .with_source(err)
        })
}

fn deserialize_payload<T>(object: Map<String, Value>) -> TransformerResult<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(Value::Object(object)).map_err(|err| {
        ConduitError::invalid_request("invalid OpenAI inbound request body").with_source(err)
    })
}

// ---------------------------------------------------------------------------
// `InboundTransformer` impl for OpenAI Chat Completions (`/v1/chat/completions`).
// ---------------------------------------------------------------------------
//
// This is the inbound half of Go's `openai.InboundTransformer.TransformRequest`
// (inbound.go) for the chat-completions path. It applies the Go-compatible
// HTTP-layer guards (non-nil request, non-empty body, `application/json`
// content type) and then delegates the body-to-`LlmRequest` translation to
// `normalize_chat_completions_body`, which itself enforces the body-layer
// guards (`model` required, `messages` required & non-empty) and parses the
// payload.
//
// Field coverage follows the Go `Request.ToLLMRequest()` conversion
// (inbound_convert.go): messages (system/developer/user/assistant/tool roles,
// multimodal content parts incl. text/image_url/input_audio/video_url),
// tools/functions, tool_choice (string or named), response_format, stream,
// stream_options, reasoning_effort, plus all other named fields
// (frequency_penalty, logprobs, max_completion_tokens, max_tokens, seed, store,
// temperature, top_p, top_logprobs, presence_penalty, prompt_cache_key,
// safety_identifier, user, logit_bias, metadata, modalities, reasoning_budget,
// reasoning_summary, service_tier, parallel_tool_calls, verbosity, stop) that
// have no first-class `ChatRequest` slot are preserved losslessly through the
// `extra` flatten (`ChatRequest.extra`) — equivalent to Go's named fields but
// without dropping unknown provider extensions.
//
// TODO(RUST-P7-002): responses inbound (`/v1/responses`) — stubbed via
// `normalize_responses_body`.
// TODO(RUST-P7-003): legacy completions inbound (`/v1/completions`) — stubbed
// via `normalize_completions_body`.

/// Inbound transformer for the OpenAI Chat Completions API surface
/// (`POST /v1/chat/completions`). Implements [`InboundTransformer`].
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiChatInbound;

struct OpenAiChatClientStream {
    inner: Box<dyn Iterator<Item = LlmResponse> + Send>,
    saw_terminal_chunk: bool,
    emitted_done: bool,
}

impl OpenAiChatClientStream {
    fn new(inner: Box<dyn Iterator<Item = LlmResponse> + Send>) -> Self {
        Self {
            inner,
            saw_terminal_chunk: false,
            emitted_done: false,
        }
    }
}

impl Iterator for OpenAiChatClientStream {
    type Item = StreamEvent;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(response) = self.inner.next() {
            if response.object == "[DONE]" {
                self.emitted_done = true;
                return Some(StreamEvent {
                    data: Some("[DONE]".to_string()),
                    done: true,
                    ..StreamEvent::default()
                });
            }
            self.saw_terminal_chunk |= response.error.is_some()
                || response
                    .choices
                    .iter()
                    .any(|choice| choice.finish_reason.is_some());
            let data = serde_json::to_string(&response).ok()?;
            return Some(StreamEvent {
                data: Some(data),
                ..StreamEvent::default()
            });
        }

        if self.saw_terminal_chunk && !self.emitted_done {
            self.emitted_done = true;
            return Some(StreamEvent {
                data: Some("[DONE]".to_string()),
                done: true,
                ..StreamEvent::default()
            });
        }
        None
    }
}

impl OpenAiChatInbound {
    pub const fn new() -> Self {
        Self
    }
}

impl InboundTransformer for OpenAiChatInbound {
    fn name(&self) -> &'static str {
        "openai/chat_completions"
    }

    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        // Body presence mirrors Go's `len(httpReq.Body) == 0` guard. We accept
        // either the pre-parsed `json_body` or the raw byte `body`; the helper
        // below enforces at least one is populated.
        let body = request_json_body(&request)?;

        // Content-type guard mirrors Go's `Content-Type` check (inbound.go):
        // the value must contain `application/json`. Fall back from the
        // dedicated `content_type` field to the `Content-Type` header, matching
        // Go's `Headers.Get` lookups.
        let content_type = request
            .content_type
            .as_deref()
            .or_else(|| {
                request.headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type")
                        .then_some(value.as_str())
                })
            })
            .unwrap_or("");
        if !content_type
            .to_ascii_lowercase()
            .contains("application/json")
        {
            return Err(ConduitError::invalid_request(format!(
                "unsupported content type: {content_type}"
            )));
        }

        let mut llm_request = normalize_chat_completions_body(body)?;

        // Carry HTTP-layer context onto the unified request, matching Go's
        // `chatReq.RawRequest = httpReq` propagation and the dispatcher's
        // header/metadata merge done by `normalize_openai_request`.
        llm_request.extra_headers = request.headers;
        llm_request.metadata = request.metadata;
        if let Some(request_id) = request.request_id {
            llm_request
                .metadata
                .insert("request_id".to_string(), Value::String(request_id));
        }
        if let Some(client_ip) = request.client_ip {
            llm_request
                .metadata
                .insert("client_ip".to_string(), Value::String(client_ip));
        }

        Ok(llm_request)
    }

    // The response/stream/error inbound paths are out of scope for
    // RUST-P7-001 (chat inbound). They are stubbed to surface a clear error
    // until the outbound counterparts land.

    fn inbound_response(&self, _response: HttpResponse) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI chat completions inbound response transform is not implemented yet",
        ))
    }

    fn inbound_stream_event(&self, _event: StreamEvent) -> TransformerResult<StreamEvent> {
        Err(ConduitError::internal(
            "OpenAI chat completions inbound stream transform is not implemented yet",
        ))
    }

    fn inbound_error(&self, _error: &ConduitError) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI chat completions inbound error mapping is not implemented yet",
        ))
    }

    fn transform_stream(
        &self,
        events: Box<dyn Iterator<Item = LlmResponse> + Send>,
    ) -> TransformerResult<Box<dyn Iterator<Item = StreamEvent> + Send>> {
        // The client-facing transformer owns the terminal wire protocol.
        // Waiting until the unified iterator reaches EOF keeps a trailing
        // usage-only chunk ahead of `[DONE]`. It also prevents the pipeline
        // from replaying a provider-native terminal (for example
        // `response.completed`) into a Chat Completions response.
        Ok(Box::new(OpenAiChatClientStream::new(events)))
    }

    /// Aggregate provider-side OpenAI streaming chunks into a single
    /// non-streaming chat-completion HTTP response, mirroring Go
    /// `InboundTransformer.AggregateStreamChunks`
    /// (`conduit/llm/transformer/openai/inbound.go:179-184`) which delegates to
    /// the package-level `AggregateStreamChunks`
    /// (`conduit/llm/transformer/openai/aggregator.go:120-388`).
    ///
    /// Pipeline `AutoAggregate` arm calls this when a non-streaming caller hits
    /// a provider that only streams (Go `autoAggregateStream`,
    /// `non_streaming.go:110`). Each [`StreamEvent::data`] SSE frame is decoded
    /// the same way Go's `DefaultTransformChunk` does (a `json.Unmarshal` into
    /// the OpenAI chunk shape, `[DONE]`/error frames filtered out), then the
    /// post-decode [`LlmResponse`]s are folded by
    /// [`aggregate_openai_stream_chunks`].
    ///
    /// The aggregated payload is serialized to JSON bytes and placed on
    /// [`HttpResponse::body`] (matching Go's `httpclient.Response.Body`),
    /// with `Content-Type: application/json` + `Cache-Control: no-cache`
    /// headers (Go `non_streaming.go:122-125`). The original events are also
    /// preserved on `HttpResponse::stream` for downstream retry/debug code.
    fn aggregate_stream_chunks(&self, events: Vec<StreamEvent>) -> TransformerResult<HttpResponse> {
        use crate::openai_stream::{
            aggregate_openai_stream_chunks, openai_sse_chunk_to_llm_response,
        };
        use conduit_llm::LlmResponse;

        // Go `non_streaming.go:105-108`: empty chunks → `ErrEmptyStreamChunks`.
        // We mirror that as an `invalid_request` ConduitError so the pipeline's
        // `ctx.fail("execute:aggregate:transform", err)` surfaces it faithfully.
        if events.is_empty() {
            return Err(ConduitError::invalid_request(
                "cannot aggregate an empty stream chunk list",
            ));
        }

        // Decode each SSE frame, dropping `[DONE]` and propagating parse errors
        // the same way Go's outer loop does (Go silently `continue`s on invalid
        // chunks via `DefaultTransformChunk`; `openai_sse_chunk_to_llm_response`
        // returns `None` for `[DONE]` and an `Err` for genuine parse failures —
        // we mirror Go's skip-on-error by treating `Err` as `None` here so a
        // single malformed frame doesn't abort the whole aggregation).
        let mut chunks: Vec<LlmResponse> = Vec::with_capacity(events.len());
        for event in &events {
            let Some(data) = event.data.as_deref() else {
                continue;
            };
            match openai_sse_chunk_to_llm_response(data, event.event_type.as_deref()) {
                Ok(Some(resp)) => chunks.push(resp),
                Ok(None) => {} // [DONE] sentinel — skip.
                Err(_) => {}   // Go's `DefaultTransformChunk` also `continue`s.
            }
        }

        // Fold decoded chunks into the final non-streaming response. This
        // helper is the byte-for-byte Go `AggregateStreamChunks` port (content
        // concat, tool_call sharding, last-wins usage, finish_reason fallback).
        let aggregated: LlmResponse = aggregate_openai_stream_chunks(&chunks);

        // Go `non_streaming.go:116-119`: empty body → `ErrEmptyAggregatedBody`.
        // Serialize first, then check emptiness (matches Go's `len(body) == 0`
        // post-marshal guard).
        let body = serde_json::to_vec(&aggregated).map_err(|err| {
            ConduitError::internal("failed to marshal aggregated OpenAI response").with_source(err)
        })?;
        if body.is_empty() {
            return Err(ConduitError::internal(
                "aggregated OpenAI response body is empty",
            ));
        }

        // Go constructs the headers map literal (`non_streaming.go:122-125`).
        // `HeaderMap` is an alias for `IndexMap<String, String>` (see
        // `conduit-llm::model`); the keys match Go verbatim.
        let mut headers = conduit_llm::model::HeaderMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Cache-Control".to_string(), "no-cache".to_string());

        Ok(HttpResponse {
            status: 200,
            headers,
            body: Some(body),
            // Preserve the original stream frames so downstream retry/debug
            // code retains a lossless event log (the existing
            // `OutboundTransformer::aggregate_stream` default does the same).
            stream: events,
            ..HttpResponse::default()
        })
    }
}

// TODO(RUST-P7-002): full responses inbound input-item → llm.Message
// conversion (reasoning/function_call/custom_tool_call/function_call_output
// merging, content-item arrays, annotations, web_search_call reconstruction).
// The Go implementation is `conduit/llm/transformer/openai/responses/inbound.go`
// (1083 lines). Until that lands, `OpenAiResponsesInbound` wraps the existing
// `normalize_responses_body` normalizer (which preserves `input` as a raw
// `Value` on `ResponsesRequest.input`) and enforces the Go request-level
// guards: non-empty body, JSON content-type, model-required.

/// Inbound transformer for the OpenAI Responses API surface
/// (`POST /v1/responses` and `POST /v1/responses/compact`). Implements
/// [`InboundTransformer`].
///
/// Mirrors Go `responses.InboundTransformer` (inbound.go:22-166). The
/// request-side guards (nil body, content-type, model-required, JSON decode)
/// and the `api_format` / `request_type` selection (compact vs chat) are
/// byte-compatible with the Go gateway. The COMMON input-item → `LlmMessage`
/// conversion (Go `convertInputToMessages` / `convertReasoningWithFollowing`,
/// inbound.go:251-602) is ported via [`convert_responses_input_to_messages`]
/// (string input, `message`/`input_text` items with text content, `reasoning`
/// items with merge semantics, `function_call` items). The resulting typed
/// messages are attached to `metadata` under
/// [`RESPONSES_INPUT_MESSAGES_METADATA_KEY`] for downstream consumption.
///
/// The remaining exotic item types (`image_generation_call`,
/// `web_search_call`, and any unknown future types) are **intentionally
/// skipped silently — that is Go parity**: Go's item dispatch falls through to
/// `default: return nil, nil` (responses/inbound.go:598-600, :704-705), so no
/// message is produced for them (verified by
/// `s17_unknown_item_types_are_silently_skipped`). For those the raw `input`
/// [`Value`] on [`ResponsesRequest::input`] stays the source of truth so no
/// data is lost.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiResponsesInbound {
    compact: bool,
}

impl OpenAiResponsesInbound {
    pub const fn new() -> Self {
        Self { compact: false }
    }

    /// Whether this inbound transformer should handle the compact variant
    /// (`/v1/responses/compact`). Mirrors Go's path-based dispatch where the
    /// compact path forces `RequestType::Compact` +
    /// `APIFormat::OpenAIResponsesCompact`.
    pub fn compact() -> Self {
        Self { compact: true }
    }
}

impl InboundTransformer for OpenAiResponsesInbound {
    fn name(&self) -> &'static str {
        "openai/responses"
    }

    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        // Body presence mirrors Go's `len(httpReq.Body) == 0` guard
        // (inbound.go:43-45).
        let body = request_json_body(&request)?;

        // Content-type guard mirrors Go inbound.go:48-51: the value must
        // contain `application/json` (empty content-type is accepted, matching
        // Go's `contentType != ""` short-circuit).
        let content_type = request
            .content_type
            .as_deref()
            .or_else(|| {
                request.headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type")
                        .then_some(value.as_str())
                })
            })
            .unwrap_or("");
        if !content_type.is_empty()
            && !content_type
                .to_ascii_lowercase()
                .contains("application/json")
        {
            return Err(ConduitError::invalid_request(format!(
                "unsupported content type: {content_type}"
            )));
        }

        // Compact dispatch mirrors Go's path-based routing: the
        // `/v1/responses/compact` path selects the compact variant. The
        // dispatcher (`normalize_openai_request`) has already resolved the
        // `api_format` from the path, so we read it back here.
        let compact = matches!(request.api_format, Some(ApiFormat::OpenAiResponsesCompact));

        let mut llm_request = normalize_responses_body(body, compact)?;

        // Go's `req.Model == ""` guard (inbound.go:59-61). The normalizer
        // treats `model` as optional (so the chat-completions path can carry
        // model-less requests for some providers), but the Responses API
        // requires it.
        if llm_request
            .model
            .as_deref()
            .map(str::is_empty)
            .unwrap_or(true)
        {
            return Err(ConduitError::invalid_request("model is required"));
        }

        // Carry HTTP-layer context, matching Go's propagation of headers and
        // the dispatcher's metadata merge (same pattern as
        // `OpenAiChatInbound::inbound_request`).
        llm_request.extra_headers = request.headers;
        llm_request.metadata = request.metadata;

        // RUST-P7-001 S17: convert the Responses `input` value into typed
        // `LlmMessage`s for the common item types (string, message, reasoning,
        // function_call). Exotic item types remain deferred (see
        // [`convert_responses_input_to_messages`]); for those the raw `input`
        // [`Value`] on `ResponsesRequest` stays the source of truth. The typed
        // messages are attached to `metadata` under
        // [`RESPONSES_INPUT_MESSAGES_METADATA_KEY`] so downstream outbound
        // transformers can consume them without re-parsing when present.
        let LlmRequestPayload::Responses(ref responses) = llm_request.payload else {
            // Unreachable: `normalize_responses_body` always produces a
            // `Responses` payload variant.
            return Err(ConduitError::internal(
                "expected Responses payload after normalize_responses_body",
            ));
        };
        if let Some(input) = responses.input.as_ref() {
            match convert_responses_input_to_messages(input) {
                Ok(messages) if !messages.is_empty() => {
                    llm_request.metadata.insert(
                        RESPONSES_INPUT_MESSAGES_METADATA_KEY.to_string(),
                        serde_json::to_value(&messages).map_err(|err| {
                            ConduitError::internal("failed to serialize responses input messages")
                                .with_source(err)
                        })?,
                    );
                }
                // Empty result (e.g. an input array consisting solely of
                // deferred exotic item types) is not an error — the raw
                // `input` remains the fallback source of truth.
                Ok(_) => {}
                Err(err) => return Err(err),
            }
        }
        if let Some(request_id) = request.request_id {
            llm_request
                .metadata
                .insert("request_id".to_string(), Value::String(request_id));
        }
        if let Some(client_ip) = request.client_ip {
            llm_request
                .metadata
                .insert("client_ip".to_string(), Value::String(client_ip));
        }

        Ok(llm_request)
    }

    fn inbound_response(&self, response: HttpResponse) -> TransformerResult<HttpResponse> {
        // This legacy hook receives an already Responses-shaped HTTP response.
        // The production pipeline uses `transform_response` below for the
        // unified cross-protocol leg, while same-format responses may safely
        // remain byte-for-byte wire compatible here.
        Ok(response)
    }

    fn inbound_stream_event(&self, event: StreamEvent) -> TransformerResult<StreamEvent> {
        Ok(event)
    }

    fn inbound_error(&self, error: &ConduitError) -> TransformerResult<HttpResponse> {
        crate::openai_responses_inbound::transform_error(error)
    }

    fn transform_response(
        &self,
        response: conduit_llm::LlmResponse,
    ) -> TransformerResult<HttpResponse> {
        crate::openai_responses_inbound::transform_response(response, self.compact)
    }

    fn transform_stream(
        &self,
        events: Box<dyn Iterator<Item = conduit_llm::LlmResponse> + Send>,
    ) -> TransformerResult<Box<dyn Iterator<Item = StreamEvent> + Send>> {
        crate::openai_responses_inbound::transform_stream(events, self.compact)
    }

    fn aggregate_stream_chunks(&self, events: Vec<StreamEvent>) -> TransformerResult<HttpResponse> {
        for event in events.iter().rev() {
            let Some(data) = event.data.as_deref() else {
                continue;
            };
            let value: Value = match serde_json::from_str(data) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("type").and_then(Value::as_str) != Some("response.completed") {
                continue;
            }
            let response = value.get("response").cloned().ok_or_else(|| {
                ConduitError::new(
                    ErrorKind::InvalidResponse,
                    "response.completed is missing response",
                )
            })?;
            let usage = crate::openai_outbound::extract_usage(&response);
            let body = serde_json::to_vec(&response).map_err(|err| {
                ConduitError::internal("failed to serialize completed Responses stream")
                    .with_source(err)
            })?;
            return Ok(HttpResponse {
                status: 200,
                body: Some(body),
                json_body: Some(response),
                usage,
                ..HttpResponse::default()
            });
        }

        Err(ConduitError::new(
            ErrorKind::InvalidResponse,
            "Responses stream is missing response.completed",
        ))
    }
}

// ---------------------------------------------------------------------------
// Dedicated inbound transformers for remaining OpenAI route types.
// Each mirrors the corresponding Go `*InboundTransformer` (embedding_inbound.go,
// audio_inbound.go, image_inbound.go, video_inbound.go) and replaces the
// `OpenAiChatInbound` fallback that the bridge previously used.
// ---------------------------------------------------------------------------

/// Inbound transformer for the OpenAI Embeddings API surface
/// (`POST /v1/embeddings`). Implements [`InboundTransformer`].
///
/// Mirrors Go `EmbeddingInboundTransformer` (embedding_inbound.go). The
/// request-side validation (non-nil body, JSON content-type, model-required,
/// input validation) delegates to [`normalize_embeddings_body`]. Embeddings
/// never stream.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiEmbeddingInbound;

impl OpenAiEmbeddingInbound {
    pub const fn new() -> Self {
        Self
    }
}

impl InboundTransformer for OpenAiEmbeddingInbound {
    fn name(&self) -> &'static str {
        "openai/embeddings"
    }

    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        let body = request_json_body(&request)?;

        // Content-type guard: Go embedding_inbound.go:39-46 — empty content-type
        // defaults to application/json; non-JSON is rejected.
        let content_type = request
            .content_type
            .as_deref()
            .or_else(|| {
                request.headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type")
                        .then_some(value.as_str())
                })
            })
            .unwrap_or("application/json");
        if !content_type
            .to_ascii_lowercase()
            .contains("application/json")
        {
            return Err(ConduitError::invalid_request(format!(
                "unsupported content type: {content_type}"
            )));
        }

        let mut llm_request = normalize_embeddings_body(body)?;

        // Carry HTTP-layer context onto the unified request.
        llm_request.extra_headers = request.headers;
        llm_request.metadata = request.metadata;
        if let Some(request_id) = request.request_id {
            llm_request
                .metadata
                .insert("request_id".to_string(), Value::String(request_id));
        }
        if let Some(client_ip) = request.client_ip {
            llm_request
                .metadata
                .insert("client_ip".to_string(), Value::String(client_ip));
        }

        Ok(llm_request)
    }

    fn inbound_response(&self, _response: HttpResponse) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI embeddings inbound response transform is not implemented yet",
        ))
    }

    fn inbound_stream_event(&self, _event: StreamEvent) -> TransformerResult<StreamEvent> {
        Err(ConduitError::internal(
            "OpenAI embeddings inbound stream transform is not implemented yet",
        ))
    }

    fn inbound_error(&self, _error: &ConduitError) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI embeddings inbound error mapping is not implemented yet",
        ))
    }
}

/// Inbound transformer for the OpenAI Audio Speech API surface
/// (`POST /v1/audio/speech`). Implements [`InboundTransformer`].
///
/// Mirrors Go `AudioInboundTransformer` specialized for speech
/// (audio_inbound.go). The request-side validation (non-nil body, JSON
/// content-type, model-required, input+voice required, stream_format
/// validation) delegates to [`normalize_audio_body`] with
/// `RequestType::Speech`.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiSpeechInbound;

impl OpenAiSpeechInbound {
    pub const fn new() -> Self {
        Self
    }
}

impl InboundTransformer for OpenAiSpeechInbound {
    fn name(&self) -> &'static str {
        "openai/audio_speech"
    }

    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        let body = request_json_body(&request)?;

        // Content-type guard: Go audio_inbound.go mirrors the same pattern
        // as chat/embedding — JSON is required for the speech endpoint.
        let content_type = request
            .content_type
            .as_deref()
            .or_else(|| {
                request.headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type")
                        .then_some(value.as_str())
                })
            })
            .unwrap_or("application/json");
        if !content_type
            .to_ascii_lowercase()
            .contains("application/json")
        {
            return Err(ConduitError::invalid_request(format!(
                "unsupported content type: {content_type}"
            )));
        }

        let mut llm_request = normalize_audio_body(body, RequestType::Speech)?;

        llm_request.extra_headers = request.headers;
        llm_request.metadata = request.metadata;
        if let Some(request_id) = request.request_id {
            llm_request
                .metadata
                .insert("request_id".to_string(), Value::String(request_id));
        }
        if let Some(client_ip) = request.client_ip {
            llm_request
                .metadata
                .insert("client_ip".to_string(), Value::String(client_ip));
        }

        Ok(llm_request)
    }

    fn inbound_response(&self, _response: HttpResponse) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI audio speech inbound response transform is not implemented yet",
        ))
    }

    fn inbound_stream_event(&self, _event: StreamEvent) -> TransformerResult<StreamEvent> {
        Err(ConduitError::internal(
            "OpenAI audio speech inbound stream transform is not implemented yet",
        ))
    }

    fn inbound_error(&self, _error: &ConduitError) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI audio speech inbound error mapping is not implemented yet",
        ))
    }
}

/// Inbound transformer for the OpenAI Image Generation API surface
/// (`POST /v1/images/generations`). Implements [`InboundTransformer`].
///
/// Mirrors Go `ImageGenerationInboundTransformer` (image_inbound.go). The
/// request-side validation (non-nil body, JSON content-type, prompt required,
/// model defaults to `dall-e-2`, stream rejected) delegates to
/// [`normalize_image_body_with_format`] with `ApiFormat::OpenAiImageGeneration`.
/// Images never stream.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiImageGenerationInbound;

impl OpenAiImageGenerationInbound {
    pub const fn new() -> Self {
        Self
    }
}

impl InboundTransformer for OpenAiImageGenerationInbound {
    fn name(&self) -> &'static str {
        "openai/image_generation"
    }

    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        let body = request_json_body(&request)?;

        // Content-type guard: Go image_inbound.go mirrors the same pattern —
        // JSON is required for the generations endpoint. (The multipart edits
        // endpoint uses a different transformer not covered here.)
        let content_type = request
            .content_type
            .as_deref()
            .or_else(|| {
                request.headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type")
                        .then_some(value.as_str())
                })
            })
            .unwrap_or("application/json");
        if !content_type
            .to_ascii_lowercase()
            .contains("application/json")
        {
            return Err(ConduitError::invalid_request(format!(
                "unsupported content type: {content_type}"
            )));
        }

        let mut llm_request = normalize_image_body_with_format(
            body,
            RequestType::Image,
            ApiFormat::OpenAiImageGeneration,
        )?;

        llm_request.extra_headers = request.headers;
        llm_request.metadata = request.metadata;
        if let Some(request_id) = request.request_id {
            llm_request
                .metadata
                .insert("request_id".to_string(), Value::String(request_id));
        }
        if let Some(client_ip) = request.client_ip {
            llm_request
                .metadata
                .insert("client_ip".to_string(), Value::String(client_ip));
        }

        Ok(llm_request)
    }

    fn inbound_response(&self, _response: HttpResponse) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI image generation inbound response transform is not implemented yet",
        ))
    }

    fn inbound_stream_event(&self, _event: StreamEvent) -> TransformerResult<StreamEvent> {
        Err(ConduitError::internal(
            "OpenAI image generation inbound stream transform is not implemented yet",
        ))
    }

    fn inbound_error(&self, _error: &ConduitError) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI image generation inbound error mapping is not implemented yet",
        ))
    }
}

/// Inbound transformer for the OpenAI Video API surface
/// (`POST /v1/videos`). Implements [`InboundTransformer`].
///
/// Mirrors Go `VideoInboundTransformer` (video_inbound.go). The request-side
/// validation (non-nil body, JSON content-type, model required, prompt
/// required) delegates to [`normalize_video_body`]. Videos never stream.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiVideoInbound;

impl OpenAiVideoInbound {
    pub const fn new() -> Self {
        Self
    }
}

impl InboundTransformer for OpenAiVideoInbound {
    fn name(&self) -> &'static str {
        "openai/video"
    }

    fn inbound_request(&self, request: HttpRequest) -> TransformerResult<LlmRequest> {
        let body = request_json_body(&request)?;

        // Content-type guard: Go video_inbound.go accepts either JSON or
        // multipart; under the JSON-view contract the gateway always supplies
        // JSON at this layer, so we enforce JSON here.
        let content_type = request
            .content_type
            .as_deref()
            .or_else(|| {
                request.headers.iter().find_map(|(name, value)| {
                    name.eq_ignore_ascii_case("content-type")
                        .then_some(value.as_str())
                })
            })
            .unwrap_or("application/json");
        if !content_type
            .to_ascii_lowercase()
            .contains("application/json")
        {
            return Err(ConduitError::invalid_request(format!(
                "unsupported content type: {content_type}"
            )));
        }

        let mut llm_request = normalize_video_body(body)?;

        llm_request.extra_headers = request.headers;
        llm_request.metadata = request.metadata;
        if let Some(request_id) = request.request_id {
            llm_request
                .metadata
                .insert("request_id".to_string(), Value::String(request_id));
        }
        if let Some(client_ip) = request.client_ip {
            llm_request
                .metadata
                .insert("client_ip".to_string(), Value::String(client_ip));
        }

        Ok(llm_request)
    }

    fn inbound_response(&self, _response: HttpResponse) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI video inbound response transform is not implemented yet",
        ))
    }

    fn inbound_stream_event(&self, _event: StreamEvent) -> TransformerResult<StreamEvent> {
        Err(ConduitError::internal(
            "OpenAI video inbound stream transform is not implemented yet",
        ))
    }

    fn inbound_error(&self, _error: &ConduitError) -> TransformerResult<HttpResponse> {
        Err(ConduitError::internal(
            "OpenAI video inbound error mapping is not implemented yet",
        ))
    }
}

// ===========================================================================
// RUST-P7-001 S17 — OpenAI Responses `input` → `Vec<LlmMessage>` conversion
// (bounded slice).
//
// Mirrors Go `conduit/llm/transformer/openai/responses/inbound.go`:
//   - `convertInputToMessages`        (inbound.go:329-383)
//   - `convertReasoningWithFollowing` (inbound.go:388-473)
//   - `convertItemToMessage`          (inbound.go:476-602) — common cases only
//   - `convertToMessageContent`       (inbound.go:604-616)
//   - `convertToMessageContentParts`  (inbound.go:646-667)
//   - `convertContentItemToPart`      (inbound.go:670-707)
//
// Bounded scope: the COMMON input-item types are ported losslessly:
//   - whole-string `input`  → single user `LlmMessage`
//   - `type=message`/`input_text`/`""` items with content (string or
//     text-typed structured parts) → `LlmMessage`
//   - `type=reasoning` items → merged into the following assistant message
//     per Go's `convertReasoningWithFollowing` (function_call merge +
//     assistant-text merge + stop-on-non-assistant/unknown)
//   - `type=function_call` items → assistant `tool_calls`
//   - `type=input_image` standalone + content part → image_url part
//     (Go inbound.go:498-517, 687-699)
//   - `type=custom_tool_call` standalone + reasoning merge arm → assistant
//     tool_call with `responses_custom_tool` type (Go inbound.go:430-446,
//     536-556)
//   - `type=function_call_output` / `custom_tool_call_output` → tool-role
//     message (Go inbound.go:558-588)
//   - `type=compaction` / `compaction_summary` standalone + content part →
//     assistant message with a compact-content part (Go inbound.go:595-596,
//     compact.go:37-59)
//
// Only `image_generation_call`, `web_search_call`, and any unknown future
// types remain skipped (matching Go's `default: return nil, nil`); they carry
// a `// TODO(RUST-P7-001 S17):` marker for the follow-up port.
// ==========================================================================

/// Metadata key under which [`convert_responses_input_to_messages`] stores the
/// typed `Vec<LlmMessage>` representation on [`LlmRequest::metadata`] when the
/// inbound transformer wires the converter. Downstream code can check this key
/// to consume the typed messages directly; when absent, the raw `input`
/// [`Value`] on `ResponsesRequest` remains the source of truth (e.g. for the
/// still-deferred `image_generation_call` / `web_search_call` types).
pub const RESPONSES_INPUT_MESSAGES_METADATA_KEY: &str = "openai_responses_messages";

/// Convert the OpenAI Responses API `input` JSON value (string or array of
/// input items) into the unified [`LlmMessage`] slice, mirroring Go
/// `convertInputToMessages` (inbound.go:329-383).
///
/// All common and exotic item types recognized by Go are converted losslessly
/// (`message`, `input_text`, bare-string `content`, `reasoning`,
/// `function_call`, `input_image`, `custom_tool_call`,
/// `function_call_output`, `custom_tool_call_output`, `compaction`,
/// `compaction_summary`). Only `image_generation_call`, `web_search_call`,
/// and any unknown type are silently skipped — each carries a
/// `// TODO(RUST-P7-001 S17):` marker at the dispatch site below. A
/// non-string/non-array `input` is rejected with a parity-style 400 error,
/// matching Go's `Input.UnmarshalJSON`.
pub fn convert_responses_input_to_messages(input: &Value) -> TransformerResult<Vec<LlmMessage>> {
    match input {
        // Go `convertInputToMessages`: nil input yields no messages.
        Value::Null => Ok(Vec::new()),
        // Go `input.Text != nil` shortcut: single user message with the
        // string as literal content.
        Value::String(text) => Ok(vec![LlmMessage {
            role: Some("user".to_string()),
            content: Some(MessageContent::Text(text.clone())),
            ..Default::default()
        }]),
        // Go `input.Items` array path: iterate and dispatch per item type.
        Value::Array(items) => convert_responses_input_items(items),
        // Go's `Input.UnmarshalJSON` rejects any shape that is neither string
        // nor array with `"invalid input"`. We surface the same parity-style
        // 400 here.
        other => Err(ConduitError::invalid_request(format!(
            "OpenAI Responses `input` must be a string or array of items (got {})",
            other
        ))),
    }
}

/// Iterate the Responses `input` items array, dispatching each item to the
/// matching conversion arm. Mirrors Go `convertInputToMessages` array branch
/// (inbound.go:347-382). `reasoning` items are special-cased through
/// [`convert_reasoning_with_following`] which may consume subsequent items.
fn convert_responses_input_items(items: &[Value]) -> TransformerResult<Vec<LlmMessage>> {
    let mut messages = Vec::with_capacity(items.len());
    let mut i = 0;
    while i < items.len() {
        let item_type = items[i].get("type").and_then(|v| v.as_str()).unwrap_or("");
        if item_type == "reasoning" {
            // Go `convertInputToMessages`: reasoning is handled by the
            // look-ahead helper which may merge following function_call /
            // assistant-text items into a single assistant message.
            let (msg_opt, consumed) = convert_reasoning_with_following(items, i)?;
            if let Some(msg) = msg_opt {
                messages.push(msg);
            }
            // `consumed` is always >= 1 for a reasoning start item, so the
            // loop always advances.
            i += consumed;
            continue;
        }
        if let Some(msg) = convert_item_to_message(&items[i])? {
            messages.push(msg);
        }
        i += 1;
    }
    Ok(messages)
}

/// Convert a reasoning item at `start_idx` into an assistant [`LlmMessage`],
/// then look ahead and merge any immediately-following `function_call` items
/// (and a single assistant `message`/`input_text` text item) into the same
/// message. Mirrors Go `convertReasoningWithFollowing` (inbound.go:388-473).
///
/// Returns `(Option<LlmMessage>, consumed)` where `consumed` is the number of
/// items from `start_idx` that were folded into the produced message. The
/// reasoning item alone always yields a message (even with empty summary),
/// matching Go which returns a non-nil `*llm.Message` as soon as it sees a
/// `reasoning` item.
fn convert_reasoning_with_following(
    items: &[Value],
    start_idx: usize,
) -> TransformerResult<(Option<LlmMessage>, usize)> {
    if start_idx >= items.len() {
        return Ok((None, 0));
    }
    let reasoning_item = &items[start_idx];
    if reasoning_item
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        != "reasoning"
    {
        return Ok((None, 0));
    }

    // Start the merged assistant message. `reasoning_signature` carries the
    // Go `ReasoningSignature` (encrypted_content) field verbatim.
    let mut msg = LlmMessage {
        role: Some("assistant".to_string()),
        reasoning_signature: reasoning_item
            .get("encrypted_content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        ..Default::default()
    };

    // Concatenate `summary[].text` items into a single `reasoning_content`
    // string, mirroring Go's `strings.Builder` accumulation
    // (inbound.go:400-408).
    let mut reasoning_text = String::new();
    if let Some(Value::Array(summaries)) = reasoning_item.get("summary") {
        for summary in summaries {
            if let Some(text) = summary.get("text").and_then(|v| v.as_str()) {
                reasoning_text.push_str(text);
            }
        }
    }
    if !reasoning_text.is_empty() {
        msg.reasoning_content = Some(reasoning_text);
    }

    let mut consumed = 1usize;

    // Look ahead and merge eligible following items.
    for j in (start_idx + 1)..items.len() {
        let next = &items[j];
        let next_type = next.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match next_type {
            "function_call" => {
                // Go merges function_call items into the same assistant
                // message's ToolCalls slice.
                if let Some(tool_call) = convert_function_call_to_tool_call(next) {
                    msg.tool_calls.push(tool_call);
                }
                consumed += 1;
            }
            // Go inbound.go:430-446: custom_tool_call items merge into the
            // same assistant message's ToolCalls slice with a
            // `ResponseCustomToolCall` payload (mirrored via `extra` on the
            // unified Rust `ToolCall`).
            "custom_tool_call" => {
                if let Some(tool_call) = convert_custom_tool_call_to_tool_call(next) {
                    msg.tool_calls.push(tool_call);
                }
                consumed += 1;
            }
            // Go `case "message", "input_text", "":` — merge assistant-role
            // text content into the running message; stop on any other role.
            "message" | "input_text" | "" => {
                let role = next.get("role").and_then(|v| v.as_str()).unwrap_or("");
                if role == "assistant" {
                    if let Some(id) = next.get("id").and_then(|v| v.as_str()) {
                        msg.id = Some(id.to_string());
                    }
                    if let Some(content) = convert_item_content_to_message_content(next)? {
                        msg.content = Some(content);
                    }
                    consumed += 1;
                } else {
                    // Non-assistant message: stop merging, the caller's outer
                    // loop will emit it as a standalone message.
                    break;
                }
            }
            // Any other type (function_call_output, custom_tool_call_output,
            // compaction, image_generation_call, web_search_call, …) stops the
            // merge per Go's `default: return msg, consumed, nil`.
            _ => break,
        }
    }

    Ok((Some(msg), consumed))
}

/// Convert a single Responses input item to an [`LlmMessage`], mirroring Go
/// `convertItemToMessage` (inbound.go:476-602) for the common item types.
///
/// Returns `Ok(None)` for unknown / deferred item types so the caller can
/// simply skip them, matching Go's `default: return nil, nil`.
fn convert_item_to_message(item: &Value) -> TransformerResult<Option<LlmMessage>> {
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match item_type {
        "message" | "input_text" | "" => {
            // Go builds the message with `item.ID` and `item.Role` verbatim.
            let msg = LlmMessage {
                id: item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                role: item
                    .get("role")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                content: convert_item_content_to_message_content(item)?,
                ..Default::default()
            };
            Ok(Some(msg))
        }
        // Go inbound.go:498-517: standalone input_image item → user message
        // (role defaults to "user" when blank) with a single `image_url`
        // content part. Items without an `image_url` are dropped (return nil),
        // matching Go's `return nil, nil` short-circuit.
        "input_image" => {
            let Some(image_url) = item.get("image_url").and_then(|v| v.as_str()) else {
                return Ok(None);
            };
            let role = item
                .get("role")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("user");
            let mut url_obj = Map::new();
            url_obj.insert("url".to_string(), Value::String(image_url.to_string()));
            if let Some(detail) = item.get("detail").and_then(|v| v.as_str()) {
                if !detail.is_empty() {
                    url_obj.insert("detail".to_string(), Value::String(detail.to_string()));
                }
            }
            Ok(Some(LlmMessage {
                role: Some(role.to_string()),
                content: Some(MessageContent::Parts(vec![ContentPart {
                    part_type: "image_url".to_string(),
                    image_url: Some(Value::Object(url_obj)),
                    ..Default::default()
                }])),
                ..Default::default()
            }))
        }
        "function_call" => {
            // Function call from assistant — convert to a tool_call-bearing
            // assistant message. Mirrors Go inbound.go:519-534.
            let Some(tool_call) = convert_function_call_to_tool_call(item) else {
                return Ok(None);
            };
            Ok(Some(LlmMessage {
                role: Some("assistant".to_string()),
                tool_calls: vec![tool_call],
                ..Default::default()
            }))
        }
        // Go inbound.go:536-556: custom_tool_call → assistant message with a
        // single tool call whose type is `responses_custom_tool` and whose
        // `ResponseCustomToolCall` payload (call_id/name/input) rides on
        // `extra` since the Rust unified `ToolCall` has no first-class slot.
        "custom_tool_call" => {
            let Some(tool_call) = convert_custom_tool_call_to_tool_call(item) else {
                return Ok(None);
            };
            Ok(Some(LlmMessage {
                role: Some("assistant".to_string()),
                tool_calls: vec![tool_call],
                ..Default::default()
            }))
        }
        // Go inbound.go:558-572: function_call_output → tool-role message
        // carrying `tool_call_id` + content converted from `Output`. The Go
        // path rejects a nil `Output` with `ErrInvalidRequest`; we mirror.
        "function_call_output" => convert_tool_call_output_message(item),
        // Go inbound.go:574-588: custom_tool_call_output → same shape as
        // function_call_output (tool-role message with `tool_call_id` +
        // `Name` preserved on `tool_call_name`).
        "custom_tool_call_output" => convert_tool_call_output_message(item),
        // `reasoning` is handled by `convert_reasoning_with_following` in the
        // outer loop; if it reaches here the Go branch also returns nil.
        "reasoning" => Ok(None),
        // Go inbound.go:595-596 + compact.go:37-51: compaction /
        // compaction_summary → assistant message carrying a single compact
        // content part (type echoes the item type; compact payload in
        // `extra["compact"]`).
        "compaction" | "compaction_summary" => Ok(Some(LlmMessage {
            id: item
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            role: Some("assistant".to_string()),
            content: Some(MessageContent::Parts(vec![ContentPart {
                part_type: item_type.to_string(),
                extra: compact_content_part_extra(item),
                ..Default::default()
            }])),
            ..Default::default()
        })),
        // `image_generation_call`, `web_search_call`, and any future exotic
        // item types: intentional silent skip, Go parity — Go's dispatch ends
        // in `default: return nil, nil` (responses/inbound.go:598-600,
        // :704-705). Verified by `s17_unknown_item_types_are_silently_skipped`.
        _ => Ok(None),
    }
}

/// Build a unified [`ToolCall`] from a Responses `function_call` item,
/// mirroring Go inbound.go:519-534 / 419-428. The function sub-object carries
/// `name`, `arguments`, and `namespace` exactly as Go's `FunctionCall` struct.
/// Returns `None` when the item lacks a `call_id` (treated as malformed).
fn convert_function_call_to_tool_call(item: &Value) -> Option<ToolCall> {
    let call_id = item.get("call_id").and_then(|v| v.as_str())?;
    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = item.get("arguments").and_then(|v| v.as_str()).unwrap_or("");
    let namespace = item.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let mut function = Map::new();
    function.insert("name".to_string(), Value::String(name.to_string()));
    function.insert(
        "arguments".to_string(),
        Value::String(arguments.to_string()),
    );
    if !namespace.is_empty() {
        function.insert(
            "namespace".to_string(),
            Value::String(namespace.to_string()),
        );
    }
    Some(ToolCall {
        id: Some(call_id.to_string()),
        call_type: "function".to_string(),
        function: Value::Object(function),
        ..Default::default()
    })
}

/// Build a unified [`ToolCall`] from a Responses `custom_tool_call` item,
/// mirroring Go inbound.go:536-556 / 430-446. The Rust unified `ToolCall` has
/// no first-class `ResponseCustomToolCall` slot, so the structured payload
/// (`call_id` / `name` / `input`) is preserved on `extra` under the
/// `response_custom_tool_call` key (lossless round-trip for downstream
/// outbound transformers). Returns `None` when the item lacks a `call_id`.
fn convert_custom_tool_call_to_tool_call(item: &Value) -> Option<ToolCall> {
    let call_id = item.get("call_id").and_then(|v| v.as_str())?;
    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let input_str = item.get("input").and_then(|v| v.as_str()).unwrap_or("");
    let mut response_custom_tool_call = Map::new();
    response_custom_tool_call.insert("call_id".to_string(), Value::String(call_id.to_string()));
    response_custom_tool_call.insert("name".to_string(), Value::String(name.to_string()));
    response_custom_tool_call.insert("input".to_string(), Value::String(input_str.to_string()));
    let mut extra = BTreeMap::new();
    extra.insert(
        "response_custom_tool_call".to_string(),
        Value::Object(response_custom_tool_call),
    );
    Some(ToolCall {
        id: Some(call_id.to_string()),
        call_type: "responses_custom_tool".to_string(),
        function: Value::Object(Map::new()),
        extra,
        ..Default::default()
    })
}

/// Convert a `function_call_output` / `custom_tool_call_output` item into a
/// tool-role [`LlmMessage`], mirroring Go inbound.go:558-588. The `Output`
/// field is mandatory (Go rejects `nil` with `ErrInvalidRequest`); the
/// `tool_call_id` carries `call_id` and `tool_call_name` carries `name` when
/// present. `Output` may be a string or an array of content items, reusing
/// the shared content-conversion path.
fn convert_tool_call_output_message(item: &Value) -> TransformerResult<Option<LlmMessage>> {
    let Some(output) = item.get("output") else {
        return Err(ConduitError::invalid_request(format!(
            "{} item must have non-nil Output",
            item.get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("tool_call_output")
        )));
    };
    // Reuse the shared content conversion by treating the `output` value as
    // the `content` of a synthetic message-shaped Value. We build the inner
    // content via the same path Go's `convertToMessageContent(*item.Output)`
    // takes (Input.Text shortcut, then Input.Items array).
    let synthetic = serde_json::json!({"content": output});
    let content = convert_item_content_to_message_content(&synthetic)?;
    let msg = LlmMessage {
        role: Some("tool".to_string()),
        tool_call_id: item
            .get("call_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        tool_call_name: item
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string()),
        content,
        ..Default::default()
    };
    Ok(Some(msg))
}

/// Convert a message/input_text item's `content` field (which may be a bare
/// string or an array of structured content items) into a [`MessageContent`],
/// mirroring Go `convertToMessageContent` (inbound.go:604-616) and the
/// preceding `isOutputMessageContent` / `GetContentItems` dispatch
/// (inbound.go:488-495).
///
/// Common content shapes handled losslessly:
///   - `content` is a string → `MessageContent::Text`
///   - `content` is an array of `{type:"input_text"|"text"|"output_text",
///     text:"..."}` items → collapses to `MessageContent::Text` when a single
///     text item is present, otherwise `MessageContent::Parts`
///
/// Non-text content-item types (input_image, compaction, …) are silently
/// skipped here — each TODO marker lives at the dispatch in
/// [`convert_content_item_to_part`].
fn convert_item_content_to_message_content(
    item: &Value,
) -> TransformerResult<Option<MessageContent>> {
    // Go checks `item.Text` first as a fallback when `Content` is nil
    // (inbound.go:493-495).
    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
        return Ok(Some(MessageContent::Text(text.to_string())));
    }
    let Some(content) = item.get("content") else {
        return Ok(None);
    };
    // Bare-string content → Input.Text path → single "input_text" part →
    // collapses to Content.Content (Go `convertToMessageContentParts`).
    if let Some(text) = content.as_str() {
        return Ok(Some(MessageContent::Text(text.to_string())));
    }
    let Some(parts) = content.as_array() else {
        // Non-string / non-array content: treat as opaque JSON fallback so the
        // data round-trips losslessly. (Go would have failed the typed decode
        // earlier; we surface the value via the Json variant instead.)
        return Ok(Some(MessageContent::Json(content.clone())));
    };
    let mut converted: Vec<ContentPart> = Vec::with_capacity(parts.len());
    for raw in parts {
        if let Some(part) = convert_content_item_to_part(raw)? {
            converted.push(part);
        }
    }
    // Go `convertToMessageContent`: a single text part collapses to a bare
    // Content.Content string (inbound.go:607-611). We mirror that by returning
    // `MessageContent::Text` when the parts slice holds exactly one text-typed
    // part with a non-None `text` slot.
    if converted.len() == 1 {
        match converted[0].part_type.as_str() {
            "text" | "input_text" => {
                if let Some(text) = converted[0].text.clone() {
                    return Ok(Some(MessageContent::Text(text)));
                }
            }
            _ => {}
        }
    }
    if converted.is_empty() {
        Ok(None)
    } else {
        Ok(Some(MessageContent::Parts(converted)))
    }
}

/// Convert a single Responses content-item object to a unified [`ContentPart`],
/// mirroring Go `convertContentItemToPart` (inbound.go:670-707) for the common
/// text shapes. Non-text item types are deferred with TODO markers and return
/// `Ok(None)` so the caller skips them.
/// Build the `extra` map carrying the optional `id` field for a content part.
/// The unified [`ContentPart`] has no first-class `id` slot (it mirrors the
/// chat-completions content-part shape); the Responses `id` round-trips via
/// the `extra` flatten so downstream consumers can still access it.
fn content_part_id_extra(item: &Value) -> BTreeMap<String, Value> {
    let mut extra = BTreeMap::new();
    if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
        extra.insert("id".to_string(), Value::String(id.to_string()));
    }
    extra
}

fn convert_content_item_to_part(item: &Value) -> TransformerResult<Option<ContentPart>> {
    let item_type = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match item_type {
        "input_text" | "text" | "output_text" => {
            // Go inbound.go:676-685: text part with `item.Text`. The `id` is
            // carried onto the part for round-tripping.
            let text = item
                .get("text")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if text.is_none() {
                return Ok(None);
            }
            Ok(Some(ContentPart {
                part_type: "text".to_string(),
                text,
                extra: content_part_id_extra(item),
                ..Default::default()
            }))
        }
        // Go inbound.go:687-699: input_image content part → image_url part
        // with `url` + `detail`. The Rust `ContentPart` models `image_url` as
        // an opaque JSON `Value` (mirroring the chat-completions shape), so we
        // build the same `{url, detail}` object Go carries on `llm.ImageURL`.
        "input_image" => {
            let Some(image_url) = item.get("image_url").and_then(|v| v.as_str()) else {
                return Ok(None);
            };
            let mut url_obj = Map::new();
            url_obj.insert("url".to_string(), Value::String(image_url.to_string()));
            if let Some(detail) = item.get("detail").and_then(|v| v.as_str()) {
                if !detail.is_empty() {
                    url_obj.insert("detail".to_string(), Value::String(detail.to_string()));
                }
            }
            Ok(Some(ContentPart {
                part_type: "image_url".to_string(),
                image_url: Some(Value::Object(url_obj)),
                extra: content_part_id_extra(item),
                ..Default::default()
            }))
        }
        // Go compact.go:53-59 + compactionContentFromItem: a compaction
        // content part carries `id` / `encrypted_content` / `created_by` on
        // the `Compact` sub-struct. The Rust unified `ContentPart` has no
        // first-class compact slot, so the same data round-trips via `extra`
        // under a reserved `compact` key (lossless for downstream consumers).
        "compaction" | "compaction_summary" => Ok(Some(ContentPart {
            part_type: item_type.to_string(),
            extra: compact_content_part_extra(item),
            ..Default::default()
        })),
        _ => Ok(None),
    }
}

/// Build the `extra` map for a compaction content part, mirroring Go
/// `compactionContentFromItem` (compact.go:29-35). The compact payload
/// (`id` / `encrypted_content` / `created_by`) is stored under a `compact`
/// key in the part's `extra` flatten, since the Rust unified `ContentPart`
/// has no first-class compact slot.
fn compact_content_part_extra(item: &Value) -> BTreeMap<String, Value> {
    let mut extra = BTreeMap::new();
    let mut compact = Map::new();
    if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
        compact.insert("id".to_string(), Value::String(id.to_string()));
    }
    if let Some(enc) = item.get("encrypted_content").and_then(|v| v.as_str()) {
        compact.insert(
            "encrypted_content".to_string(),
            Value::String(enc.to_string()),
        );
    }
    if let Some(created_by) = item.get("created_by").and_then(|v| v.as_str()) {
        compact.insert(
            "created_by".to_string(),
            Value::String(created_by.to_string()),
        );
    }
    extra.insert("compact".to_string(), Value::Object(compact));
    extra
}

#[cfg(test)]
mod tests {
    use conduit_llm::{Choice, MessageContent, Usage};
    use serde_json::json;

    use super::*;

    #[test]
    fn chat_client_stream_owns_done_after_trailing_usage_chunk() -> TransformerResult<()> {
        let chunks = vec![
            LlmResponse {
                id: "chatcmpl_test".to_string(),
                choices: vec![Choice {
                    finish_reason: Some("stop".to_string()),
                    ..Choice::default()
                }],
                ..LlmResponse::default()
            },
            LlmResponse {
                id: "chatcmpl_test".to_string(),
                usage: Some(Usage {
                    total_tokens: 3,
                    ..Usage::default()
                }),
                ..LlmResponse::default()
            },
        ];

        let events: Vec<_> = OpenAiChatInbound::new()
            .transform_stream(Box::new(chunks.into_iter()))?
            .collect();

        assert_eq!(events.len(), 3);
        assert!(
            events[1]
                .data
                .as_deref()
                .and_then(|data| serde_json::from_str::<Value>(data).ok())
                .is_some_and(|value| value["usage"]["total_tokens"] == 3)
        );
        assert_eq!(events[2].data.as_deref(), Some("[DONE]"));
        assert!(events[2].done);
        Ok(())
    }

    #[test]
    fn chat_completions_body_normalizes_basic_fields_and_preserves_extras() -> TransformerResult<()>
    {
        let request = normalize_chat_completions_body(json!({
            "model": "gpt-4o-mini",
            "stream": true,
            "messages": [
                {"role": "system", "content": "be direct"},
                {"role": "user", "content": [{"type": "text", "text": "hello"}]}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "look up a value",
                    "parameters": {"type": "object"}
                }
            }],
            "tool_choice": "auto",
            "response_format": {"type": "json_object"},
            "stream_options": {"include_usage": true},
            "temperature": 0.2,
            "provider_flag": {"kept": true}
        }))?;

        assert_eq!(request.request_type, RequestType::Chat);
        assert_eq!(request.api_format, ApiFormat::OpenAiChatCompletions);
        assert_eq!(request.model.as_deref(), Some("gpt-4o-mini"));
        assert!(request.stream);

        let LlmRequestPayload::Chat(payload) = request.payload else {
            panic!("expected chat payload");
        };
        assert_eq!(payload.messages.len(), 2);
        assert_eq!(payload.messages[0].role, "system");
        assert_eq!(
            payload.messages[0].content,
            Some(MessageContent::Text("be direct".to_string()))
        );
        assert_eq!(payload.tools.len(), 1);
        assert_eq!(payload.tool_choice, Some(json!("auto")));
        assert_eq!(
            payload.response_format,
            Some(json!({"type": "json_object"}))
        );
        assert_eq!(payload.stream_options, Some(json!({"include_usage": true})));
        assert_eq!(payload.temperature, Some(0.2));
        assert_eq!(
            payload.extra.get("provider_flag"),
            Some(&json!({"kept": true}))
        );
        assert!(!payload.extra.contains_key("model"));
        assert!(!payload.extra.contains_key("stream"));
        Ok(())
    }

    // ---- S12: inbound request validation parity (inbound.go) ----

    // 缺 `model` 必须拒绝（Go: "model is required"）。
    #[test]
    fn chat_completions_rejects_missing_model() {
        match normalize_chat_completions_body(json!({
            "messages": [{"role": "user", "content": "hi"}]
        })) {
            Err(err) => assert!(err.to_string().contains("model is required")),
            Ok(_) => panic!("expected model-required error"),
        }
    }

    // 空 messages 数组必须拒绝（Go: `len(oaiReq.Messages) == 0`）。
    #[test]
    fn chat_completions_rejects_empty_messages() {
        match normalize_chat_completions_body(json!({
            "model": "gpt-4o-mini",
            "messages": []
        })) {
            Err(err) => assert!(err.to_string().contains("messages")),
            Ok(_) => panic!("expected empty-messages error"),
        }
    }

    // messages 缺省（None）同样拒绝。
    #[test]
    fn chat_completions_rejects_absent_messages() {
        match normalize_chat_completions_body(json!({ "model": "gpt-4o-mini" })) {
            Err(err) => assert!(err.to_string().contains("messages")),
            Ok(_) => panic!("expected absent-messages error"),
        }
    }

    // `stream_options` 非对象必须拒绝（Go 解码进 typed `StreamOptions` struct）。
    #[test]
    fn chat_completions_rejects_non_object_stream_options() {
        match normalize_chat_completions_body(json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "hi"}],
            "stream_options": "yes"
        })) {
            Err(err) => assert!(err.to_string().contains("stream_options")),
            Ok(_) => panic!("expected stream_options shape error"),
        }
    }

    // `stream_options.include_usage` 非布尔必须拒绝。
    #[test]
    fn chat_completions_rejects_non_boolean_include_usage() {
        match normalize_chat_completions_body(json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "hi"}],
            "stream_options": {"include_usage": "true"}
        })) {
            Err(err) => assert!(err.to_string().contains("include_usage")),
            Ok(_) => panic!("expected include_usage boolean error"),
        }
    }

    // 合法 `stream_options`（含未知 provider 扩展键）应通过。
    #[test]
    fn chat_completions_accepts_well_formed_stream_options() -> TransformerResult<()> {
        let _ = normalize_chat_completions_body(json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true,
            "stream_options": {"include_usage": true, "provider_extra": 7}
        }))?;
        Ok(())
    }

    #[test]
    fn completions_body_normalizes_legacy_prompt_request() -> TransformerResult<()> {
        let request = normalize_completions_body(json!({
            "model": "gpt-3.5-turbo-instruct",
            "prompt": ["first", "second"],
            "suffix": "done",
            "max_tokens": 32,
            "top_p": 0.9,
            "stop": ["\n\n"],
            "logprobs": 2
        }))?;

        assert_eq!(request.request_type, RequestType::Completion);
        assert_eq!(request.api_format, ApiFormat::OpenAiCompletions);
        assert_eq!(request.model.as_deref(), Some("gpt-3.5-turbo-instruct"));
        assert!(!request.stream);

        let LlmRequestPayload::Completion(payload) = request.payload else {
            panic!("expected completion payload");
        };
        assert_eq!(payload.prompt, Some(json!(["first", "second"])));
        assert_eq!(payload.suffix.as_deref(), Some("done"));
        assert_eq!(payload.max_tokens, Some(32));
        assert_eq!(payload.top_p, Some(0.9));
        assert_eq!(payload.stop, Some(json!(["\n\n"])));
        assert_eq!(payload.extra.get("logprobs"), Some(&json!(2)));
        Ok(())
    }

    #[test]
    fn responses_body_normalizes_input_and_tools() -> TransformerResult<()> {
        let request = normalize_responses_body(
            json!({
                "model": "gpt-4.1",
                "stream": true,
                "input": "summarize this",
                "instructions": "keep it short",
                "previous_response_id": "resp_previous",
                "reasoning": {"effort": "low"},
                "tools": [{"type": "web_search_preview"}],
                "response_format": {"type": "json_object"},
                "parallel_tool_calls": true
            }),
            false,
        )?;

        assert_eq!(request.request_type, RequestType::Chat);
        assert_eq!(request.api_format, ApiFormat::OpenAiResponses);
        assert_eq!(request.model.as_deref(), Some("gpt-4.1"));
        assert!(request.stream);

        let LlmRequestPayload::Responses(payload) = request.payload else {
            panic!("expected responses payload");
        };
        assert_eq!(payload.input, Some(json!("summarize this")));
        assert_eq!(payload.instructions.as_deref(), Some("keep it short"));
        assert_eq!(
            payload.previous_response_id.as_deref(),
            Some("resp_previous")
        );
        assert_eq!(payload.reasoning, Some(json!({"effort": "low"})));
        assert_eq!(
            payload.tools,
            vec![UnifiedTool {
                tool_type: "web_search_preview".to_string(),
                name: None,
                description: None,
                parameters: None,
                extra: Default::default(),
            }]
        );
        assert_eq!(
            payload.response_format,
            Some(json!({"type": "json_object"}))
        );
        assert!(!payload.compact);
        assert_eq!(payload.extra.get("parallel_tool_calls"), Some(&json!(true)));
        Ok(())
    }

    #[test]
    fn responses_compact_path_forces_compact_request_type() -> TransformerResult<()> {
        let request = normalize_openai_request(HttpRequest {
            method: "POST".to_string(),
            path: RESPONSES_COMPACT_PATH.to_string(),
            body: Some(br#"{"model":"gpt-4.1","input":[{"role":"user","content":"hi"}]}"#.to_vec()),
            ..HttpRequest::default()
        })?;

        assert_eq!(request.request_type, RequestType::Compact);
        assert_eq!(request.api_format, ApiFormat::OpenAiResponsesCompact);
        assert_eq!(request.model.as_deref(), Some("gpt-4.1"));

        let LlmRequestPayload::Responses(payload) = request.payload else {
            panic!("expected responses payload");
        };
        assert!(payload.compact);
        assert_eq!(
            payload.input,
            Some(json!([{"role": "user", "content": "hi"}]))
        );
        Ok(())
    }

    // =======================================================================
    // OpenAiResponsesInbound — mirrors Go `responses.InboundTransformer`
    // (inbound.go:22-166) request-side guards + the image-generation-result
    // field round-trip that was previously untested.
    // =======================================================================

    #[test]
    fn responses_body_preserves_image_generation_result() -> TransformerResult<()> {
        // Mirrors the Go `Request.ImageGenerationResult` field (model.go) —
        // the inbound request carries the result of a prior image-generation
        // tool call, and the typed `ResponsesRequest` must round-trip it
        // losslessly. This is the assertion the parity audit flagged as
        // missing.
        let request = normalize_responses_body(
            json!({
                "model": "gpt-4o",
                "input": "describe this image",
                "image_generation_result": {
                    "result": "iVBORw0KGgoAAAANSUhEUg=="
                }
            }),
            false,
        )?;

        let LlmRequestPayload::Responses(payload) = request.payload else {
            panic!("expected responses payload");
        };
        assert_eq!(
            payload.image_generation_result,
            Some(json!({"result": "iVBORw0KGgoAAAANSUhEUg=="}))
        );
        Ok(())
    }

    #[test]
    fn responses_inbound_rejects_nil_body() {
        // Mirrors Go inbound.go:39-45 ("nil request" + "empty body" cases).
        let inbound = OpenAiResponsesInbound::new();
        let err = match inbound.inbound_request(HttpRequest::default()) {
            Ok(_) => panic!("expected Err for empty body"),
            Err(e) => e,
        };
        assert!(
            err.message.contains("body is required") || err.message.contains("body"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn responses_inbound_rejects_invalid_json() {
        let inbound = OpenAiResponsesInbound::new();
        let err = match inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            body: Some(b"{invalid json}".to_vec()),
            ..HttpRequest::default()
        }) {
            Ok(_) => panic!("expected Err for invalid JSON"),
            Err(e) => e,
        };
        assert!(err.message.contains("valid JSON"), "got: {}", err.message);
    }

    #[test]
    fn responses_inbound_rejects_missing_model() {
        // Mirrors Go inbound.go:59-61 ("missing model" case).
        let inbound = OpenAiResponsesInbound::new();
        let err = match inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            json_body: Some(json!({"input": "Hello"})),
            ..HttpRequest::default()
        }) {
            Ok(_) => panic!("expected Err for missing model"),
            Err(e) => e,
        };
        assert!(
            err.message.contains("model is required"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn responses_inbound_rejects_unsupported_content_type() {
        // Mirrors Go inbound.go:48-51 — content-type must contain
        // application/json (when non-empty).
        let inbound = OpenAiResponsesInbound::new();
        let err = match inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            content_type: Some("text/plain".to_string()),
            json_body: Some(json!({"model": "gpt-4o", "input": "hi"})),
            ..HttpRequest::default()
        }) {
            Ok(_) => panic!("expected Err for text/plain content type"),
            Err(e) => e,
        };
        assert!(
            err.message.contains("unsupported content type"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn responses_inbound_accepts_empty_content_type() -> TransformerResult<()> {
        // Mirrors Go's `contentType != ""` short-circuit: empty content-type
        // is accepted (treated as JSON).
        let inbound = OpenAiResponsesInbound::new();
        let req = inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            json_body: Some(json!({"model": "gpt-4o", "input": "hi"})),
            ..HttpRequest::default()
        })?;
        assert_eq!(req.model.as_deref(), Some("gpt-4o"));
        Ok(())
    }

    #[test]
    fn responses_inbound_normalizes_simple_text_input() -> TransformerResult<()> {
        // Mirrors Go "simple text input" case (inbound_test.go:59-73). The
        // input value is preserved on `ResponsesRequest.input`; the full Go
        // transformer additionally converts it to a `Messages` slice, but
        // that conversion is a deferred follow-up (see module docs).
        let inbound = OpenAiResponsesInbound::new();
        let req = inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            json_body: Some(json!({
                "model": "gpt-4o",
                "input": "Hello, world!"
            })),
            ..HttpRequest::default()
        })?;

        assert_eq!(req.request_type, RequestType::Chat);
        assert_eq!(req.api_format, ApiFormat::OpenAiResponses);
        assert_eq!(req.model.as_deref(), Some("gpt-4o"));

        let LlmRequestPayload::Responses(payload) = req.payload else {
            panic!("expected responses payload");
        };
        assert_eq!(
            payload.input.as_ref().and_then(|v| v.as_str()),
            Some("Hello, world!")
        );
        Ok(())
    }

    #[test]
    fn responses_inbound_preserves_instructions_and_previous_response_id() -> TransformerResult<()>
    {
        // Mirrors Go "request with instructions" + "request with
        // previous_response_id" cases.
        let inbound = OpenAiResponsesInbound::new();
        let req = inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            json_body: Some(json!({
                "model": "gpt-4o",
                "instructions": "You are a helpful assistant.",
                "previous_response_id": "resp_prev_123",
                "input": "Hello!"
            })),
            ..HttpRequest::default()
        })?;

        let LlmRequestPayload::Responses(payload) = req.payload else {
            panic!("expected responses payload");
        };
        assert_eq!(
            payload.instructions.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert_eq!(
            payload.previous_response_id.as_deref(),
            Some("resp_prev_123")
        );
        Ok(())
    }

    #[test]
    fn responses_inbound_preserves_reasoning_and_tools() -> TransformerResult<()> {
        // Mirrors Go "request with reasoning" + "request with function tools"
        // + "request with image generation tool" cases. Tools round-trip via
        // the typed `UnifiedTool` vec; reasoning is preserved as a raw Value.
        let inbound = OpenAiResponsesInbound::new();
        let req = inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            json_body: Some(json!({
                "model": "o3",
                "input": "Solve this problem",
                "reasoning": {"effort": "high", "max_tokens": 5000},
                "tools": [
                    {"type": "function", "name": "get_weather", "parameters": {"type": "object"}},
                    {"type": "image_generation", "quality": "high", "size": "1024x1024"}
                ]
            })),
            ..HttpRequest::default()
        })?;

        let LlmRequestPayload::Responses(payload) = req.payload else {
            panic!("expected responses payload");
        };
        // Reasoning preserved as raw JSON.
        assert_eq!(
            payload.reasoning,
            Some(json!({"effort": "high", "max_tokens": 5000}))
        );
        // Two tools, second is image_generation.
        assert_eq!(payload.tools.len(), 2);
        assert_eq!(payload.tools[1].tool_type, "image_generation");
        assert_eq!(payload.tools[1].extra.get("quality"), Some(&json!("high")));
        assert_eq!(
            payload.tools[1].extra.get("size"),
            Some(&json!("1024x1024"))
        );
        Ok(())
    }

    #[test]
    fn responses_inbound_compact_path_forces_compact_request_type() -> TransformerResult<()> {
        // Mirrors the compact dispatch: the `OpenAiResponsesCompact`
        // api_format (set by the path dispatcher) forces compact.
        let inbound = OpenAiResponsesInbound::new();
        let req = inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            path: RESPONSES_COMPACT_PATH.to_string(),
            api_format: Some(ApiFormat::OpenAiResponsesCompact),
            json_body: Some(json!({
                "model": "gpt-4.1",
                "input": [{"role": "user", "content": "hi"}]
            })),
            ..HttpRequest::default()
        })?;

        assert_eq!(req.request_type, RequestType::Compact);
        assert_eq!(req.api_format, ApiFormat::OpenAiResponsesCompact);

        let LlmRequestPayload::Responses(payload) = req.payload else {
            panic!("expected responses payload");
        };
        assert!(payload.compact);
        Ok(())
    }

    #[test]
    fn responses_inbound_carries_http_headers_and_metadata() -> TransformerResult<()> {
        // Mirrors the HTTP-layer context propagation pattern from
        // `OpenAiChatInbound::inbound_request`.
        let inbound = OpenAiResponsesInbound::new();
        let mut headers = conduit_llm::model::HeaderMap::new();
        headers.insert("X-Custom".to_string(), "v".to_string());
        let mut metadata = conduit_llm::model::ExtensionMap::new();
        metadata.insert("k".to_string(), json!("v"));

        let req = inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            json_body: Some(json!({"model": "gpt-4o", "input": "hi"})),
            headers,
            metadata,
            request_id: Some("req-1".to_string()),
            ..HttpRequest::default()
        })?;

        assert_eq!(req.extra_headers.get("X-Custom"), Some(&"v".to_string()));
        assert_eq!(req.metadata.get("k"), Some(&json!("v")));
        assert_eq!(req.metadata.get("request_id"), Some(&json!("req-1")));
        Ok(())
    }

    #[test]
    fn responses_legacy_hooks_match_wire_passthrough_and_error_contract() -> TransformerResult<()> {
        // Production cross-protocol behavior is exercised through
        // transform_response/transform_stream. The older raw hooks remain
        // wire-compatible for callers that already hold Responses frames.
        let inbound = OpenAiResponsesInbound::new();
        let response = HttpResponse {
            status: 202,
            json_body: Some(json!({"object": "response"})),
            ..HttpResponse::default()
        };
        let response = inbound.inbound_response(response)?;
        assert_eq!(response.status, 202);
        assert_eq!(response.json_body, Some(json!({"object": "response"})));

        let event = StreamEvent {
            event_type: Some("response.completed".to_string()),
            data: Some("{}".to_string()),
            ..StreamEvent::default()
        };
        let event = inbound.inbound_stream_event(event)?;
        assert_eq!(event.event_type.as_deref(), Some("response.completed"));

        let error = inbound.inbound_error(&ConduitError::invalid_request("bad input"))?;
        assert_eq!(error.status, 400);
        assert!(
            error
                .json_body
                .as_ref()
                .is_some_and(|body| body["error"]["message"] == "bad input")
        );
        Ok(())
    }

    #[test]
    fn responses_stream_aggregate_extracts_completed_usage() -> TransformerResult<()> {
        let inbound = OpenAiResponsesInbound::new();
        let response = inbound.aggregate_stream_chunks(vec![StreamEvent {
            event_type: Some("response.completed".to_string()),
            data: Some(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": "resp_1",
                        "object": "response",
                        "status": "completed",
                        "usage": {
                            "input_tokens": 120,
                            "output_tokens": 40,
                            "total_tokens": 160
                        }
                    }
                })
                .to_string(),
            ),
            ..StreamEvent::default()
        }])?;
        let usage = response.usage.ok_or_else(|| {
            ConduitError::internal("completed Responses event did not produce usage")
        })?;
        assert_eq!(usage.prompt_tokens, 120);
        assert_eq!(usage.completion_tokens, 40);
        assert_eq!(usage.total_tokens, 160);
        Ok(())
    }

    #[test]
    fn http_request_normalization_preserves_headers_and_request_metadata() -> TransformerResult<()>
    {
        let request = normalize_openai_request(HttpRequest {
            method: "POST".to_string(),
            path: CHAT_COMPLETIONS_PATH.to_string(),
            headers: [("x-client".to_string(), "sdk".to_string())].into(),
            json_body: Some(json!({
                "model": "gpt-4o-mini",
                "messages": [{"role": "user", "content": "hi"}]
            })),
            request_id: Some("req_123".to_string()),
            client_ip: Some("203.0.113.10".to_string()),
            metadata: [("tenant".to_string(), json!("project-a"))].into(),
            ..HttpRequest::default()
        })?;

        assert_eq!(
            request.extra_headers.get("x-client"),
            Some(&"sdk".to_string())
        );
        assert_eq!(request.metadata.get("tenant"), Some(&json!("project-a")));
        assert_eq!(request.metadata.get("request_id"), Some(&json!("req_123")));
        assert_eq!(
            request.metadata.get("client_ip"),
            Some(&json!("203.0.113.10"))
        );
        Ok(())
    }

    #[test]
    fn invalid_json_body_maps_to_invalid_request() {
        let err = normalize_openai_request(HttpRequest {
            method: "POST".to_string(),
            path: CHAT_COMPLETIONS_PATH.to_string(),
            body: Some(b"{not-json".to_vec()),
            ..HttpRequest::default()
        })
        .err();

        assert_eq!(
            err.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );
    }

    // -------------------------------------------------------------------------
    // `OpenAiChatInbound` (RUST-P7-001) — parity tests with Go
    // `inbound_test.go::TestInboundTransformer_TransformRequest`.
    // -------------------------------------------------------------------------

    // Build an OpenAI chat-completions HTTP request with the given JSON body
    // and content type. The body mirrors the inline fixtures Go marshals via
    // `mustMarshal(llm.Request{...})` in `inbound_test.go`.
    fn chat_http_request(content_type: &str, body: Value) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            path: CHAT_COMPLETIONS_PATH.to_string(),
            content_type: Some(content_type.to_string()),
            json_body: Some(body),
            ..HttpRequest::default()
        }
    }

    fn chat_request_body(model: &str, messages: Vec<Value>, extra: Value) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), json!(model));
        body.insert("messages".to_string(), Value::Array(messages));
        if let Value::Object(extras) = extra {
            for (key, value) in extras {
                body.insert(key, value);
            }
        }
        Value::Object(body)
    }

    #[test]
    fn openai_chat_inbound_transforms_valid_request_with_text_content() -> TransformerResult<()> {
        // Mirrors Go case "valid request".
        let transformer = OpenAiChatInbound::new();
        let request = transformer.inbound_request(chat_http_request(
            "application/json",
            json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "Hello, world!"}]
            }),
        ))?;

        assert_eq!(transformer.name(), "openai/chat_completions");
        assert_eq!(request.request_type, RequestType::Chat);
        assert_eq!(request.api_format, ApiFormat::OpenAiChatCompletions);
        assert_eq!(request.model.as_deref(), Some("gpt-4"));
        assert!(!request.stream);

        let LlmRequestPayload::Chat(payload) = request.payload else {
            panic!("expected chat payload");
        };
        assert_eq!(payload.messages.len(), 1);
        assert_eq!(payload.messages[0].role, "user");
        assert_eq!(
            payload.messages[0].content,
            Some(MessageContent::Text("Hello, world!".to_string()))
        );
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_sets_stream_flag_when_requested() -> TransformerResult<()> {
        let transformer = OpenAiChatInbound::new();
        let request = transformer.inbound_request(chat_http_request(
            "application/json",
            json!({
                "model": "gpt-4",
                "stream": true,
                "messages": [{"role": "user", "content": "stream me"}]
            }),
        ))?;

        assert!(request.stream, "stream flag must reflect request body");
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_supports_developer_and_system_roles() -> TransformerResult<()> {
        // Go preserves role strings verbatim (system/developer/user/assistant/tool).
        let transformer = OpenAiChatInbound::new();
        let request = transformer.inbound_request(chat_http_request(
            "application/json",
            json!({
                "model": "gpt-4o",
                "messages": [
                    {"role": "developer", "content": "be terse"},
                    {"role": "system", "content": "always answer in JSON"},
                    {"role": "user", "content": "ping"}
                ]
            }),
        ))?;

        let LlmRequestPayload::Chat(payload) = request.payload else {
            panic!("expected chat payload");
        };
        let roles: Vec<&str> = payload.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["developer", "system", "user"]);
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_preserves_multimodal_content_parts() -> TransformerResult<()> {
        // Mirrors Go `MessageContentPart` round-trip (image_url / input_audio).
        // video_url has no dedicated Rust slot and is preserved via part.extra.
        let transformer = OpenAiChatInbound::new();
        let request = transformer.inbound_request(chat_http_request(
            "application/json",
            json!({
                "model": "gpt-4o",
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "describe these"},
                        {"type": "image_url", "image_url": {"url": "https://example.com/cat.png", "detail": "high"}},
                        {"type": "input_audio", "input_audio": {"format": "wav", "data": "UklGRiQ="}},
                        {"type": "video_url", "video_url": {"url": "https://example.com/clip.mp4"}}
                    ]
                }]
            }),
        ))?;

        let LlmRequestPayload::Chat(payload) = request.payload else {
            panic!("expected chat payload");
        };
        let Some(content) = payload.messages[0].content.as_ref() else {
            panic!("content present");
        };
        let MessageContent::Parts(parts) = content else {
            panic!("expected multipart content");
        };
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0].part_type, "text");
        assert_eq!(parts[0].text.as_deref(), Some("describe these"));
        assert_eq!(parts[1].part_type, "image_url");
        assert_eq!(
            parts[1].image_url.as_ref().and_then(|v| v.get("url")),
            Some(&json!("https://example.com/cat.png"))
        );
        assert_eq!(parts[2].part_type, "input_audio");
        assert_eq!(
            parts[2].input_audio.as_ref().and_then(|v| v.get("format")),
            Some(&json!("wav"))
        );
        assert_eq!(parts[3].part_type, "video_url");
        // video_url is preserved via the flatten extra bag.
        assert_eq!(
            parts[3].extra.get("video_url"),
            Some(&json!({"url": "https://example.com/clip.mp4"}))
        );
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_preserves_tools_tool_choice_and_response_format() -> TransformerResult<()>
    {
        let transformer = OpenAiChatInbound::new();
        let request = transformer.inbound_request(chat_http_request(
            "application/json",
            json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "lookup weather"}],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "look up the weather",
                        "parameters": {"type": "object"},
                        "strict": true
                    }
                }],
                "tool_choice": {"type": "function", "function": {"name": "get_weather"}},
                "response_format": {"type": "json_object"},
                "parallel_tool_calls": true
            }),
        ))?;

        let LlmRequestPayload::Chat(payload) = request.payload else {
            panic!("expected chat payload");
        };
        assert_eq!(payload.tools.len(), 1);
        assert_eq!(payload.tools[0].tool_type, "function");
        // The OpenAI chat-completions `function` sub-object is preserved
        // losslessly under `UnifiedTool.extra` (the Rust unified tool model
        // mirrors the Responses-style flat shape; nested chat-completions
        // function definitions round-trip via the extension bag).
        assert_eq!(
            payload.tools[0].extra.get("name"),
            Some(&json!("get_weather"))
        );
        assert_eq!(
            payload.tools[0].extra.get("parameters"),
            Some(&json!({"type": "object"}))
        );
        assert_eq!(payload.tools[0].extra.get("strict"), Some(&json!(true)));
        // tool_choice is kept as an opaque JSON value (string or named object).
        assert_eq!(
            payload.tool_choice,
            Some(json!({"type": "function", "function": {"name": "get_weather"}}))
        );
        assert_eq!(
            payload.response_format,
            Some(json!({"type": "json_object"}))
        );
        // parallel_tool_calls has no first-class slot → preserved via extra.
        assert_eq!(payload.extra.get("parallel_tool_calls"), Some(&json!(true)));
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_preserves_reasoning_fields() -> TransformerResult<()> {
        // Mirrors Go cases "request with reasoning budget / effort / summary".
        let transformer = OpenAiChatInbound::new();
        let request = transformer.inbound_request(chat_http_request(
            "application/json",
            json!({
                "model": "o3",
                "messages": [{"role": "user", "content": "think hard"}],
                "reasoning_effort": "medium",
                "reasoning_summary": "detailed"
            }),
        ))?;

        let LlmRequestPayload::Chat(payload) = request.payload else {
            panic!("expected chat payload");
        };
        assert_eq!(payload.reasoning_effort.as_deref(), Some("medium"));
        // reasoning_summary has no first-class slot → extra.
        assert_eq!(
            payload.extra.get("reasoning_summary"),
            Some(&json!("detailed"))
        );
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_preserves_stream_options() -> TransformerResult<()> {
        let transformer = OpenAiChatInbound::new();
        let request = transformer.inbound_request(chat_http_request(
            "application/json",
            json!({
                "model": "gpt-4o",
                "stream": true,
                "stream_options": {"include_usage": true},
                "messages": [{"role": "user", "content": "stream"}]
            }),
        ))?;

        assert!(request.stream);
        let LlmRequestPayload::Chat(payload) = request.payload else {
            panic!("expected chat payload");
        };
        assert_eq!(payload.stream_options, Some(json!({"include_usage": true})));
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_preserves_extra_provider_fields() -> TransformerResult<()> {
        // Unrecognized top-level fields (modalities, logprobs, service_tier,
        // prompt_cache_key, safety_identifier, ...) must round-trip via extra,
        // matching Go's named-field capture but losslessly for unknown extras.
        let transformer = OpenAiChatInbound::new();
        let request = transformer.inbound_request(chat_http_request(
            "application/json",
            chat_request_body(
                "gpt-4o",
                vec![json!({"role": "user", "content": "hi"})],
                json!({
                    "modalities": ["text", "audio"],
                    "logprobs": true,
                    "top_logprobs": 5,
                    "service_tier": "priority",
                    "prompt_cache_key": "cache-1",
                    "safety_identifier": "user-42",
                    "store": true,
                    "seed": 7,
                    "frequency_penalty": 0.5
                }),
            ),
        ))?;

        let LlmRequestPayload::Chat(payload) = request.payload else {
            panic!("expected chat payload");
        };
        let extra = &payload.extra;
        assert_eq!(extra.get("modalities"), Some(&json!(["text", "audio"])));
        assert_eq!(extra.get("logprobs"), Some(&json!(true)));
        assert_eq!(extra.get("top_logprobs"), Some(&json!(5)));
        assert_eq!(extra.get("service_tier"), Some(&json!("priority")));
        assert_eq!(extra.get("prompt_cache_key"), Some(&json!("cache-1")));
        assert_eq!(extra.get("safety_identifier"), Some(&json!("user-42")));
        assert_eq!(extra.get("store"), Some(&json!(true)));
        assert_eq!(extra.get("seed"), Some(&json!(7)));
        assert_eq!(extra.get("frequency_penalty"), Some(&json!(0.5)));
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_preserves_assistant_tool_calls_and_tool_role_messages()
    -> TransformerResult<()> {
        let transformer = OpenAiChatInbound::new();
        let request = transformer.inbound_request(chat_http_request(
            "application/json",
            json!({
                "model": "gpt-4o",
                "messages": [
                    {"role": "user", "content": "what is the weather?"},
                    {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "get_weather", "arguments": "{\"city\":\"Shanghai\"}"}
                        }]
                    },
                    {"role": "tool", "tool_call_id": "call_1", "content": "sunny"}
                ]
            }),
        ))?;

        let LlmRequestPayload::Chat(payload) = request.payload else {
            panic!("expected chat payload");
        };
        assert_eq!(payload.messages.len(), 3);
        // assistant message carries the tool call
        let assistant = &payload.messages[1];
        assert_eq!(assistant.role, "assistant");
        assert_eq!(assistant.tool_calls.len(), 1);
        assert_eq!(assistant.tool_calls[0].call_type, "function");
        assert_eq!(
            assistant.tool_calls[0].function.get("name"),
            Some(&json!("get_weather"))
        );
        // tool response message
        let tool_msg = &payload.messages[2];
        assert_eq!(tool_msg.role, "tool");
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call_1"));
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_rejects_empty_body() {
        let transformer = OpenAiChatInbound::new();
        let request = HttpRequest {
            method: "POST".to_string(),
            path: CHAT_COMPLETIONS_PATH.to_string(),
            content_type: Some("application/json".to_string()),
            body: None,
            json_body: None,
            ..HttpRequest::default()
        };
        let err = transformer.inbound_request(request).err();

        assert_eq!(
            err.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );
        assert!(
            err.map(|err| err.public_message().to_lowercase())
                .map_or(false, |message| message.contains("body"))
        );
    }

    #[test]
    fn openai_chat_inbound_rejects_unsupported_content_type() {
        let transformer = OpenAiChatInbound::new();
        let request = chat_http_request(
            "text/plain",
            json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}]}),
        );
        let err = match transformer.inbound_request(request) {
            Err(err) => err,
            Ok(_) => panic!("text/plain must be rejected"),
        };

        assert_eq!(err.error_type(), "invalid_request");
        assert!(err.public_message().contains("unsupported content type"));
    }

    #[test]
    fn openai_chat_inbound_accepts_json_content_type_with_charset() -> TransformerResult<()> {
        let transformer = OpenAiChatInbound::new();
        let request = chat_http_request(
            "application/json; charset=utf-8",
            json!({"model": "gpt-4", "messages": [{"role": "user", "content": "hi"}]}),
        );
        // Should not error.
        transformer.inbound_request(request)?;
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_reads_content_type_from_header_when_field_absent()
    -> TransformerResult<()> {
        let transformer = OpenAiChatInbound::new();
        let request = HttpRequest {
            method: "POST".to_string(),
            path: CHAT_COMPLETIONS_PATH.to_string(),
            headers: [("Content-Type".to_string(), "application/json".to_string())].into(),
            json_body: Some(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "hi"}]
            })),
            ..HttpRequest::default()
        };
        transformer.inbound_request(request)?;
        Ok(())
    }

    #[test]
    fn openai_chat_inbound_rejects_invalid_json_body() {
        let transformer = OpenAiChatInbound::new();
        let request = HttpRequest {
            method: "POST".to_string(),
            path: CHAT_COMPLETIONS_PATH.to_string(),
            content_type: Some("application/json".to_string()),
            body: Some(b"{invalid json}".to_vec()),
            ..HttpRequest::default()
        };
        let err = match transformer.inbound_request(request) {
            Err(err) => err,
            Ok(_) => panic!("invalid JSON must be rejected"),
        };

        assert_eq!(err.error_type(), "invalid_request");
    }

    #[test]
    fn openai_chat_inbound_rejects_missing_model() {
        let transformer = OpenAiChatInbound::new();
        let request = chat_http_request(
            "application/json",
            json!({"messages": [{"role": "user", "content": "hi"}]}),
        );
        let err = match transformer.inbound_request(request) {
            Err(err) => err,
            Ok(_) => panic!("missing model must be rejected"),
        };

        assert_eq!(err.error_type(), "invalid_request");
        assert!(err.public_message().contains("model"));
    }

    #[test]
    fn openai_chat_inbound_rejects_missing_messages() {
        let transformer = OpenAiChatInbound::new();
        let request = chat_http_request("application/json", json!({"model": "gpt-4"}));
        let err = match transformer.inbound_request(request) {
            Err(err) => err,
            Ok(_) => panic!("missing messages must be rejected"),
        };

        assert_eq!(err.error_type(), "invalid_request");
        assert!(err.public_message().contains("messages"));
    }

    #[test]
    fn openai_chat_inbound_rejects_empty_messages_array() {
        let transformer = OpenAiChatInbound::new();
        let request = chat_http_request(
            "application/json",
            json!({"model": "gpt-4", "messages": []}),
        );
        let err = match transformer.inbound_request(request) {
            Err(err) => err,
            Ok(_) => panic!("empty messages must be rejected"),
        };

        assert_eq!(err.error_type(), "invalid_request");
        assert!(err.public_message().contains("messages"));
    }

    #[test]
    fn openai_chat_inbound_propagates_request_id_and_client_ip_to_metadata() -> TransformerResult<()>
    {
        let transformer = OpenAiChatInbound::new();
        let request = HttpRequest {
            method: "POST".to_string(),
            path: CHAT_COMPLETIONS_PATH.to_string(),
            content_type: Some("application/json".to_string()),
            headers: [("x-client".to_string(), "sdk".to_string())].into(),
            request_id: Some("req_abc".to_string()),
            client_ip: Some("198.51.100.20".to_string()),
            metadata: [("tenant".to_string(), json!("t1"))].into(),
            json_body: Some(json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "hi"}]
            })),
            ..HttpRequest::default()
        };
        let result = transformer.inbound_request(request)?;

        assert_eq!(
            result.extra_headers.get("x-client"),
            Some(&"sdk".to_string())
        );
        assert_eq!(result.metadata.get("tenant"), Some(&json!("t1")));
        assert_eq!(result.metadata.get("request_id"), Some(&json!("req_abc")));
        assert_eq!(
            result.metadata.get("client_ip"),
            Some(&json!("198.51.100.20"))
        );
        Ok(())
    }

    #[test]
    fn openai_chat_legacy_raw_hooks_remain_explicitly_unsupported() {
        let transformer = OpenAiChatInbound::new();
        let err_response = transformer.inbound_response(HttpResponse::default()).err();
        assert_eq!(
            err_response.as_ref().map(|err| err.error_type()),
            Some("internal_error")
        );

        let err_stream = transformer
            .inbound_stream_event(StreamEvent::default())
            .err();
        assert_eq!(
            err_stream.as_ref().map(|err| err.error_type()),
            Some("internal_error")
        );

        let err_error = transformer
            .inbound_error(&ConduitError::invalid_request("boom"))
            .err();
        assert_eq!(
            err_error.as_ref().map(|err| err.error_type()),
            Some("internal_error")
        );
    }

    #[test]
    fn normalize_chat_completions_body_rejects_missing_model_and_messages() {
        let missing_model = normalize_chat_completions_body(json!({
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .err();
        assert_eq!(
            missing_model.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );

        let missing_messages = normalize_chat_completions_body(json!({"model": "gpt-4"})).err();
        assert_eq!(
            missing_messages.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );

        let empty_messages = normalize_chat_completions_body(json!({
            "model": "gpt-4",
            "messages": []
        }))
        .err();
        assert_eq!(
            empty_messages.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );
    }

    // -------------------------------------------------------------------------
    // `normalize_embeddings_body` (RUST-P7-001 S08) — parity tests with Go
    // `embedding_test.go::TestEmbeddingInboundTransformer_TransformRequest`.
    // -------------------------------------------------------------------------

    // Mirrors Go case "valid string input".
    #[test]
    fn embeddings_body_normalizes_string_input_and_preserves_options() -> TransformerResult<()> {
        let request = normalize_embeddings_body(json!({
            "model": "text-embedding-ada-002",
            "input": "The quick brown fox",
            "encoding_format": "base64",
            "dimensions": 256,
            "user": "user-42"
        }))?;

        assert_eq!(request.request_type, RequestType::Embedding);
        assert_eq!(request.api_format, ApiFormat::OpenAiEmbeddings);
        assert_eq!(request.model.as_deref(), Some("text-embedding-ada-002"));
        // Embeddings never stream (Go leaves Stream = nil).
        assert!(!request.stream);

        let LlmRequestPayload::Embedding(payload) = request.payload else {
            panic!("expected embedding payload");
        };
        assert_eq!(payload.input, Some(json!("The quick brown fox")));
        assert_eq!(payload.encoding_format.as_deref(), Some("base64"));
        assert_eq!(payload.dimensions, Some(256));
        assert_eq!(payload.user.as_deref(), Some("user-42"));
        Ok(())
    }

    // Mirrors Go case "valid array input": input as `[]string` round-trips.
    #[test]
    fn embeddings_body_normalizes_string_array_input() -> TransformerResult<()> {
        let request = normalize_embeddings_body(json!({
            "model": "text-embedding-ada-002",
            "input": ["Hello", "World"]
        }))?;

        let LlmRequestPayload::Embedding(payload) = request.payload else {
            panic!("expected embedding payload");
        };
        assert_eq!(payload.input, Some(json!(["Hello", "World"])));
        Ok(())
    }

    // Mirrors Go case "valid token ids input" + "valid nested token ids
    // input": integer arrays (flat and nested) are preserved verbatim as
    // opaque JSON values.
    #[test]
    fn embeddings_body_preserves_integer_and_nested_integer_array_input() -> TransformerResult<()> {
        let flat = normalize_embeddings_body(json!({
            "model": "text-embedding-ada-002",
            "input": [1234, 5678, 9012]
        }))?;
        let LlmRequestPayload::Embedding(flat_payload) = flat.payload else {
            panic!("expected embedding payload");
        };
        assert_eq!(flat_payload.input, Some(json!([1234, 5678, 9012])));

        let nested = normalize_embeddings_body(json!({
            "model": "text-embedding-ada-002",
            "input": [[1234, 5678], [9012, 3456]]
        }))?;
        let LlmRequestPayload::Embedding(nested_payload) = nested.payload else {
            panic!("expected embedding payload");
        };
        assert_eq!(
            nested_payload.input,
            Some(json!([[1234, 5678], [9012, 3456]]))
        );
        Ok(())
    }

    // Mirrors Go case "missing model".
    #[test]
    fn embeddings_body_rejects_missing_model() {
        match normalize_embeddings_body(json!({ "input": "test" })) {
            Err(err) => assert!(err.to_string().contains("model is required")),
            Ok(_) => panic!("expected model-required error"),
        }
    }

    // Mirrors Go cases "missing input" + "empty string input" +
    // "whitespace only input": all fall through to the default
    // `validateEmbeddingInput` branch ("input cannot be empty string").
    #[test]
    fn embeddings_body_rejects_empty_or_missing_string_input() {
        for body in [
            json!({"model": "text-embedding-ada-002"}),
            json!({"model": "text-embedding-ada-002", "input": ""}),
            json!({"model": "text-embedding-ada-002", "input": "   "}),
        ] {
            match normalize_embeddings_body(body) {
                Err(err) => assert!(
                    err.to_string().contains("input cannot be empty string"),
                    "expected empty-string error"
                ),
                Ok(_) => panic!("expected empty-string error"),
            }
        }
    }

    // Mirrors Go case "empty array input".
    #[test]
    fn embeddings_body_rejects_empty_array_input() {
        match normalize_embeddings_body(json!({
            "model": "text-embedding-ada-002",
            "input": []
        })) {
            Err(err) => assert!(err.to_string().contains("input cannot be empty array")),
            Ok(_) => panic!("expected empty-array error"),
        }
    }

    // Mirrors Go case "empty nested array input": the inner empty array at
    // index 0 surfaces the per-element message.
    #[test]
    fn embeddings_body_rejects_empty_nested_inner_array() {
        match normalize_embeddings_body(json!({
            "model": "text-embedding-ada-002",
            "input": [[], [1234]]
        })) {
            Err(err) => assert!(err.to_string().contains("input[0] cannot be empty array")),
            Ok(_) => panic!("expected per-element empty-array error"),
        }
    }

    // Mirrors Go's `EmbeddingInput.UnmarshalJSON` rejection of non-string /
    // non-array shapes (number/object/bool): surfaces a parity-style
    // "invalid embedding input type" error so the inbound 400 stays stable.
    #[test]
    fn embeddings_body_rejects_non_string_non_array_input() {
        let err = normalize_embeddings_body(json!({
            "model": "text-embedding-ada-002",
            "input": 42
        }))
        .err();
        assert_eq!(
            err.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );
        assert!(err.map(|err| err.to_string()).map_or(false, |message| {
            message.contains("invalid embedding input type")
        }));
    }

    // Mirrors the dispatcher path: `/v1/embeddings` resolves to
    // `ApiFormat::OpenAiEmbeddings` and routes through the embedding
    // normalizer (not the `_ => Err` fallback).
    #[test]
    fn openai_request_routes_embeddings_path_to_embedding_payload() -> TransformerResult<()> {
        let request = normalize_openai_request(HttpRequest {
            method: "POST".to_string(),
            path: EMBEDDINGS_PATH.to_string(),
            json_body: Some(json!({
                "model": "text-embedding-ada-002",
                "input": "hello"
            })),
            ..HttpRequest::default()
        })?;

        assert_eq!(request.api_format, ApiFormat::OpenAiEmbeddings);
        assert_eq!(request.request_type, RequestType::Embedding);
        assert!(matches!(request.payload, LlmRequestPayload::Embedding(_)));
        Ok(())
    }

    // -------------------------------------------------------------------------
    // `normalize_audio_body` (RUST-P7-001 S11) — parity tests with Go
    // `audio_inbound_test.go::TestAudioInboundTransformer_{Speech,
    // Transcription, Translation}` (TransformRequest cases only).
    // -------------------------------------------------------------------------

    // Mirrors Go case "valid request" (speech): all fields round-trip onto
    // the unified AudioRequest, stream is false when stream_format is absent.
    #[test]
    fn audio_speech_body_normalizes_tts_request_and_preserves_options() -> TransformerResult<()> {
        let request = normalize_audio_body(
            json!({
                "model": "tts-1",
                "input": "Hello world",
                "voice": "alloy",
                "response_format": "mp3",
                "speed": 1.25,
                "instructions": "read warmly"
            }),
            RequestType::Speech,
        )?;

        assert_eq!(request.request_type, RequestType::Speech);
        assert_eq!(request.api_format, ApiFormat::OpenAiAudioSpeech);
        assert_eq!(request.model.as_deref(), Some("tts-1"));
        // No stream_format → non-streaming, mirroring Go's `isStream =
        // streamFormat != ""`.
        assert!(!request.stream);

        let LlmRequestPayload::Audio(payload) = request.payload else {
            panic!("expected audio payload");
        };
        assert_eq!(payload.input, Some(json!("Hello world")));
        assert_eq!(payload.voice.as_deref(), Some("alloy"));
        assert_eq!(payload.response_format.as_deref(), Some("mp3"));
        // `speed` / `instructions` have no first-class slot on the Rust
        // unified AudioRequest → preserved via `extra`.
        assert_eq!(payload.extra.get("speed"), Some(&json!(1.25)));
        assert_eq!(
            payload.extra.get("instructions"),
            Some(&json!("read warmly"))
        );
        Ok(())
    }

    // Mirrors Go case "missing input": speech requires `input`.
    #[test]
    fn audio_speech_body_rejects_missing_input() {
        match normalize_audio_body(
            json!({"model": "tts-1", "voice": "alloy"}),
            RequestType::Speech,
        ) {
            Err(err) => assert!(err.to_string().contains("input is required")),
            Ok(_) => panic!("expected input-required error"),
        }
    }

    // Mirrors Go case "missing voice": error message contains "voice".
    #[test]
    fn audio_speech_body_rejects_missing_voice() {
        match normalize_audio_body(
            json!({"model": "tts-1", "input": "hi"}),
            RequestType::Speech,
        ) {
            Err(err) => assert!(err.to_string().contains("voice")),
            Ok(_) => panic!("expected voice-required error"),
        }
    }

    // Mirrors Go case "missing model": all audio kinds reject absent model.
    #[test]
    fn audio_body_rejects_missing_model() {
        match normalize_audio_body(
            json!({"input": "hi", "voice": "alloy"}),
            RequestType::Speech,
        ) {
            Err(err) => assert!(err.to_string().contains("model is required")),
            Ok(_) => panic!("expected model-required error"),
        }
    }

    // Mirrors Go case "unsupported stream_format rejected": only "sse" /
    // "audio" are allowed; any other value surfaces a parity-style error.
    #[test]
    fn audio_speech_body_rejects_unsupported_stream_format() {
        match normalize_audio_body(
            json!({
                "model": "gpt-4o-mini-tts",
                "input": "hi",
                "voice": "alloy",
                "stream_format": "json"
            }),
            RequestType::Speech,
        ) {
            Err(err) => assert!(err.to_string().contains("stream_format")),
            Ok(_) => panic!("expected stream_format error"),
        }
    }

    // Mirrors Go cases "sse stream_format enables streaming" and "audio
    // stream_format enables binary streaming": each canonical value sets
    // `stream=true` and is preserved (lowercased) via `extra`.
    #[test]
    fn audio_speech_body_stream_format_enables_streaming() -> TransformerResult<()> {
        for raw in ["sse", "AUDIO", "  audio  "] {
            let request = normalize_audio_body(
                json!({
                    "model": "gpt-4o-mini-tts",
                    "input": "hi",
                    "voice": "alloy",
                    "stream_format": raw
                }),
                RequestType::Speech,
            )?;
            assert!(
                request.stream,
                "stream must be true for stream_format={raw}"
            );
            let LlmRequestPayload::Audio(payload) = request.payload else {
                panic!("expected audio payload");
            };
            let canonical = raw.trim().to_ascii_lowercase();
            assert_eq!(
                payload.extra.get("stream_format"),
                Some(&json!(canonical)),
                "stream_format must be canonicalized for raw={raw}"
            );
        }
        Ok(())
    }

    // Mirrors Go case "valid multipart request" (transcription): the gateway
    // supplies the JSON view of the multipart form (file metadata + scalar
    // fields). `model` + `file` round-trip; language/response_format/
    // temperature land on first-class slots.
    #[test]
    fn audio_transcription_body_normalizes_multipart_json_view() -> TransformerResult<()> {
        let request = normalize_audio_body(
            json!({
                "model": "whisper-1",
                "file": "<audio bytes: 16, filename: speech.mp3>",
                "language": "en",
                "response_format": "json",
                "temperature": 0.2,
                "timestamp_granularities[]": ["word", "segment"]
            }),
            RequestType::Transcription,
        )?;

        assert_eq!(request.request_type, RequestType::Transcription);
        assert_eq!(request.api_format, ApiFormat::OpenAiAudioTranscriptions);
        assert_eq!(request.model.as_deref(), Some("whisper-1"));
        // STT endpoints never engage the streaming pipeline via this field.
        assert!(!request.stream);

        let LlmRequestPayload::Audio(payload) = request.payload else {
            panic!("expected audio payload");
        };
        assert_eq!(
            payload.file,
            Some(json!("<audio bytes: 16, filename: speech.mp3>"))
        );
        assert_eq!(payload.language.as_deref(), Some("en"));
        assert_eq!(payload.response_format.as_deref(), Some("json"));
        assert_eq!(payload.temperature, Some(0.2));
        // Unmodeled multipart fields ride via `extra` (Go: `Extra` map).
        assert_eq!(
            payload.extra.get("timestamp_granularities[]"),
            Some(&json!(["word", "segment"]))
        );
        Ok(())
    }

    // Mirrors Go case "missing file": transcription/translation require a
    // `file` part.
    #[test]
    fn audio_stt_body_rejects_missing_file() {
        let err =
            normalize_audio_body(json!({"model": "whisper-1"}), RequestType::Transcription).err();
        assert_eq!(
            err.as_ref().map(|err| err.error_type()),
            Some("invalid_request")
        );
        assert!(err.map(|err| err.to_string()).map_or(false, |message| {
            message.contains("file is required for transcription")
        }));

        let err =
            normalize_audio_body(json!({"model": "whisper-1"}), RequestType::Translation).err();
        assert!(err.map(|err| err.to_string()).map_or(false, |message| {
            message.contains("file is required for translation")
        }));
    }

    // Mirrors Go case "valid multipart request" (translation): translation
    // does not carry `language` (Go omits it) but otherwise mirrors
    // transcription.
    #[test]
    fn audio_translation_body_normalizes_multipart_json_view() -> TransformerResult<()> {
        let request = normalize_audio_body(
            json!({
                "model": "whisper-1",
                "file": "<audio bytes: 9, filename: de.mp3>",
                "prompt": "translate this"
            }),
            RequestType::Translation,
        )?;

        assert_eq!(request.request_type, RequestType::Translation);
        assert_eq!(request.api_format, ApiFormat::OpenAiAudioTranslations);
        assert_eq!(request.model.as_deref(), Some("whisper-1"));

        let LlmRequestPayload::Audio(payload) = request.payload else {
            panic!("expected audio payload");
        };
        assert_eq!(
            payload.file,
            Some(json!("<audio bytes: 9, filename: de.mp3>"))
        );
        assert_eq!(payload.extra.get("prompt"), Some(&json!("translate this")));
        // Translation has no language slot.
        assert!(payload.language.is_none());
        Ok(())
    }

    // Dispatcher parity: each of the three `/v1/audio/*` paths routes through
    // `normalize_openai_body` to the audio normalizer with the correct
    // `ApiFormat` + `RequestType` (instead of the `_ => Err` fallback).
    #[test]
    fn openai_request_routes_each_audio_path_to_audio_payload() -> TransformerResult<()> {
        for (path, api_format, request_type, body) in [
            (
                AUDIO_SPEECH_PATH,
                ApiFormat::OpenAiAudioSpeech,
                RequestType::Speech,
                json!({"model": "tts-1", "input": "hi", "voice": "alloy"}),
            ),
            (
                AUDIO_TRANSCRIPTIONS_PATH,
                ApiFormat::OpenAiAudioTranscriptions,
                RequestType::Transcription,
                json!({"model": "whisper-1", "file": "<audio bytes: 4>"}),
            ),
            (
                AUDIO_TRANSLATIONS_PATH,
                ApiFormat::OpenAiAudioTranslations,
                RequestType::Translation,
                json!({"model": "whisper-1", "file": "<audio bytes: 4>"}),
            ),
        ] {
            let request = normalize_openai_request(HttpRequest {
                method: "POST".to_string(),
                path: path.to_string(),
                json_body: Some(body),
                ..HttpRequest::default()
            })?;

            assert_eq!(request.api_format, api_format, "api_format for {path}");
            assert_eq!(
                request.request_type, request_type,
                "request_type for {path}"
            );
            assert!(
                matches!(request.payload, LlmRequestPayload::Audio(_)),
                "expected audio payload for {path}"
            );
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // `normalize_image_body_with_format` (RUST-P7-001 S09) — parity tests
    // with Go `image_inbound_test.go::TestImageInboundTransformer_
    // TransformRequest_Generation_JSON` and `_Edit_Multipart_*`
    // (TransformRequest cases only).
    // -------------------------------------------------------------------------

    // Mirrors Go `TestImageInboundTransformer_TransformRequest_Generation_JSON`:
    // all typed fields round-trip onto the unified ImageRequest; `model` is
    // kept; stream is hard-false; modalities is not modeled on the Rust
    // unified LlmRequest (it lives on the Go `Request.Modalities` slice) so
    // we only assert stream/payload here.
    #[test]
    fn image_generation_body_normalizes_json_request_and_preserves_fields() -> TransformerResult<()>
    {
        let request = normalize_image_body_with_format(
            json!({
                "prompt": "a cat",
                "model": "dall-e-3",
                "n": 2,
                "response_format": "url",
                "size": "1024x1024",
                "user": "u1",
                "quality": "hd",
                "style": "vivid",
                "background": "transparent",
                "output_format": "png",
                "output_compression": 80,
                "moderation": "low",
                "partial_images": 2
            }),
            RequestType::Image,
            ApiFormat::OpenAiImageGeneration,
        )?;

        assert_eq!(request.request_type, RequestType::Image);
        assert_eq!(request.api_format, ApiFormat::OpenAiImageGeneration);
        assert_eq!(request.model.as_deref(), Some("dall-e-3"));
        // Images never stream.
        assert!(!request.stream);

        let LlmRequestPayload::Image(payload) = request.payload else {
            panic!("expected image payload");
        };
        assert_eq!(payload.prompt.as_deref(), Some("a cat"));
        assert_eq!(payload.n, Some(2));
        assert_eq!(payload.response_format.as_deref(), Some("url"));
        assert_eq!(payload.size.as_deref(), Some("1024x1024"));
        assert_eq!(payload.quality.as_deref(), Some("hd"));
        assert_eq!(payload.style.as_deref(), Some("vivid"));
        // Unmodeled generation fields ride via `extra`.
        assert_eq!(payload.extra.get("user"), Some(&json!("u1")));
        assert_eq!(payload.extra.get("background"), Some(&json!("transparent")));
        assert_eq!(payload.extra.get("output_format"), Some(&json!("png")));
        assert_eq!(payload.extra.get("output_compression"), Some(&json!(80)));
        assert_eq!(payload.extra.get("moderation"), Some(&json!("low")));
        assert_eq!(payload.extra.get("partial_images"), Some(&json!(2)));
        Ok(())
    }

    // Mirrors Go `transformGenerationRequest`: `model` defaults to
    // `"dall-e-2"` when absent.
    #[test]
    fn image_generation_body_defaults_model_to_dall_e_2() -> TransformerResult<()> {
        let request = normalize_image_body_with_format(
            json!({"prompt": "a cat"}),
            RequestType::Image,
            ApiFormat::OpenAiImageGeneration,
        )?;
        assert_eq!(request.model.as_deref(), Some("dall-e-2"));
        Ok(())
    }

    // Mirrors Go case: missing `prompt` -> `"prompt is required"` for
    // generations, `"prompt is required for image edits"` for edits.
    #[test]
    fn image_body_rejects_missing_prompt_with_endpoint_specific_message() {
        match normalize_image_body_with_format(
            json!({"model": "dall-e-3"}),
            RequestType::Image,
            ApiFormat::OpenAiImageGeneration,
        ) {
            Err(err) => assert!(err.to_string().contains("prompt is required")),
            Ok(_) => panic!("expected prompt-required error"),
        }

        match normalize_image_body_with_format(
            json!({"model": "dall-e-2", "image": "data:image/png;base64,AAA"}),
            RequestType::Image,
            ApiFormat::OpenAiImageEdit,
        ) {
            Err(err) => assert!(
                err.to_string()
                    .contains("prompt is required for image edits"),
                "edits endpoint must surface its own error string"
            ),
            Ok(_) => panic!("expected edits prompt-required error"),
        }
    }

    // Mirrors Go `transformGenerationRequest` stream guard: an inbound
    // `stream:true` must be rejected with `"image generation does not support
    // streaming"`.
    #[test]
    fn image_generation_body_rejects_inbound_stream_true() {
        match normalize_image_body_with_format(
            json!({"prompt": "a cat", "stream": true}),
            RequestType::Image,
            ApiFormat::OpenAiImageGeneration,
        ) {
            Err(err) => assert!(
                err.to_string()
                    .contains("image generation does not support streaming")
            ),
            Ok(_) => panic!("expected stream rejection"),
        }
    }

    // Mirrors Go `transformEditRequest`: under the JSON-view contract the
    // gateway supplies `image` (data URL) + scalar fields; `prompt` + `image`
    // + `model` round-trip and `n` / `size` / `quality` / `response_format`
    // land on first-class slots.
    #[test]
    fn image_edit_body_normalizes_multipart_json_view_with_mask() -> TransformerResult<()> {
        let request = normalize_image_body_with_format(
            json!({
                "prompt": "make it blue",
                "model": "dall-e-2",
                "response_format": "b64_json",
                "n": 1,
                "size": "512x512",
                "image": "data:image/png;base64,aW1n",
                "mask": "data:image/png;base64,bXNr",
                "input_fidelity": "high"
            }),
            RequestType::Image,
            ApiFormat::OpenAiImageEdit,
        )?;

        assert_eq!(request.request_type, RequestType::Image);
        assert_eq!(request.api_format, ApiFormat::OpenAiImageEdit);
        assert_eq!(request.model.as_deref(), Some("dall-e-2"));
        assert!(!request.stream);

        let LlmRequestPayload::Image(payload) = request.payload else {
            panic!("expected image payload");
        };
        assert_eq!(payload.prompt.as_deref(), Some("make it blue"));
        assert_eq!(payload.response_format.as_deref(), Some("b64_json"));
        assert_eq!(payload.n, Some(1));
        assert_eq!(payload.size.as_deref(), Some("512x512"));
        assert_eq!(payload.image, Some(json!("data:image/png;base64,aW1n")));
        assert_eq!(payload.mask, Some(json!("data:image/png;base64,bXNr")));
        // `input_fidelity` has no first-class slot on the Rust unified
        // ImageRequest -> preserved via `extra`.
        assert_eq!(payload.extra.get("input_fidelity"), Some(&json!("high")));
        Ok(())
    }

    // Mirrors Go case: edits require an `image` part; under the JSON-view
    // contract an absent/null `image` surfaces the Go-shaped error.
    #[test]
    fn image_edit_body_rejects_missing_image_part() {
        match normalize_image_body_with_format(
            json!({"prompt": "edit", "model": "dall-e-2"}),
            RequestType::Image,
            ApiFormat::OpenAiImageEdit,
        ) {
            Err(err) => assert!(
                err.to_string()
                    .contains("at least one image is required for edits"),
                "expected edits image-required error"
            ),
            Ok(_) => panic!("expected image-required error"),
        }
    }

    // Mirrors Go case: edits accept the multi-image `image[]` form; under
    // the JSON-view contract the gateway surfaces it as a JSON array on the
    // `image` key, which round-trips losslessly through `ImageRequest.image`.
    #[test]
    fn image_edit_body_preserves_multi_image_array_form() -> TransformerResult<()> {
        let request = normalize_image_body_with_format(
            json!({
                "prompt": "combine these images",
                "model": "gpt-image-1.5",
                "image": [
                    "data:image/png;base64,aW1nMQ==",
                    "data:image/png;base64,aW1nMg=="
                ]
            }),
            RequestType::Image,
            ApiFormat::OpenAiImageEdit,
        )?;

        let LlmRequestPayload::Image(payload) = request.payload else {
            panic!("expected image payload");
        };
        assert_eq!(
            payload.image,
            Some(json!([
                "data:image/png;base64,aW1nMQ==",
                "data:image/png;base64,aW1nMg=="
            ]))
        );
        Ok(())
    }

    // Dispatcher parity: each of the two `/v1/images/*` S09 paths routes
    // through `normalize_openai_body` to the image normalizer with the
    // correct `ApiFormat` + `RequestType::Image` (instead of the
    // `_ => Err` fallback).
    #[test]
    fn openai_request_routes_each_image_path_to_image_payload() -> TransformerResult<()> {
        for (path, api_format, body) in [
            (
                IMAGES_GENERATIONS_PATH,
                ApiFormat::OpenAiImageGeneration,
                json!({"prompt": "a cat", "model": "dall-e-3"}),
            ),
            (
                IMAGES_EDITS_PATH,
                ApiFormat::OpenAiImageEdit,
                json!({
                    "prompt": "edit",
                    "model": "dall-e-2",
                    "image": "data:image/png;base64,AAA"
                }),
            ),
        ] {
            let request = normalize_openai_request(HttpRequest {
                method: "POST".to_string(),
                path: path.to_string(),
                json_body: Some(body),
                ..HttpRequest::default()
            })?;

            assert_eq!(request.api_format, api_format, "api_format for {path}");
            assert_eq!(
                request.request_type,
                RequestType::Image,
                "request_type for {path}"
            );
            assert!(
                matches!(request.payload, LlmRequestPayload::Image(_)),
                "expected image payload for {path}"
            );
        }
        Ok(())
    }

    // -------------------------------------------------------------------------
    // `normalize_video_body` (RUST-P7-001 S10) — parity tests with Go
    // `video_inbound_test.go::TestVideoInboundTransformer_TransformRequest_JSON`
    // (TransformRequest cases only).
    // -------------------------------------------------------------------------

    // Mirrors Go `TestVideoInboundTransformer_TransformRequest_JSON`: all
    // typed fields round-trip; the `input_reference` URL lands on the unified
    // `VideoRequest.image` slot; `seconds` → `duration`; `size` is preserved.
    #[test]
    fn video_body_normalizes_json_request_and_preserves_fields() -> TransformerResult<()> {
        let request = normalize_video_body(json!({
            "model": "sora-2",
            "prompt": "a cat walking",
            "input_reference": "https://example.com/a.png",
            "seconds": "8",
            "size": "1280x720"
        }))?;

        assert_eq!(request.request_type, RequestType::Video);
        assert_eq!(request.api_format, ApiFormat::OpenAiVideo);
        assert_eq!(request.model.as_deref(), Some("sora-2"));
        // Videos never stream.
        assert!(!request.stream);

        let LlmRequestPayload::Video(payload) = request.payload else {
            panic!("expected video payload");
        };
        assert_eq!(payload.prompt.as_deref(), Some("a cat walking"));
        assert_eq!(payload.image, Some(json!("https://example.com/a.png")));
        assert_eq!(payload.duration.as_deref(), Some("8"));
        assert_eq!(payload.size.as_deref(), Some("1280x720"));
        Ok(())
    }

    // Mirrors Go case: missing `model` -> `"model is required"`.
    #[test]
    fn video_body_rejects_missing_model() {
        match normalize_video_body(json!({"prompt": "a cat walking"})) {
            Err(err) => assert!(err.to_string().contains("model is required")),
            Ok(_) => panic!("expected model-required error"),
        }
    }

    // Mirrors Go case: missing `prompt` -> `"prompt is required"`.
    #[test]
    fn video_body_rejects_missing_prompt() {
        match normalize_video_body(json!({"model": "sora-2"})) {
            Err(err) => assert!(err.to_string().contains("prompt is required")),
            Ok(_) => panic!("expected prompt-required error"),
        }
    }

    // Mirrors Go case: an absent `input_reference` does not produce an
    // `image_url` content part — under the unified flat model that means the
    // `image` slot stays `None` and `duration`/`size` still round-trip.
    #[test]
    fn video_body_supports_text_only_request_without_input_reference() -> TransformerResult<()> {
        let request = normalize_video_body(json!({
            "model": "sora-2",
            "prompt": "a cat walking",
            "seconds": "5",
            "size": "1280x720"
        }))?;

        let LlmRequestPayload::Video(payload) = request.payload else {
            panic!("expected video payload");
        };
        assert!(payload.image.is_none());
        assert_eq!(payload.duration.as_deref(), Some("5"));
        assert_eq!(payload.size.as_deref(), Some("1280x720"));
        Ok(())
    }

    // Mirrors Go multipart case `TestVideoInboundTransformer_TransformRequest_
    // Multipart_WithInputReferenceFile`: under the JSON-view contract the
    // gateway surfaces the `input_reference` file part as a data URL on the
    // `input_reference` key, which round-trips losslessly through
    // `VideoRequest.image`.
    #[test]
    fn video_body_preserves_input_reference_data_url_from_multipart_json_view()
    -> TransformerResult<()> {
        let data_url = "data:image/png;base64,cG5nZGF0YQ==";
        let request = normalize_video_body(json!({
            "model": "sora-2",
            "prompt": "a cat walking",
            "input_reference": data_url
        }))?;

        let LlmRequestPayload::Video(payload) = request.payload else {
            panic!("expected video payload");
        };
        assert_eq!(payload.image, Some(json!(data_url)));
        Ok(())
    }

    // Dispatcher parity: `POST /v1/videos` routes through
    // `normalize_openai_body` to the video normalizer with
    // `ApiFormat::OpenAiVideo` + `RequestType::Video` (instead of the
    // `_ => Err` fallback).
    #[test]
    fn openai_request_routes_videos_path_to_video_payload() -> TransformerResult<()> {
        let request = normalize_openai_request(HttpRequest {
            method: "POST".to_string(),
            path: VIDEOS_PATH.to_string(),
            json_body: Some(json!({
                "model": "sora-2",
                "prompt": "a cat walking"
            })),
            ..HttpRequest::default()
        })?;

        assert_eq!(request.api_format, ApiFormat::OpenAiVideo);
        assert_eq!(request.request_type, RequestType::Video);
        assert!(matches!(request.payload, LlmRequestPayload::Video(_)));
        Ok(())
    }

    // ---- RUST-P8-002 S07 — InboundTransformer::aggregate_stream_chunks ----
    //
    // Mirrors the intent of Go `inbound_test.go` cases that exercise
    // `InboundTransformer.AggregateStreamChunks` end-to-end (SSE frames →
    // aggregated JSON body). The per-field folding parity is already covered
    // exhaustively by `aggregate_openai_stream_chunks`'s own test suite in
    // `openai_stream.rs`; this test focuses on the trait-method wiring: SSE
    // decode → fold → JSON serialize → HTTP response shape.

    #[test]
    fn chat_inbound_aggregate_stream_chunks_merges_content_and_sets_body() -> TransformerResult<()>
    {
        let inbound = OpenAiChatInbound::new();
        // Two streaming chunks whose `delta.content` concatenates to "Hello world".
        let chunk_json = json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion.chunk",
            "created": 1700000000_i64,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant"},
                "finish_reason": None::<String>,
            }],
        });
        let chunk_two = json!({
            "id": "chatcmpl-abc",
            "object": "chat.completion.chunk",
            "created": 1700000000_i64,
            "model": "gpt-4o-mini",
            "choices": [{
                "index": 0,
                "delta": {"content": "Hello world"},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 5_i64, "completion_tokens": 2_i64, "total_tokens": 7_i64},
        });
        let events = vec![
            StreamEvent {
                data: Some(chunk_json.to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                data: Some(chunk_two.to_string()),
                ..StreamEvent::default()
            },
        ];

        let response = inbound.aggregate_stream_chunks(events)?;

        // Go `non_streaming.go:122-125` sets these headers verbatim.
        assert_eq!(response.status, 200);
        assert_eq!(
            response.headers.get("Content-Type").map(|s| s.as_str()),
            Some("application/json")
        );
        assert_eq!(
            response.headers.get("Cache-Control").map(|s| s.as_str()),
            Some("no-cache")
        );

        // Body is the serialized aggregated LlmResponse.
        let body = response
            .body
            .as_ref()
            .ok_or_else(|| ConduitError::internal("expected non-empty body"))?;
        let aggregated: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
            ConduitError::internal("failed to parse aggregated body").with_source(e)
        })?;

        // `object` flips from `chat.completion.chunk` to `chat.completion`
        // (Go aggregator.go:356).
        assert_eq!(
            aggregated.get("object").and_then(|v| v.as_str()),
            Some("chat.completion")
        );
        assert_eq!(
            aggregated.get("id").and_then(|v| v.as_str()),
            Some("chatcmpl-abc")
        );

        // Content concatenated + finish_reason preserved.
        let choice = aggregated
            .get("choices")
            .and_then(|c| c.get(0))
            .ok_or_else(|| ConduitError::internal("missing choice"))?;
        assert_eq!(
            choice
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str()),
            Some("Hello world")
        );
        assert_eq!(
            choice.get("finish_reason").and_then(|f| f.as_str()),
            Some("stop")
        );

        // Last-wins usage frame (Go aggregator.go:236).
        let usage = aggregated
            .get("usage")
            .ok_or_else(|| ConduitError::internal("missing usage"))?;
        assert_eq!(usage.get("total_tokens").and_then(|t| t.as_i64()), Some(7));

        // Original events preserved on `stream` (lossless log for retry/debug).
        assert_eq!(response.stream.len(), 2);
        Ok(())
    }

    #[test]
    fn chat_inbound_aggregate_stream_chunks_rejects_empty_chunk_list() {
        let inbound = OpenAiChatInbound::new();
        // Go `non_streaming.go:105-108` surfaces `ErrEmptyStreamChunks` when
        // no chunks were collected. The Rust impl mirrors that as an
        // `invalid_request` error so the pipeline surfaces it faithfully.
        let err = match inbound.aggregate_stream_chunks(Vec::new()) {
            Ok(_) => panic!("expected an error for empty chunk list"),
            Err(err) => err,
        };
        assert_eq!(err.kind, conduit_core::ErrorKind::InvalidRequest);
    }

    #[test]
    fn chat_inbound_aggregate_stream_chunks_skips_done_sentinel() -> TransformerResult<()> {
        // `[DONE]` frames must be filtered out — Go's outer loop skips them
        // (aggregator.go:138-139) before invoking `chunkTransformer`.
        let inbound = OpenAiChatInbound::new();
        let chunk = json!({
            "id": "x",
            "object": "chat.completion.chunk",
            "created": 0_i64,
            "model": "m",
            "choices": [{
                "index": 0,
                "delta": {"content": "hi"},
                "finish_reason": "stop",
            }],
        });
        let events = vec![
            StreamEvent {
                data: Some(chunk.to_string()),
                ..StreamEvent::default()
            },
            StreamEvent {
                data: Some("[DONE]".to_string()),
                done: true,
                ..StreamEvent::default()
            },
        ];
        let response = inbound.aggregate_stream_chunks(events)?;
        let body = response.body.as_deref().unwrap_or(&[]);
        let aggregated: serde_json::Value = serde_json::from_slice(body).map_err(|e| {
            ConduitError::internal("failed to parse aggregated body").with_source(e)
        })?;
        // Only one real choice — the [DONE] sentinel was filtered.
        assert_eq!(
            aggregated
                .get("choices")
                .and_then(|c| c.as_array())
                .map(|a| a.len()),
            Some(1)
        );
        Ok(())
    }

    // -------------------------------------------------------------------------
    // RUST-P7-001 S17 — `convert_responses_input_to_messages` parity with Go
    // `convertInputToMessages` / `convertReasoningWithFollowing` /
    // `convertItemToMessage` (responses/inbound.go:329-602).
    // -------------------------------------------------------------------------

    /// Convenience: run the converter on a JSON literal and return the typed
    /// messages, panicking on error (mirrors `require.NoError` in the Go
    /// tests). Use the `?`-returning [`convert_responses_input_to_messages`]
    /// directly when assertions on the error path are needed.
    fn convert_input(input: Value) -> Vec<LlmMessage> {
        convert_responses_input_to_messages(&input)
            .unwrap_or_else(|err| panic!("convert_responses_input_to_messages failed: {err}"))
    }

    // Mirrors Go "simple text input" (inbound_test.go:59-73) — a bare-string
    // `input` produces a single user message carrying the text verbatim.
    #[test]
    fn s17_convert_input_string_to_single_user_message() {
        let messages = convert_input(json!("Hello, world!"));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role.as_deref(), Some("user"));
        assert_eq!(
            messages[0].content.as_ref(),
            Some(&MessageContent::Text("Hello, world!".to_string()))
        );
    }

    // Mirrors Go `TestConvertReasoningWithFollowing` case "reasoning item with
    // summary only" (inbound_test.go:1517-1538).
    #[test]
    fn s17_reasoning_with_summary_only() {
        let messages = convert_input(json!([{
            "id": "reasoning_123",
            "type": "reasoning",
            "summary": [
                {"type": "summary_text", "text": "First, I need to analyze the problem."},
                {"type": "summary_text", "text": " Then, I will solve it step by step."}
            ]
        }]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("assistant"));
        assert_eq!(
            msg.reasoning_content.as_deref(),
            Some("First, I need to analyze the problem. Then, I will solve it step by step.")
        );
        assert!(msg.reasoning_signature.is_none());
        assert!(msg.tool_calls.is_empty());
    }

    // Mirrors Go case "reasoning item with encrypted content"
    // (inbound_test.go:1539-1562).
    #[test]
    fn s17_reasoning_with_encrypted_content() {
        let messages = convert_input(json!([{
            "id": "reasoning_456",
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "Reasoning summary"}],
            "encrypted_content": "encrypted_data_here"
        }]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("assistant"));
        assert_eq!(msg.reasoning_content.as_deref(), Some("Reasoning summary"));
        assert_eq!(
            msg.reasoning_signature.as_deref(),
            Some("encrypted_data_here")
        );
    }

    // Mirrors Go case "reasoning item with empty summary"
    // (inbound_test.go:1563-1580) — empty `summary` array leaves
    // `reasoning_content` as `None`.
    #[test]
    fn s17_reasoning_with_empty_summary_yields_no_reasoning_content() {
        let messages = convert_input(json!([{
            "id": "reasoning_789",
            "type": "reasoning",
            "summary": []
        }]));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role.as_deref(), Some("assistant"));
        assert!(messages[0].reasoning_content.is_none());
    }

    // Mirrors Go case "reasoning merged with function_call"
    // (inbound_test.go:1581-1610) — the following function_call item is folded
    // into the same assistant message's `tool_calls`.
    #[test]
    fn s17_reasoning_merges_following_function_call() {
        let messages = convert_input(json!([
            {
                "id": "reasoning_001",
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "I need to call the function."}]
            },
            {
                "type": "function_call",
                "call_id": "call_123",
                "name": "get_weather",
                "arguments": "{\"location\": \"Tokyo\"}"
            }
        ]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("assistant"));
        assert_eq!(
            msg.reasoning_content.as_deref(),
            Some("I need to call the function.")
        );
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].id.as_deref(), Some("call_123"));
        assert_eq!(msg.tool_calls[0].call_type, "function");
        assert_eq!(
            msg.tool_calls[0].function.get("name"),
            Some(&json!("get_weather"))
        );
        assert_eq!(
            msg.tool_calls[0].function.get("arguments"),
            Some(&json!("{\"location\": \"Tokyo\"}"))
        );
    }

    // Mirrors Go case "reasoning merged with assistant text message"
    // (inbound_test.go:1611-1638).
    #[test]
    fn s17_reasoning_merges_following_assistant_text_message() {
        let messages = convert_input(json!([
            {
                "id": "reasoning_002",
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "Thinking about the answer."}]
            },
            {
                "type": "message",
                "role": "assistant",
                "text": "The answer is 42."
            }
        ]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("assistant"));
        assert_eq!(
            msg.reasoning_content.as_deref(),
            Some("Thinking about the answer.")
        );
        assert_eq!(
            msg.content.as_ref(),
            Some(&MessageContent::Text("The answer is 42.".to_string()))
        );
    }

    // Mirrors Go case "reasoning stops at user message"
    // (inbound_test.go:1639-1665) — a non-assistant following message halts the
    // merge; it is emitted as a standalone message instead.
    #[test]
    fn s17_reasoning_stops_at_user_message() {
        let messages = convert_input(json!([
            {
                "id": "reasoning_003",
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "Thinking..."}]
            },
            {
                "type": "message",
                "role": "user",
                "text": "Next question"
            }
        ]));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role.as_deref(), Some("assistant"));
        assert_eq!(
            messages[0].reasoning_content.as_deref(),
            Some("Thinking...")
        );
        assert!(messages[0].tool_calls.is_empty());
        assert_eq!(messages[1].role.as_deref(), Some("user"));
    }

    // Mirrors Go case "reasoning stops at function_call_output"
    // (inbound_test.go:1666-1690) — `function_call_output` halts the merge but
    // is itself converted to a tool-role message (Go inbound.go:558-572).
    #[test]
    fn s17_reasoning_stops_at_function_call_output_emits_tool_message() {
        let messages = convert_input(json!([
            {
                "id": "reasoning_004",
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "Thinking..."}]
            },
            {
                "type": "function_call_output",
                "call_id": "call_456",
                "output": "result"
            }
        ]));
        // Reasoning message + standalone tool message.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role.as_deref(), Some("assistant"));
        assert_eq!(
            messages[0].reasoning_content.as_deref(),
            Some("Thinking...")
        );
        assert!(messages[0].tool_calls.is_empty());
        assert_eq!(messages[1].role.as_deref(), Some("tool"));
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call_456"));
        assert_eq!(
            messages[1].content.as_ref(),
            Some(&MessageContent::Text("result".to_string()))
        );
    }

    // Mirrors Go `TestInboundTransformer_TransformRequest_WithReasoningInput`
    // case "request with reasoning input item merged with assistant"
    // (inbound_test.go:1710-1753) — end-to-end: a user message, then a
    // reasoning item, then an assistant text message, collapses to two
    // messages where the latter carries both reasoning and text content.
    #[test]
    fn s17_reasoning_input_merged_with_assistant_end_to_end() {
        let messages = convert_input(json!([
            {"type": "message", "role": "user", "content": "What is 2+2?"},
            {
                "type": "reasoning",
                "id": "reasoning_abc",
                "summary": [
                    {"type": "summary_text", "text": "Let me think about this math problem."}
                ]
            },
            {"type": "message", "role": "assistant", "content": "The answer is 4."}
        ]));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role.as_deref(), Some("user"));
        assert_eq!(
            messages[0].content.as_ref(),
            Some(&MessageContent::Text("What is 2+2?".to_string()))
        );
        let assistant = &messages[1];
        assert_eq!(assistant.role.as_deref(), Some("assistant"));
        assert_eq!(
            assistant.reasoning_content.as_deref(),
            Some("Let me think about this math problem.")
        );
        assert_eq!(
            assistant.content.as_ref(),
            Some(&MessageContent::Text("The answer is 4.".to_string()))
        );
    }

    // Mirrors Go `TestConvertToMessageContentParts` text cases
    // (inbound_test.go:1336-1436): an input item with structured content array
    // of text parts produces a multipart message, or collapses to a bare Text
    // when only a single text part is present.
    #[test]
    fn s17_message_with_structured_text_parts_array() {
        let messages = convert_input(json!([{
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "First"},
                {"type": "input_text", "text": "Second"}
            ]
        }]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("user"));
        let content = match msg.content.as_ref() {
            Some(c) => c,
            None => panic!("content present"),
        };
        match content {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0].part_type, "text");
                assert_eq!(parts[0].text.as_deref(), Some("First"));
                assert_eq!(parts[1].part_type, "text");
                assert_eq!(parts[1].text.as_deref(), Some("Second"));
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    // Single text content-item collapses to a bare Text content, mirroring
    // Go `convertToMessageContent` collapse rule (inbound.go:607-611).
    #[test]
    fn s17_message_with_single_structured_text_part_collapses_to_text() {
        let messages = convert_input(json!([{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Hello world"}]
        }]));
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].content.as_ref(),
            Some(&MessageContent::Text("Hello world".to_string()))
        );
    }

    // Mirrors Go `convertItemToMessage` case `function_call`
    // (inbound.go:519-534): a standalone function_call item produces an
    // assistant message with one tool call.
    #[test]
    fn s17_standalone_function_call_item_to_assistant_tool_call() {
        let messages = convert_input(json!([{
            "type": "function_call",
            "call_id": "call_42",
            "name": "get_weather",
            "namespace": "wx",
            "arguments": "{\"city\":\"SF\"}"
        }]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("assistant"));
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].id.as_deref(), Some("call_42"));
        assert_eq!(msg.tool_calls[0].call_type, "function");
        assert_eq!(
            msg.tool_calls[0].function.get("name"),
            Some(&json!("get_weather"))
        );
        assert_eq!(
            msg.tool_calls[0].function.get("namespace"),
            Some(&json!("wx"))
        );
        assert_eq!(
            msg.tool_calls[0].function.get("arguments"),
            Some(&json!("{\"city\":\"SF\"}"))
        );
    }

    // Only truly-unknown / unsupported item types are silently skipped
    // (matching Go's `default: return nil, nil`). `image_generation_call`,
    // `web_search_call`, and any future exotic types remain TODO (deferred).
    #[test]
    fn s17_unknown_item_types_are_silently_skipped() {
        let messages = convert_input(json!([
            {"type": "image_generation_call"},
            {"type": "web_search_call"},
            {"type": "future_unknown_type", "foo": "bar"}
        ]));
        assert!(
            messages.is_empty(),
            "expected no messages, got {messages:?}"
        );
    }

    // Mirrors Go `convertItemToMessage` case `input_image` (inbound.go:498-517):
    // a standalone input_image item with a URL produces a user-role message
    // with a single `image_url` content part carrying `{url, detail}`.
    #[test]
    fn s17_input_image_standalone_item_to_user_message_with_image_url_part() {
        let messages = convert_input(json!([{
            "type": "input_image",
            "image_url": "https://example.com/cat.png",
            "detail": "high"
        }]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("user"));
        let content = match msg.content.as_ref() {
            Some(c) => c,
            None => panic!("content present"),
        };
        match content {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].part_type, "image_url");
                let image_url = match parts[0].image_url.as_ref() {
                    Some(v) => v,
                    None => panic!("image_url obj"),
                };
                assert_eq!(
                    image_url.get("url").and_then(|v| v.as_str()),
                    Some("https://example.com/cat.png")
                );
                assert_eq!(
                    image_url.get("detail").and_then(|v| v.as_str()),
                    Some("high")
                );
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    // Mirrors Go: a standalone input_image with an explicit role preserves it
    // instead of defaulting to "user".
    #[test]
    fn s17_input_image_standalone_item_preserves_explicit_role() {
        let messages = convert_input(json!([{
            "type": "input_image",
            "role": "system",
            "image_url": "https://example.com/sys.png"
        }]));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role.as_deref(), Some("system"));
    }

    // Mirrors Go: input_image with no `image_url` is dropped (returns nil).
    #[test]
    fn s17_input_image_without_url_is_dropped() {
        let messages = convert_input(json!([{"type": "input_image"}]));
        assert!(messages.is_empty());
    }

    // Mirrors Go `TestConvertToMessageContentParts` case "single input_image
    // returns one part" (inbound_test.go:1383-1394): an input_image content
    // item inside a message's `content` array produces an `image_url` part.
    #[test]
    fn s17_input_image_content_part_in_message() {
        let messages = convert_input(json!([{
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_text", "text": "look:"},
                {"type": "input_image", "image_url": "https://example.com/i.png"}
            ]
        }]));
        assert_eq!(messages.len(), 1);
        let content = match messages[0].content.as_ref() {
            Some(c) => c,
            None => panic!("content present"),
        };
        match content {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[0].part_type, "text");
                assert_eq!(parts[1].part_type, "image_url");
                assert_eq!(
                    parts[1].image_url.as_ref().and_then(|v| v.get("url")),
                    Some(&json!("https://example.com/i.png"))
                );
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    // Mirrors Go `convertItemToMessage` case `function_call_output`
    // (inbound.go:558-572): tool-role message with `tool_call_id` + content
    // derived from the `output` field.
    #[test]
    fn s17_function_call_output_to_tool_message_with_string_output() {
        let messages = convert_input(json!([{
            "type": "function_call_output",
            "call_id": "call_9",
            "output": "sunny"
        }]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("tool"));
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_9"));
        assert_eq!(
            msg.content.as_ref(),
            Some(&MessageContent::Text("sunny".to_string()))
        );
    }

    // Mirrors Go: function_call_output with a `name` field populates
    // `tool_call_name`.
    #[test]
    fn s17_function_call_output_with_name_populates_tool_call_name() {
        let messages = convert_input(json!([{
            "type": "function_call_output",
            "call_id": "c",
            "name": "get_weather",
            "output": "r"
        }]));
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].tool_call_name.as_deref(), Some("get_weather"));
    }

    // Mirrors Go: missing `output` on function_call_output yields a 400.
    #[test]
    fn s17_function_call_output_rejects_missing_output() {
        let err = convert_responses_input_to_messages(&json!([{
            "type": "function_call_output",
            "call_id": "c"
        }]))
        .err();
        assert!(err.is_some(), "expected an error");
        assert!(
            err.map(|e| e.to_string().contains("non-nil Output"))
                .unwrap_or(false),
            "expected non-nil Output error"
        );
    }

    // Mirrors Go `convertItemToMessage` case `custom_tool_call`
    // (inbound.go:536-556): assistant message with a single tool call whose
    // type is `responses_custom_tool`; the `ResponseCustomToolCall` payload
    // rides on `extra` (the Rust unified `ToolCall` has no first-class slot).
    #[test]
    fn s17_custom_tool_call_to_assistant_tool_call() {
        let messages = convert_input(json!([{
            "type": "custom_tool_call",
            "call_id": "ct_1",
            "name": "search_web",
            "input": "query: cats"
        }]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("assistant"));
        assert_eq!(msg.tool_calls.len(), 1);
        let tc = &msg.tool_calls[0];
        assert_eq!(tc.id.as_deref(), Some("ct_1"));
        assert_eq!(tc.call_type, "responses_custom_tool");
        let rctc = match tc.extra.get("response_custom_tool_call") {
            Some(v) => v,
            None => panic!("missing response_custom_tool_call"),
        };
        assert_eq!(rctc.get("call_id"), Some(&json!("ct_1")));
        assert_eq!(rctc.get("name"), Some(&json!("search_web")));
        assert_eq!(rctc.get("input"), Some(&json!("query: cats")));
    }

    // Mirrors Go: custom_tool_call without `input` defaults to empty string.
    #[test]
    fn s17_custom_tool_call_without_input_defaults_to_empty() {
        let messages = convert_input(json!([{
            "type": "custom_tool_call",
            "call_id": "ct_2",
            "name": "n"
        }]));
        assert_eq!(messages.len(), 1);
        let rctc = match messages[0].tool_calls[0]
            .extra
            .get("response_custom_tool_call")
        {
            Some(v) => v,
            None => panic!("missing"),
        };
        assert_eq!(rctc.get("input"), Some(&json!("")));
    }

    // Mirrors Go: custom_tool_call_output produces the same tool-message
    // shape as function_call_output.
    #[test]
    fn s17_custom_tool_call_output_to_tool_message() {
        let messages = convert_input(json!([{
            "type": "custom_tool_call_output",
            "call_id": "ct_3",
            "name": "search_web",
            "output": "result text"
        }]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("tool"));
        assert_eq!(msg.tool_call_id.as_deref(), Some("ct_3"));
        assert_eq!(msg.tool_call_name.as_deref(), Some("search_web"));
        assert_eq!(
            msg.content.as_ref(),
            Some(&MessageContent::Text("result text".to_string()))
        );
    }

    // Mirrors Go: reasoning + custom_tool_call merge into a single assistant
    // message (Go inbound.go:430-446).
    #[test]
    fn s17_reasoning_merges_following_custom_tool_call() {
        let messages = convert_input(json!([
            {
                "type": "reasoning",
                "summary": [{"type": "summary_text", "text": "think"}]
            },
            {
                "type": "custom_tool_call",
                "call_id": "ct_m",
                "name": "n",
                "input": "i"
            }
        ]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("assistant"));
        assert_eq!(msg.reasoning_content.as_deref(), Some("think"));
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].call_type, "responses_custom_tool");
    }

    // Mirrors Go `TestConvertItemToMessage_Compaction` (inbound_test.go:887-975):
    // a compaction item produces an assistant message whose single content
    // part carries `id` / `encrypted_content` / `created_by` under
    // `extra.compact` (the Rust unified `ContentPart` has no first-class
    // compact slot).
    #[test]
    fn s17_compaction_item_to_assistant_message_with_compact_part() {
        let messages = convert_input(json!([{
            "type": "compaction",
            "id": "compaction_123",
            "encrypted_content": "encrypted_data_here",
            "created_by": "assistant"
        }]));
        assert_eq!(messages.len(), 1);
        let msg = &messages[0];
        assert_eq!(msg.role.as_deref(), Some("assistant"));
        let content = match msg.content.as_ref() {
            Some(c) => c,
            None => panic!("content present"),
        };
        match content {
            MessageContent::Parts(parts) => {
                assert_eq!(parts.len(), 1);
                assert_eq!(parts[0].part_type, "compaction");
                let compact = match parts[0].extra.get("compact") {
                    Some(v) => v,
                    None => panic!("missing compact"),
                };
                assert_eq!(compact.get("id"), Some(&json!("compaction_123")));
                assert_eq!(
                    compact.get("encrypted_content"),
                    Some(&json!("encrypted_data_here"))
                );
                assert_eq!(compact.get("created_by"), Some(&json!("assistant")));
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    // Mirrors Go compaction case "compaction item without created_by"
    // (inbound_test.go:914-932).
    #[test]
    fn s17_compaction_item_without_created_by() {
        let messages = convert_input(json!([{
            "type": "compaction",
            "id": "compaction_456",
            "encrypted_content": "encrypted_only"
        }]));
        assert_eq!(messages.len(), 1);
        let content = match messages[0].content.as_ref() {
            Some(c) => c,
            None => panic!("content present"),
        };
        match content {
            MessageContent::Parts(parts) => {
                let compact = match parts[0].extra.get("compact") {
                    Some(v) => v,
                    None => panic!("missing"),
                };
                assert!(compact.get("created_by").is_none());
                assert_eq!(compact.get("id"), Some(&json!("compaction_456")));
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    // Mirrors Go `compaction_summary` variant — same shape with the
    // echoed item type on the content part.
    #[test]
    fn s17_compaction_summary_item_emits_compaction_summary_part() {
        let messages = convert_input(json!([{
            "type": "compaction_summary",
            "id": "cs_1",
            "encrypted_content": "enc"
        }]));
        assert_eq!(messages.len(), 1);
        let content = match messages[0].content.as_ref() {
            Some(c) => c,
            None => panic!("content present"),
        };
        match content {
            MessageContent::Parts(parts) => {
                assert_eq!(parts[0].part_type, "compaction_summary");
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    // Mirrors Go `TestInboundTransformer_TransformRequest_WithCompactionInput`
    // (inbound_test.go:1029-1093): end-to-end conversion of a message +
    // compaction item sequence.
    #[test]
    fn s17_end_to_end_message_then_compaction() {
        let messages = convert_input(json!([
            {"type": "message", "role": "user", "content": "Hello"},
            {
                "type": "compaction",
                "id": "compaction_abc",
                "encrypted_content": "base64encoded",
                "created_by": "assistant"
            }
        ]));
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role.as_deref(), Some("user"));
        assert_eq!(
            messages[0].content.as_ref(),
            Some(&MessageContent::Text("Hello".to_string()))
        );
        assert_eq!(messages[1].role.as_deref(), Some("assistant"));
    }

    // Mirrors Go `Input.UnmarshalJSON` rejection (model.go:348-366): a
    // non-string / non-array `input` is malformed.
    #[test]
    fn s17_non_string_non_array_input_is_rejected() {
        let err = convert_responses_input_to_messages(&json!({"unexpected": "shape"})).err();
        assert!(err.is_some(), "expected an error");
        assert!(
            err.map(|e| e.to_string().contains("must be a string or array"))
                .unwrap_or(false),
            "expected shape error"
        );
    }

    // A null input yields an empty message slice, matching Go's
    // `convertInputToMessages(nil-input)` short-circuit.
    #[test]
    fn s17_null_input_yields_empty_messages() {
        let messages = convert_input(Value::Null);
        assert!(messages.is_empty());
    }

    // End-to-end wiring: `OpenAiResponsesInbound::inbound_request` attaches the
    // typed messages to `metadata[RESPONSES_INPUT_MESSAGES_METADATA_KEY]` for
    // the common cases, mirroring Go's `chatReq.Messages` population.
    #[test]
    fn s17_inbound_wires_typed_messages_to_metadata_for_common_cases() -> TransformerResult<()> {
        let inbound = OpenAiResponsesInbound::new();
        let req = inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            json_body: Some(json!({
                "model": "gpt-4o",
                "input": [
                    {"type": "message", "role": "user", "content": "Hello"},
                    {
                        "type": "reasoning",
                        "summary": [{"type": "summary_text", "text": "think"}]
                    },
                    {
                        "type": "function_call",
                        "call_id": "c1",
                        "name": "do",
                        "arguments": "{}"
                    }
                ]
            })),
            ..HttpRequest::default()
        })?;

        let messages_value = match req.metadata.get(RESPONSES_INPUT_MESSAGES_METADATA_KEY) {
            Some(v) => v,
            None => {
                return Err(ConduitError::internal(
                    "typed messages must be attached for common-case input",
                ));
            }
        };
        let messages: Vec<LlmMessage> =
            serde_json::from_value(messages_value.clone()).map_err(|err| {
                ConduitError::internal("failed to deserialize messages").with_source(err)
            })?;
        // reasoning + function_call merge into a single assistant message;
        // user message stands alone -> 2 messages total.
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role.as_deref(), Some("user"));
        assert_eq!(messages[1].role.as_deref(), Some("assistant"));
        assert_eq!(messages[1].reasoning_content.as_deref(), Some("think"));
        assert_eq!(messages[1].tool_calls.len(), 1);
        Ok(())
    }

    // End-to-end wiring: when `input` is absent, no metadata key is set.
    #[test]
    fn s17_inbound_leaves_metadata_absent_when_input_is_absent() -> TransformerResult<()> {
        let inbound = OpenAiResponsesInbound::new();
        let req = inbound.inbound_request(HttpRequest {
            method: "POST".to_string(),
            json_body: Some(json!({"model": "gpt-4o"})),
            ..HttpRequest::default()
        })?;
        assert!(
            req.metadata
                .get(RESPONSES_INPUT_MESSAGES_METADATA_KEY)
                .is_none(),
            "no metadata key expected when input is absent"
        );
        Ok(())
    }
}
