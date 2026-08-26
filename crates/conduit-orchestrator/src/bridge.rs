//! HTTP→orchestrator bridge: the inbound half of
//! `OpenAiOrchestratorService::process` — turn a client [`HttpRequest`] into a
//! [`LlmRequest`] (via the route's inbound transformer) and resolve candidate
//! channels for it. This is the part of the `/v1/chat/completions` path that
//! does NOT depend on ④ (per-channel outbound): it stops at candidate
//! selection, leaving the outbound-transform + execute + response mapping to
//! the ⑥ completion once ④ lands.
//!
//! Wiring this with the real `OpenAiChatInbound` transformer + `DbCandidateSource`
//! is the first time a real OpenAI request flows through inbound-transform →
//! candidate selection (the e2e test uses fake transformers throughout).

use conduit_core::ConduitError;
use conduit_llm::{HttpRequest, LlmRequest};
use conduit_transformers::InboundTransformer;

use crate::candidates::{
    CandidateRequest, ChannelModelsCandidate, RequestContentPart, RequestMessage,
    RequestMessageContent, RequestTool, RequestToolCall, SelectionDiagnostics,
};
use crate::orchestrator::CandidateSource;

/// Run the inbound transformer over a client request, build the
/// [`CandidateRequest`], and resolve candidate channels.
///
/// Returns the materialised [`LlmRequest`] (the ⑥ completion needs it for the
/// outbound transform) alongside the selected candidates.
pub async fn resolve_candidates(
    inbound: &dyn InboundTransformer,
    http_request: HttpRequest,
    source: &dyn CandidateSource,
) -> Result<
    (
        LlmRequest,
        Vec<ChannelModelsCandidate>,
        SelectionDiagnostics,
    ),
    ConduitError,
> {
    let llm_request = inbound
        .inbound_request(http_request)
        .map_err(|err| ConduitError::internal(format!("inbound transform failed: {err}")))?;

    let candidate_request = build_candidate_request(&llm_request);
    let (candidates, diagnostics) = source.select_with_diagnostics(&candidate_request).await?;
    Ok((llm_request, candidates, diagnostics))
}

/// Map an [`LlmRequest`] onto the selector's [`CandidateRequest`].
///
/// Carries the scalar dimensions the selector filters on (model, request type,
/// api format, stream, image-request flag). Message/tool content conversion
/// (`ChatMessage`→`RequestMessage`, `UnifiedTool`→`RequestTool`) lands with the
/// ⑥ outbound/response completion — `select_legacy` matches by model-entry map
/// and does not consult messages, so this is sufficient to drive selection.
pub(crate) fn build_candidate_request(llm_request: &LlmRequest) -> CandidateRequest {
    let model = llm_request.model.clone().unwrap_or_default();
    let mut request = CandidateRequest::new(
        model,
        llm_request.request_type,
        llm_request.api_format.as_str().to_string(),
    )
    .with_stream(llm_request.stream);

    request.project_channel_ids = metadata_model_channel_list(
        &llm_request.metadata,
        "project_channels_by_model",
        &request.model,
    )
    .unwrap_or_else(|| metadata_string_list(&llm_request.metadata, "project_channel_ids"));
    request.project_upstream_models_by_channel = metadata_model_string_map(
        &llm_request.metadata,
        "project_upstream_models_by_model",
        &request.model,
    );
    request.project_channel_tags =
        metadata_string_list(&llm_request.metadata, "project_channel_tags");
    request.project_channel_tags_match_mode =
        metadata_string(&llm_request.metadata, "project_channel_tags_match_mode");
    request.key_channel_ids = metadata_string_list(&llm_request.metadata, "api_key_channel_ids");
    request.key_channel_tags = metadata_string_list(&llm_request.metadata, "api_key_channel_tags");
    request.key_channel_tags_match_mode =
        metadata_string(&llm_request.metadata, "api_key_channel_tags_match_mode");
    match &llm_request.payload {
        conduit_llm::LlmRequestPayload::Chat(chat) => {
            request.max_output_tokens = chat.max_tokens;
            request.messages = chat.messages.iter().map(candidate_message).collect();
            request.tools = chat.tools.iter().map(candidate_tool).collect();
            request.is_image_request = chat.messages.iter().any(|message| {
                matches!(
                    &message.content,
                    Some(conduit_llm::MessageContent::Parts(parts))
                        if parts.iter().any(|part| part.image_url.is_some())
                )
            });
        }
        conduit_llm::LlmRequestPayload::Responses(responses) => {
            request.max_output_tokens = responses
                .extra
                .get("max_output_tokens")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            request.tools = responses.tools.iter().map(candidate_tool).collect();
        }
        conduit_llm::LlmRequestPayload::Completion(completion) => {
            request.max_output_tokens = completion.max_tokens;
        }
        conduit_llm::LlmRequestPayload::Image(_) => request.is_image_request = true,
        _ => {}
    }
    request
}

fn metadata_model_string_map(
    metadata: &std::collections::BTreeMap<String, serde_json::Value>,
    key: &str,
    model: &str,
) -> std::collections::BTreeMap<String, String> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_object)
        .and_then(|models| models.get(model))
        .and_then(serde_json::Value::as_object)
        .map(|channels| {
            channels
                .iter()
                .filter_map(|(channel, upstream)| {
                    upstream
                        .as_str()
                        .map(|upstream| (channel.clone(), upstream.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn metadata_model_channel_list(
    metadata: &std::collections::BTreeMap<String, serde_json::Value>,
    key: &str,
    model: &str,
) -> Option<Vec<String>> {
    let values = metadata.get(key)?.as_object()?.get(model)?.as_array()?;
    Some(
        values
            .iter()
            .filter_map(|value| match value {
                serde_json::Value::String(value) => Some(value.clone()),
                serde_json::Value::Number(value) => Some(value.to_string()),
                _ => None,
            })
            .collect(),
    )
}

fn candidate_message(message: &conduit_llm::ChatMessage) -> RequestMessage {
    let content = match &message.content {
        Some(conduit_llm::MessageContent::Text(text)) => RequestMessageContent::Text(text.clone()),
        Some(conduit_llm::MessageContent::Parts(parts)) => RequestMessageContent::Parts(
            parts
                .iter()
                .map(|part| RequestContentPart {
                    part_type: part.part_type.clone(),
                    text: part.text.clone(),
                    image_url: part.image_url.as_ref().map(value_as_string),
                    input_audio: part.input_audio.as_ref().map(value_as_string),
                    video_url: part.extra.get("video_url").map(value_as_string),
                    document: part.extra.get("document").map(value_as_string),
                })
                .collect(),
        ),
        Some(conduit_llm::MessageContent::Json(value)) => {
            RequestMessageContent::Text(value_as_string(value))
        }
        None => RequestMessageContent::Empty,
    };
    RequestMessage {
        role: message.role.clone(),
        name: message.name.clone(),
        tool_call_id: message.tool_call_id.clone(),
        tool_call_name: None,
        reasoning_content: message
            .extra
            .get("reasoning_content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        content,
        tool_calls: message
            .tool_calls
            .iter()
            .map(|call| RequestToolCall {
                call_type: call.call_type.clone(),
                function_name: call
                    .function
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                function_arguments: call
                    .function
                    .get("arguments")
                    .map(value_as_string)
                    .unwrap_or_default(),
            })
            .collect(),
    }
}

fn candidate_tool(tool: &conduit_llm::UnifiedTool) -> RequestTool {
    RequestTool {
        tool_type: tool.tool_type.clone(),
        function_name: tool.name.clone().unwrap_or_default(),
        function_description: tool.description.clone().unwrap_or_default(),
        function_parameters: tool
            .parameters
            .as_ref()
            .map(value_as_string)
            .unwrap_or_default(),
    }
}

fn value_as_string(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn metadata_string(
    metadata: &std::collections::BTreeMap<String, serde_json::Value>,
    key: &str,
) -> String {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn metadata_string_list(
    metadata: &std::collections::BTreeMap<String, serde_json::Value>,
    key: &str,
) -> Vec<String> {
    metadata
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_i64().map(|id| id.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_llm::RequestType;
    use conduit_transformers::openai::OpenAiChatInbound;

    /// A real OpenAI chat-completions request flows through the real
    /// `OpenAiChatInbound` transformer into a `CandidateRequest`, and a source
    /// that offers `gpt-4` yields one candidate. Proves the inbound→select
    /// bridge works with production transformers (not just fakes).
    #[tokio::test]
    async fn inbound_request_resolves_to_candidate_via_real_transformer() {
        struct StaticSource;
        #[async_trait::async_trait]
        impl CandidateSource for StaticSource {
            async fn select(
                &self,
                request: &CandidateRequest,
            ) -> Result<Vec<ChannelModelsCandidate>, ConduitError> {
                if request.model == "gpt-4" {
                    Ok(vec![ChannelModelsCandidate {
                        channel_id: "ch-1".to_string(),
                        channel_name: "OpenAI".to_string(),
                        ordering_weight: 0,
                        priority: 0,
                        models: vec![],
                        endpoint: conduit_core::objects::channel_settings::ChannelEndpoint {
                            api_format: request.api_format.clone(),
                            ..Default::default()
                        },
                        api_format: request.api_format.clone(),
                        channel_type: "openai".to_string(),
                        policies: Default::default(),
                        credential_key_identity: String::new(),
                        tags: vec![],
                        base_url: None,
                        active_credential: None,
                        enabled_credentials: Vec::new(),
                        settings: None,
                        theoretical_cost_accounting: None,
                        cost_efficiency_score: 0,
                    }])
                } else {
                    Ok(vec![])
                }
            }
        }

        let http_request = HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            json_body: Some(serde_json::json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "Hello!"}],
            })),
            content_type: Some("application/json".to_string()),
            ..HttpRequest::default()
        };

        let (llm_request, candidates, diagnostics) =
            resolve_candidates(&OpenAiChatInbound::new(), http_request, &StaticSource)
                .await
                .unwrap_or_else(|err| panic!("bridge must succeed: {err}"));

        assert_eq!(llm_request.model.as_deref(), Some("gpt-4"));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].channel_id, "ch-1");
        assert_eq!(diagnostics.selected_count(), 1);
    }

    /// `build_candidate_request` carries the request-type / api-format / stream
    /// dimensions through to the selector input. Built via the real inbound
    /// transformer (avoids constructing `LlmRequest` literally).
    #[tokio::test]
    async fn build_candidate_request_carries_scalar_dimensions() {
        let http_request = HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            json_body: Some(serde_json::json!({
                "model": "gpt-4",
                "stream": true,
                "messages": [{"role": "user", "content": "Hi"}],
            })),
            content_type: Some("application/json".to_string()),
            ..HttpRequest::default()
        };
        let llm_request = OpenAiChatInbound::new()
            .inbound_request(http_request)
            .unwrap_or_else(|err| panic!("inbound transform failed: {err}"));

        let req = build_candidate_request(&llm_request);

        assert_eq!(req.model, "gpt-4");
        assert_eq!(req.request_type, RequestType::Chat);
        assert!(
            req.api_format.contains("openai"),
            "api_format should be the openai chat-completions format: got {}",
            req.api_format
        );
        assert!(req.stream);
    }

    #[test]
    fn build_candidate_request_carries_profile_filters() {
        let http_request = HttpRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            json_body: Some(serde_json::json!({
                "model": "gpt-4",
                "messages": [{"role": "user", "content": "hello"}]
            })),
            content_type: Some("application/json".to_string()),
            ..HttpRequest::default()
        };
        let mut llm_request = OpenAiChatInbound::new()
            .inbound_request(http_request)
            .unwrap_or_else(|error| panic!("inbound transform failed: {error}"));
        llm_request
            .metadata
            .insert("project_channel_ids".to_string(), serde_json::json!([1, 2]));
        llm_request.metadata.insert(
            "project_channel_tags".to_string(),
            serde_json::json!(["prod"]),
        );
        llm_request
            .metadata
            .insert("api_key_channel_ids".to_string(), serde_json::json!([2]));
        llm_request.metadata.insert(
            "api_key_channel_tags_match_mode".to_string(),
            serde_json::json!("all"),
        );
        llm_request.metadata.insert(
            "project_upstream_models_by_model".to_string(),
            serde_json::json!({
                "gpt-4": { "1": "gpt-4-0613", "2": "provider/gpt-4" }
            }),
        );

        let request = build_candidate_request(&llm_request);
        assert_eq!(request.project_channel_ids, ["1", "2"]);
        assert_eq!(request.project_channel_tags, ["prod"]);
        assert_eq!(request.key_channel_ids, ["2"]);
        assert_eq!(request.key_channel_tags_match_mode, "all");
        assert_eq!(
            request.project_upstream_models_by_channel,
            std::collections::BTreeMap::from([
                ("1".to_string(), "gpt-4-0613".to_string()),
                ("2".to_string(), "provider/gpt-4".to_string()),
            ])
        );
    }
}
