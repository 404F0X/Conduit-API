use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Uri};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::AppState;

/// Content types emitted by the Gemini stream writers, mirroring the headers
/// set by Go's `WriteGeminiStream` (JSON array) and `WriteSSEStream` (SSE).
pub const GEMINI_JSON_STREAM_CONTENT_TYPE: &str = "application/json; charset=UTF-8";
pub const GEMINI_SSE_STREAM_CONTENT_TYPE: &str = "text/event-stream";

/// Streaming response format selected for a Gemini `generateContent` /
/// `streamGenerateContent` request.
///
/// Mirrors `GeminiHandlers.GenerateContent` in `internal/server/api/gemini.go`,
/// which switches on the `alt` query parameter:
/// - `alt=sse` -> `WriteSSEStream` (Server-Sent Events, `text/event-stream`)
/// - default   -> `WriteGeminiStream` (JSON array, `application/json; charset=UTF-8`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiStreamFormat {
    /// `WriteGeminiStream`: events joined into a JSON array wrapped in `[` ... `]`.
    JsonArray,
    /// `WriteSSEStream`: events emitted as `event: <type>\ndata: <json>\n\n`.
    Sse,
}

impl GeminiStreamFormat {
    /// HTTP `Content-Type` header value Go sets on the streaming response.
    pub fn content_type(self) -> &'static str {
        match self {
            GeminiStreamFormat::JsonArray => GEMINI_JSON_STREAM_CONTENT_TYPE,
            GeminiStreamFormat::Sse => GEMINI_SSE_STREAM_CONTENT_TYPE,
        }
    }
}

/// Select the streaming format for a Gemini completion request, mirroring
/// `GeminiHandlers.GenerateContent`:
///
/// ```go
/// alt := c.Query("alt")
/// switch alt {
/// case "sse":
///     handlers.ChatCompletionHandlers.WithStreamWriter(WriteSSEStream).ChatCompletion(c)
/// default:
///     handlers.ChatCompletionHandlers.WithStreamWriter(WriteGeminiStream).ChatCompletion(c)
/// }
/// ```
///
/// `alt=sse` selects SSE; any other value (including absent, `alt=json`,
/// or malformed) selects the default JSON-array writer.
pub fn select_gemini_stream_format(alt: GeminiAlt) -> GeminiStreamFormat {
    match alt {
        GeminiAlt::Sse => GeminiStreamFormat::Sse,
        GeminiAlt::Json => GeminiStreamFormat::JsonArray,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiRouteBase {
    VersionRoot,
    GeminiPrefix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiAlt {
    Json,
    Sse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiRouteParts {
    pub base: GeminiRouteBase,
    pub api_version: String,
    pub model_path: String,
    pub action: String,
    pub alt: GeminiAlt,
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiModelListRouteParts {
    pub base: GeminiRouteBase,
    pub api_version: String,
    pub alt: GeminiAlt,
    pub key: Option<String>,
    pub page_size: Option<u32>,
    pub page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiModelSummary {
    pub id: String,
    pub version: Option<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub input_token_limit: Option<u32>,
    pub output_token_limit: Option<u32>,
    pub supported_generation_methods: Vec<String>,
}

impl GeminiModelSummary {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: None,
            display_name: None,
            description: None,
            input_token_limit: None,
            output_token_limit: None,
            supported_generation_methods: Vec::new(),
        }
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_token_limits(mut self, input_token_limit: u32, output_token_limit: u32) -> Self {
        self.input_token_limit = Some(input_token_limit);
        self.output_token_limit = Some(output_token_limit);
        self
    }

    pub fn with_supported_generation_methods(
        mut self,
        methods: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.supported_generation_methods = methods.into_iter().map(Into::into).collect();
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiModelListResponse {
    pub models: Vec<GeminiModelObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GeminiModelObject {
    pub name: String,
    pub base_model_id: String,
    pub version: String,
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub supported_generation_methods: Vec<String>,
}

pub async fn list_models(State(state): State<AppState>) -> Response {
    let Some(service) = state.services().model_service() else {
        return gemini_model_error("model service is not available");
    };
    match service.list_enabled_models().await {
        Ok(models) => {
            let summaries = models.into_iter().enumerate().map(|(index, model)| {
                let display_name = model.name.unwrap_or_else(|| model.id.clone());
                GeminiModelSummary::new(model.id.clone())
                    .with_version(format!("{}-{index}", model.id))
                    .with_display_name(display_name.clone())
                    .with_description(display_name)
                    .with_supported_generation_methods(["generateContent", "streamGenerateContent"])
            });
            Json(gemini_model_list_response(summaries)).into_response()
        }
        Err(err) => gemini_model_error(&err.message),
    }
}

fn gemini_model_error(message: &str) -> Response {
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": {
                "message": message,
                "code": 500,
                "status": "internal_server_error"
            }
        })),
    )
        .into_response()
}

/// POST /gemini/:version/models/*action and POST /v1beta/models/*action
/// Go: `GeminiHandlers.GenerateContent` (gemini.go:66-74). Routes through
/// the shared orchestrator pipeline with a Gemini inbound transformer.
pub async fn generate_content(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<crate::middleware::api_key_auth::ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    crate::openai_handlers::dispatch_openai(
        &state,
        crate::openai_handlers::OpenAiRoute::GeminiGenerateContent,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

pub fn parse_gemini_route_parts(request_target: &str) -> Option<GeminiRouteParts> {
    let (path, query) = split_request_target(request_target);
    let query_parts = GeminiQueryParts::parse(query);

    parse_gemini_path(path).map(|(base, api_version, model_path, action)| GeminiRouteParts {
        base,
        api_version: api_version.to_owned(),
        model_path: model_path.to_owned(),
        action: action.to_owned(),
        alt: query_parts.alt,
        key: query_parts.key,
    })
}

pub fn parse_gemini_model_list_route_parts(
    request_target: &str,
) -> Option<GeminiModelListRouteParts> {
    let (path, query) = split_request_target(request_target);
    let query_parts = GeminiQueryParts::parse(query);

    parse_gemini_model_list_path(path).map(|(base, api_version)| GeminiModelListRouteParts {
        base,
        api_version,
        alt: query_parts.alt,
        key: query_parts.key,
        page_size: query_parts.page_size,
        page_token: query_parts.page_token,
    })
}

pub fn gemini_model_list_response(
    models: impl IntoIterator<Item = GeminiModelSummary>,
) -> GeminiModelListResponse {
    GeminiModelListResponse {
        models: models
            .into_iter()
            .enumerate()
            .map(|(index, model)| {
                let base_model_id = gemini_base_model_id(&model.id);
                let display_name = model.display_name.unwrap_or_else(|| base_model_id.clone());
                let description = model.description.unwrap_or_else(|| display_name.clone());
                let supported_generation_methods = if model.supported_generation_methods.is_empty()
                {
                    vec![
                        "generateContent".to_owned(),
                        "streamGenerateContent".to_owned(),
                    ]
                } else {
                    model.supported_generation_methods
                };

                GeminiModelObject {
                    name: gemini_model_resource_name(&model.id),
                    base_model_id: base_model_id.clone(),
                    version: model
                        .version
                        .unwrap_or_else(|| format!("{base_model_id}-{index}")),
                    display_name,
                    description,
                    supported_generation_methods,
                }
            })
            .collect(),
        next_page_token: None,
    }
}

pub fn parse_gemini_model_list_response(
    body: &[u8],
) -> serde_json::Result<GeminiModelListResponse> {
    serde_json::from_slice(body)
}

fn parse_gemini_path(path: &str) -> Option<(GeminiRouteBase, String, String, String)> {
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let (base, version_index, models_index) = match segments.as_slice() {
        ["v1beta", "models", ..] => (GeminiRouteBase::VersionRoot, 0, 1),
        ["gemini", version, "models", ..] if !version.is_empty() => {
            (GeminiRouteBase::GeminiPrefix, 1, 2)
        }
        _ => return None,
    };

    let api_version = segments[version_index];
    let model_and_action = segments.get(models_index + 1..)?.join("/");
    let (model_path, action) = model_and_action.rsplit_once(':')?;

    if model_path.is_empty() || action.is_empty() {
        return None;
    }

    Some((
        base,
        api_version.to_owned(),
        model_path.to_owned(),
        action.to_owned(),
    ))
}

fn parse_gemini_model_list_path(path: &str) -> Option<(GeminiRouteBase, String)> {
    match path
        .trim_start_matches('/')
        .split('/')
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["v1beta", "models"] => Some((GeminiRouteBase::VersionRoot, "v1beta".to_owned())),
        ["gemini", version, "models"] if !version.is_empty() => {
            Some((GeminiRouteBase::GeminiPrefix, (*version).to_owned()))
        }
        _ => None,
    }
}

fn gemini_model_resource_name(model_id: &str) -> String {
    let model_id = model_id.trim_start_matches('/');
    if model_id.starts_with("models/") || model_id.starts_with("publishers/") {
        model_id.to_owned()
    } else {
        format!("models/{model_id}")
    }
}

fn gemini_base_model_id(model_id: &str) -> String {
    let model_id = model_id.trim_start_matches('/');
    model_id
        .strip_prefix("models/")
        .unwrap_or(model_id)
        .to_owned()
}

fn split_request_target(request_target: &str) -> (&str, Option<&str>) {
    request_target
        .split_once('?')
        .map_or((request_target, None), |(path, query)| (path, Some(query)))
}

struct GeminiQueryParts {
    alt: GeminiAlt,
    key: Option<String>,
    page_size: Option<u32>,
    page_token: Option<String>,
}

impl GeminiQueryParts {
    fn parse(query: Option<&str>) -> Self {
        let mut alt = GeminiAlt::Json;
        let mut key = None;
        let mut page_size = None;
        let mut page_token = None;

        for (query_key, value) in query
            .into_iter()
            .flat_map(|query| query.split('&'))
            .filter_map(|pair| pair.split_once('='))
        {
            match query_key {
                "alt" if value == "sse" => alt = GeminiAlt::Sse,
                "alt" => alt = GeminiAlt::Json,
                "key" if !value.is_empty() && key.is_none() => key = Some(value.to_owned()),
                "pageSize" if page_size.is_none() => page_size = value.parse::<u32>().ok(),
                "pageToken" if !value.is_empty() && page_token.is_none() => {
                    page_token = Some(value.to_owned());
                }
                _ => {}
            }
        }

        Self {
            alt,
            key,
            page_size,
            page_token,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use serde_json::json;

    use super::*;

    #[test]
    fn parses_v1beta_model_action_route() {
        let Some(parts) =
            parse_gemini_route_parts("/v1beta/models/gemini-1.5-flash:generateContent")
        else {
            panic!("route should parse");
        };

        assert_eq!(parts.base, GeminiRouteBase::VersionRoot);
        assert_eq!(parts.api_version, "v1beta");
        assert_eq!(parts.model_path, "gemini-1.5-flash");
        assert_eq!(parts.action, "generateContent");
        assert_eq!(parts.alt, GeminiAlt::Json);
        assert_eq!(parts.key, None);
    }

    #[test]
    fn parses_gemini_prefixed_model_action_route() {
        let Some(parts) = parse_gemini_route_parts(
            "/gemini/v1/models/publishers/google/models/gemini-pro:countTokens",
        ) else {
            panic!("route should parse");
        };

        assert_eq!(parts.base, GeminiRouteBase::GeminiPrefix);
        assert_eq!(parts.api_version, "v1");
        assert_eq!(parts.model_path, "publishers/google/models/gemini-pro");
        assert_eq!(parts.action, "countTokens");
    }

    #[test]
    fn parses_sse_alt_and_query_key() {
        let Some(parts) = parse_gemini_route_parts(
            "/v1beta/models/gemini-1.5-flash:streamGenerateContent?alt=sse&key=test-key",
        ) else {
            panic!("route should parse");
        };

        assert_eq!(parts.alt, GeminiAlt::Sse);
        assert_eq!(parts.key.as_deref(), Some("test-key"));
    }

    #[test]
    fn keeps_v1beta_and_gemini_prefixed_routes_separate() {
        let Some(direct) = parse_gemini_route_parts("/v1beta/models/model-a:generateContent")
        else {
            panic!("direct route should parse");
        };
        let Some(prefixed) =
            parse_gemini_route_parts("/gemini/v1beta/models/model-a:generateContent")
        else {
            panic!("prefixed route should parse");
        };

        assert_eq!(direct.base, GeminiRouteBase::VersionRoot);
        assert_eq!(prefixed.base, GeminiRouteBase::GeminiPrefix);
        assert_eq!(direct.api_version, prefixed.api_version);
        assert_eq!(direct.model_path, prefixed.model_path);
        assert_eq!(direct.action, prefixed.action);
    }

    #[test]
    fn rejects_non_gemini_or_incomplete_routes() {
        for path in [
            "/v1/models",
            "/gemini/models/gemini-pro:generateContent",
            "/v1beta/models/",
            "/v1beta/models/gemini-pro",
            "/v1beta/models/:generateContent",
            "/v1beta/models/gemini-pro:",
        ] {
            assert_eq!(parse_gemini_route_parts(path), None, "{path}");
        }
    }

    #[test]
    fn parses_v1beta_model_list_route_with_pagination_query() {
        let Some(parts) = parse_gemini_model_list_route_parts(
            "/v1beta/models?pageSize=25&pageToken=next-page&key=test-key",
        ) else {
            panic!("route should parse");
        };

        assert_eq!(parts.base, GeminiRouteBase::VersionRoot);
        assert_eq!(parts.api_version, "v1beta");
        assert_eq!(parts.alt, GeminiAlt::Json);
        assert_eq!(parts.key.as_deref(), Some("test-key"));
        assert_eq!(parts.page_size, Some(25));
        assert_eq!(parts.page_token.as_deref(), Some("next-page"));
    }

    #[test]
    fn parses_gemini_prefixed_model_list_route() {
        let Some(parts) = parse_gemini_model_list_route_parts("/gemini/v1/models?alt=sse") else {
            panic!("route should parse");
        };

        assert_eq!(parts.base, GeminiRouteBase::GeminiPrefix);
        assert_eq!(parts.api_version, "v1");
        assert_eq!(parts.alt, GeminiAlt::Sse);
        assert_eq!(parts.page_size, None);
        assert_eq!(parts.page_token, None);
    }

    #[test]
    fn rejects_non_list_model_routes_for_model_list_parser() {
        for path in [
            "/v1/models",
            "/gemini/models",
            "/v1beta/models/gemini-pro",
            "/v1beta/models/gemini-pro:generateContent",
            "/gemini/v1/models/gemini-pro",
        ] {
            assert_eq!(parse_gemini_model_list_route_parts(path), None, "{path}");
        }
    }

    #[test]
    fn model_list_response_serializes_empty_gemini_shape() -> Result<(), Box<dyn Error>> {
        let response = gemini_model_list_response(Vec::new());
        let body = serde_json::to_value(response)?;

        assert_eq!(body, json!({ "models": [] }));
        Ok(())
    }

    #[test]
    fn model_list_response_serializes_gemini_model_shape() -> Result<(), Box<dyn Error>> {
        let response = gemini_model_list_response([GeminiModelSummary::new("gemini-1.5-flash")
            .with_display_name("Gemini 1.5 Flash")
            .with_description("Fast multimodal model")]);
        let body = serde_json::to_value(response)?;

        assert_eq!(
            body,
            json!({
                "models": [
                    {
                        "name": "models/gemini-1.5-flash",
                        "baseModelId": "gemini-1.5-flash",
                        "version": "gemini-1.5-flash-0",
                        "displayName": "Gemini 1.5 Flash",
                        "description": "Fast multimodal model",
                        "supportedGenerationMethods": ["generateContent", "streamGenerateContent"]
                    }
                ]
            })
        );
        Ok(())
    }

    #[test]
    fn model_list_response_preserves_prefixed_resource_names() -> Result<(), Box<dyn Error>> {
        let response = gemini_model_list_response([
            GeminiModelSummary::new("models/gemini-pro"),
            GeminiModelSummary::new("publishers/google/models/gemini-pro"),
        ]);
        let body = serde_json::to_value(response)?;

        assert_eq!(body["models"][0]["name"], "models/gemini-pro");
        assert_eq!(body["models"][0]["baseModelId"], "gemini-pro");
        assert_eq!(
            body["models"][1]["name"],
            "publishers/google/models/gemini-pro"
        );
        assert_eq!(
            body["models"][1]["baseModelId"],
            "publishers/google/models/gemini-pro"
        );
        Ok(())
    }

    #[test]
    fn parses_gemini_model_list_response_shape() -> Result<(), Box<dyn Error>> {
        let response = parse_gemini_model_list_response(
            br#"{
                "models": [
                    {
                        "name": "models/gemini-1.5-pro",
                        "baseModelId": "gemini-1.5-pro",
                        "version": "gemini-1.5-pro-0",
                        "displayName": "Gemini 1.5 Pro",
                        "description": "Gemini 1.5 Pro",
                        "supportedGenerationMethods": ["generateContent"]
                    }
                ],
                "nextPageToken": "page-2"
            }"#,
        )?;

        assert_eq!(response.next_page_token.as_deref(), Some("page-2"));
        assert_eq!(response.models[0].name, "models/gemini-1.5-pro");
        assert_eq!(response.models[0].base_model_id, "gemini-1.5-pro");
        assert_eq!(
            response.models[0].supported_generation_methods,
            ["generateContent"]
        );
        Ok(())
    }

    /// Mirrors `GeminiHandlers.GenerateContent` switch on `c.Query("alt")`:
    /// only the literal `sse` selects SSE; default falls through to
    /// `WriteGeminiStream` (JSON array).
    #[test]
    fn stream_format_selects_sse_only_for_sse_alt() {
        assert_eq!(
            select_gemini_stream_format(GeminiAlt::Sse),
            GeminiStreamFormat::Sse
        );
        assert_eq!(
            select_gemini_stream_format(GeminiAlt::Json),
            GeminiStreamFormat::JsonArray
        );
    }

    /// Mirrors the headers set by Go `WriteGeminiStream` and `WriteSSEStream`.
    #[test]
    fn stream_format_content_type_matches_go_writers() {
        assert_eq!(
            GeminiStreamFormat::JsonArray.content_type(),
            "application/json; charset=UTF-8"
        );
        assert_eq!(GeminiStreamFormat::Sse.content_type(), "text/event-stream");
    }

    /// End-to-end: parsing `?alt=sse` from a real Gemini route and then
    /// selecting the stream format mirrors `GenerateContent`'s branching.
    #[test]
    fn parsed_route_alt_drives_stream_format_selection() {
        let sse_route = match parse_gemini_route_parts(
            "/v1beta/models/gemini-1.5-flash:streamGenerateContent?alt=sse",
        ) {
            Some(parts) => parts,
            None => panic!("SSE route should parse"),
        };
        assert_eq!(
            select_gemini_stream_format(sse_route.alt),
            GeminiStreamFormat::Sse
        );

        let json_route =
            match parse_gemini_route_parts("/v1beta/models/gemini-1.5-flash:streamGenerateContent")
            {
                Some(parts) => parts,
                None => panic!("JSON route should parse"),
            };
        assert_eq!(
            select_gemini_stream_format(json_route.alt),
            GeminiStreamFormat::JsonArray
        );
    }
}
