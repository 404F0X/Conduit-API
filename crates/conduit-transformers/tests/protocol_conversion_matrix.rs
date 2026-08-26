use conduit_llm::{ApiFormat, HttpRequest, LlmRequest, LlmRequestPayload};
use conduit_transformers::gemini::{GeminiOutboundConfig, GeminiPlatformType};
use conduit_transformers::{
    AnthropicInboundTransformer, AnthropicOutboundConfig, AnthropicOutboundTransformer,
    GeminiInboundTransformer, GeminiOutboundTransformer, InboundTransformer, OpenAiChatInbound,
    OpenAiResponsesOutbound, OutboundTransformer,
};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn normalized_client_requests()
-> Result<Vec<(&'static str, LlmRequest)>, Box<dyn std::error::Error>> {
    let openai = OpenAiChatInbound::new().inbound_request(HttpRequest {
        method: "POST".to_string(),
        path: "/v1/chat/completions".to_string(),
        content_type: Some("application/json".to_string()),
        json_body: Some(json!({
            "model": "client-openai-model",
            "messages": [{"role": "user", "content": "hello"}]
        })),
        ..HttpRequest::default()
    })?;

    let anthropic = AnthropicInboundTransformer::new().inbound_request(HttpRequest {
        method: "POST".to_string(),
        path: "/v1/messages".to_string(),
        json_body: Some(json!({
            "model": "client-claude-model",
            "max_tokens": 256,
            "messages": [{"role": "user", "content": "hello"}]
        })),
        ..HttpRequest::default()
    })?;

    let gemini_body = json!({
        "contents": [{"role": "user", "parts": [{"text": "hello"}]}]
    });
    let gemini = GeminiInboundTransformer::new().inbound_request(HttpRequest {
        method: "POST".to_string(),
        path: "/v1beta/models/client-gemini-model:generateContent".to_string(),
        body: Some(serde_json::to_vec(&gemini_body)?),
        ..HttpRequest::default()
    })?;

    Ok(vec![
        ("openai", openai),
        ("anthropic", anthropic),
        ("gemini", gemini),
    ])
}

fn body_json(request: &HttpRequest) -> Result<Value, Box<dyn std::error::Error>> {
    if let Some(value) = request.json_body.clone() {
        return Ok(value);
    }
    let bytes = request
        .body
        .as_deref()
        .ok_or("outbound request did not contain a body")?;
    Ok(serde_json::from_slice(bytes)?)
}

#[test]
fn three_client_protocols_convert_to_all_supported_upstream_protocols() -> TestResult {
    let anthropic = AnthropicOutboundTransformer::new(AnthropicOutboundConfig {
        platform: conduit_transformers::anthropic::PlatformType::Direct,
        base_url: String::new(),
        api_key: String::new(),
        endpoint_path: Some("/v1/messages".to_string()),
        project_id: None,
        region: None,
    });
    let gemini = GeminiOutboundTransformer::with_config(
        GeminiOutboundConfig {
            base_url: String::new(),
            api_version: "v1beta".to_string(),
            endpoint_path: String::new(),
            platform_type: GeminiPlatformType::Direct,
        },
        String::new(),
    );
    let responses = OpenAiResponsesOutbound::new("", "")?;

    for (client_name, request) in normalized_client_requests()? {
        assert!(matches!(request.payload, LlmRequestPayload::Chat(_)));

        let mut openai_request = request.clone();
        openai_request.model = Some("upstream-openai-model".to_string());
        openai_request.api_format = ApiFormat::OpenAiChatCompletions;
        let openai_body =
            conduit_transformers::openai_outbound::build_openai_outbound_body(&openai_request)?;
        assert_eq!(
            openai_body["model"], "upstream-openai-model",
            "{client_name}"
        );
        assert!(openai_body["messages"].is_array(), "{client_name}");

        let mut anthropic_request = request.clone();
        anthropic_request.model = Some("upstream-claude-model".to_string());
        anthropic_request.api_format = ApiFormat::AnthropicMessages;
        let anthropic_http = anthropic.outbound_request(&anthropic_request)?;
        let anthropic_body = body_json(&anthropic_http)?;
        assert_eq!(anthropic_http.path, "/v1/messages", "{client_name}");
        assert_eq!(
            anthropic_body["model"], "upstream-claude-model",
            "{client_name}"
        );
        assert!(anthropic_body["messages"].is_array(), "{client_name}");

        let mut gemini_request = request.clone();
        gemini_request.model = Some("upstream-gemini-model".to_string());
        gemini_request.api_format = ApiFormat::GeminiContents;
        let gemini_http = gemini.outbound_request(&gemini_request)?;
        let gemini_body = body_json(&gemini_http)?;
        assert_eq!(
            gemini_http.path, "/v1beta/models/upstream-gemini-model:generateContent",
            "{client_name}"
        );
        assert!(gemini_body["contents"].is_array(), "{client_name}");

        let mut responses_request = request;
        responses_request.model = Some("upstream-responses-model".to_string());
        responses_request.api_format = ApiFormat::OpenAiResponses;
        let responses_http = responses.outbound_request(&responses_request)?;
        let responses_body = body_json(&responses_http)?;
        assert_eq!(responses_http.path, "/v1/responses", "{client_name}");
        assert_eq!(
            responses_body["model"], "upstream-responses-model",
            "{client_name}"
        );
        assert!(
            responses_body["input"].is_string() || responses_body["input"].is_array(),
            "{client_name}"
        );
    }

    Ok(())
}

#[test]
fn streaming_paths_and_flags_follow_the_selected_upstream_protocol() -> TestResult {
    let gemini = GeminiOutboundTransformer::with_config(
        GeminiOutboundConfig {
            base_url: String::new(),
            api_version: "v1beta".to_string(),
            endpoint_path: String::new(),
            platform_type: GeminiPlatformType::Direct,
        },
        String::new(),
    );
    let responses = OpenAiResponsesOutbound::new("", "")?;

    for (client_name, mut request) in normalized_client_requests()? {
        request.stream = true;
        request.model = Some("stream-model".to_string());

        request.api_format = ApiFormat::GeminiContents;
        let gemini_http = gemini.outbound_request(&request)?;
        assert_eq!(
            gemini_http.path, "/v1beta/models/stream-model:streamGenerateContent?alt=sse",
            "{client_name}"
        );
        assert!(gemini_http.skip_inbound_query_merge, "{client_name}");

        request.api_format = ApiFormat::OpenAiResponses;
        let responses_http = responses.outbound_request(&request)?;
        assert_eq!(responses_http.path, "/v1/responses", "{client_name}");
        assert_eq!(body_json(&responses_http)?["stream"], true, "{client_name}");
    }

    Ok(())
}
