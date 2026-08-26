use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::AppState;

/// HTTP status code Go assigns to `transformer.ErrInvalidModel`
/// (`llm/transformer/anthropic/inbound.go:163-171`). Kept as a named constant so
/// the parity mapping is auditable in one place.
pub const ANTHROPIC_INVALID_MODEL_STATUS: u16 = 422;

pub const ANTHROPIC_VERSION_HEADER: &str = "anthropic-version";
pub const ANTHROPIC_BETA_HEADER: &str = "anthropic-beta";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicModelSummary {
    pub id: String,
    pub display_name: String,
    pub created: String,
}

impl AnthropicModelSummary {
    pub fn new(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            created: String::new(),
        }
    }

    pub fn with_created(mut self, created: impl Into<String>) -> Self {
        self.created = created.into();
        self
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AnthropicModelListResponse {
    pub object: &'static str,
    pub data: Vec<AnthropicModelObject>,
    pub has_more: bool,
    pub first_id: String,
    pub last_id: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AnthropicModelObject {
    pub id: String,
    #[serde(rename = "type")]
    pub object_type: &'static str,
    pub display_name: String,
    pub created: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicRouteBase {
    OpenAiCompatible,
    AnthropicPrefix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicRouteKind {
    Messages,
    CountTokens,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicRouteParts {
    pub base: AnthropicRouteBase,
    pub api_version: String,
    pub kind: AnthropicRouteKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicHeaderCompatibility {
    pub version: Option<String>,
    pub betas: Vec<String>,
}

/// Anthropic error envelope, mirroring Go `anthropic.AnthropicError`
/// (`llm/transformer/anthropic/model.go:551-557`):
///
/// ```json
/// { "type": "<envelope>", "request_id": "<id>", "error": { "type": "<t>", "message": "<m>" } }
/// ```
///
/// `status_code` is Go's `json:"-"` field (HTTP-only, never serialized) and is
/// kept here so callers building the wire response can read it without a second
/// lookup. It is excluded from JSON by `#[serde(skip)]`.
#[derive(Debug, Clone, Serialize)]
pub struct AnthropicErrorEnvelope {
    /// Envelope discriminator. Go always emits `"error"` (set by every branch
    /// of `InboundTransformer.TransformError`). `#[serde(skip_serializing_if)]`
    /// preserves Go's `omitempty` on the struct field.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub envelope_type: Option<&'static str>,
    /// `request_id` (Go field, always emitted even when empty).
    pub request_id: String,
    /// HTTP status code. Go marks it `json:"-"`; we mark `#[serde(skip)]` for
    /// the same effect.
    #[serde(skip)]
    pub status_code: u16,
    pub error: AnthropicErrorBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnthropicErrorBody {
    /// Inner `error.type`. Always present in Go output.
    #[serde(rename = "type")]
    pub error_type: &'static str,
    /// Inner `error.message`. Go uses `string` (non-pointer), so we use owned
    /// `String` to allow dynamic messages (e.g. provider error text).
    pub message: String,
}

pub async fn list_models(State(state): State<AppState>) -> Response {
    let Some(service) = state.services().model_service() else {
        return anthropic_error_response(
            AnthropicErrorKind::InternalFallback,
            "model service is not available",
            "",
        );
    };
    match service.list_enabled_models().await {
        Ok(models) => {
            let summaries = models.into_iter().map(|model| AnthropicModelSummary {
                display_name: model.name.unwrap_or_else(|| model.id.clone()),
                created: chrono::DateTime::from_timestamp(model.created, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
                id: model.id,
            });
            Json(anthropic_model_list_response(summaries)).into_response()
        }
        Err(err) => anthropic_error_response(AnthropicErrorKind::InternalFallback, err.message, ""),
    }
}

pub async fn create_message(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<crate::middleware::api_key_auth::ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let _compat = anthropic_header_compatibility(&headers);

    // Anthropic Messages API → same orchestrator pipeline as OpenAI, selected
    // by the AnthropicMessages route variant (bridge picks the Anthropic
    // inbound transformer). Go: anthropic.go:67-78 → ChatCompletion.
    crate::openai_handlers::dispatch_openai(
        &state,
        crate::openai_handlers::OpenAiRoute::AnthropicMessages,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

pub async fn count_message_tokens(
    State(state): State<AppState>,
    api_key_meta: Option<axum::Extension<crate::middleware::api_key_auth::ValidatedApiKeyMetadata>>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let _compat = anthropic_header_compatibility(&headers);
    crate::openai_handlers::dispatch_openai(
        &state,
        crate::openai_handlers::OpenAiRoute::AnthropicCountTokens,
        &uri,
        method,
        &headers,
        body,
        api_key_meta.as_ref().map(|ext| &ext.0),
    )
    .await
}

/// Build a Go-parity `AnthropicError` envelope from a discrete
/// (status, envelope, type, message, request_id) tuple. Pure function — no
/// `axum`/`StatusCode` dependency — so unit tests can assert the exact JSON
/// shape and the four Go `TransformError` priority branches
/// (`llm/transformer/anthropic/inbound.go:151-219`).
///
/// Branch priority mirrors Go:
/// 1. `ErrInvalidModel` -> 422 `invalid_model_error`
/// 2. `ErrInvalidRequest` -> 400 `invalid_request_error`
/// 3. fallback -> 500 `internal_server_error`
///
/// The envelope `type` field is always `"error"` per Go
/// `InboundTransformer.TransformError` (every branch sets `Type: "error"` or a
/// provider-derived type that we forward verbatim).
pub fn build_anthropic_error_envelope(
    status_code: u16,
    envelope_type: &'static str,
    error_type: &'static str,
    message: impl Into<String>,
    request_id: impl Into<String>,
) -> AnthropicErrorEnvelope {
    AnthropicErrorEnvelope {
        envelope_type: Some(envelope_type),
        request_id: request_id.into(),
        status_code,
        error: AnthropicErrorBody {
            error_type,
            message: message.into(),
        },
    }
}

/// Map an inbound-validation outcome onto the Go `TransformError` priority
/// table. Returns the (status, error_type) tuple Go would emit for each error
/// class, so the handler layer can build the envelope without re-implementing
/// the branching.
///
/// Mirrors Go `llm/transformer/anthropic/inbound.go:151-219`. The recognized
/// kinds cover every Go branch that produces a distinct
/// (StatusCode, Error.Type) pair; unknown kinds fall through to the 500
/// `internal_server_error` fallback just like Go.
pub fn anthropic_error_classification(
    kind: AnthropicErrorKind,
) -> (u16, &'static str, &'static str) {
    // Returns (status_code, envelope_type, error_type).
    match kind {
        // Go inbound.go:193-206 (ErrInvalidRequest).
        AnthropicErrorKind::InvalidRequest => (400, "error", "invalid_request_error"),
        // Go inbound.go:163-171 (ErrInvalidModel).
        AnthropicErrorKind::InvalidModel => (
            ANTHROPIC_INVALID_MODEL_STATUS,
            "error",
            "invalid_model_error",
        ),
        // Go inbound.go:152-161 (nil err fallback inside TransformError).
        AnthropicErrorKind::InternalUnspecified => (500, "error", "internal_server_error"),
        // Go inbound.go:208-219 (final fallback).
        AnthropicErrorKind::InternalFallback => (500, "error", "internal_server_error"),
    }
}

/// Discrete inbound error classes recognized by Go's
/// `InboundTransformer.TransformError`. Keeping them as an enum (instead of
/// reusing `conduit_core::ConduitError`) lets the HTTP layer assert the exact
/// Go-priority mapping without depending on transformer internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicErrorKind {
    /// Maps to Go `transformer.ErrInvalidRequest` -> 400 `invalid_request_error`.
    InvalidRequest,
    /// Maps to Go `transformer.ErrInvalidModel` -> 422 `invalid_model_error`.
    InvalidModel,
    /// Maps to Go's nil-error fallback -> 500 `internal_server_error`.
    InternalUnspecified,
    /// Maps to Go's final fallback (unknown error) -> 500 `internal_server_error`.
    InternalFallback,
}

pub fn anthropic_model_list_response(
    models: impl IntoIterator<Item = AnthropicModelSummary>,
) -> AnthropicModelListResponse {
    let data = models
        .into_iter()
        .map(|model| AnthropicModelObject {
            id: model.id,
            object_type: "model",
            display_name: model.display_name,
            created: model.created,
        })
        .collect::<Vec<_>>();

    AnthropicModelListResponse {
        object: "list",
        first_id: data
            .first()
            .map_or_else(String::new, |model| model.id.clone()),
        last_id: data
            .last()
            .map_or_else(String::new, |model| model.id.clone()),
        data,
        has_more: false,
    }
}

pub fn parse_anthropic_route_parts(request_target: &str) -> Option<AnthropicRouteParts> {
    let path = request_path_without_query(request_target);
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();

    let (base, api_version, kind) = match segments.as_slice() {
        ["v1", "messages"] => (
            AnthropicRouteBase::OpenAiCompatible,
            "v1",
            AnthropicRouteKind::Messages,
        ),
        ["v1", "messages", "count_tokens"] => (
            AnthropicRouteBase::OpenAiCompatible,
            "v1",
            AnthropicRouteKind::CountTokens,
        ),
        ["anthropic", version, "messages"] if !version.is_empty() => (
            AnthropicRouteBase::AnthropicPrefix,
            *version,
            AnthropicRouteKind::Messages,
        ),
        ["anthropic", version, "messages", "count_tokens"] if !version.is_empty() => (
            AnthropicRouteBase::AnthropicPrefix,
            *version,
            AnthropicRouteKind::CountTokens,
        ),
        _ => return None,
    };

    Some(AnthropicRouteParts {
        base,
        api_version: api_version.to_owned(),
        kind,
    })
}

pub fn anthropic_header_compatibility(headers: &HeaderMap) -> AnthropicHeaderCompatibility {
    let version = headers
        .get(ANTHROPIC_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let betas = headers
        .get_all(ANTHROPIC_BETA_HEADER)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    AnthropicHeaderCompatibility { version, betas }
}

fn request_path_without_query(request_path: &str) -> &str {
    request_path
        .split_once('?')
        .map_or(request_path, |(path, _)| path)
}

/// Render a Go-parity `AnthropicError` envelope as an `axum::Response`. The
/// status code is read from `envelope.status_code` (Go's `json:"-"` field),
/// which mirrors how Go writes the HTTP status separately from the JSON body.
pub fn anthropic_error_response_from_envelope(envelope: AnthropicErrorEnvelope) -> Response {
    let status =
        StatusCode::from_u16(envelope.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(envelope)).into_response()
}

/// Convenience: classify `kind`, build the envelope, and render the response.
/// Equivalent to calling `anthropic_error_classification` +
/// `build_anthropic_error_envelope` + `anthropic_error_response_from_envelope`.
pub fn anthropic_error_response(
    kind: AnthropicErrorKind,
    message: impl Into<String>,
    request_id: impl Into<String>,
) -> Response {
    let (status, envelope_type, error_type) = anthropic_error_classification(kind);
    let envelope =
        build_anthropic_error_envelope(status, envelope_type, error_type, message, request_id);
    anthropic_error_response_from_envelope(envelope)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use axum::http::HeaderValue;
    use serde_json::{Value, json};

    use super::*;

    #[test]
    fn model_list_response_serializes_empty_list_shape() -> Result<(), Box<dyn Error>> {
        let response = anthropic_model_list_response(Vec::new());
        let body = serde_json::to_value(response)?;

        assert_eq!(
            body,
            json!({
                "object": "list",
                "data": [],
                "has_more": false,
                "first_id": "",
                "last_id": ""
            })
        );
        Ok(())
    }

    #[test]
    fn model_list_response_sets_first_and_last_ids() {
        let response = anthropic_model_list_response([
            AnthropicModelSummary::new("claude-3-5-haiku-20241022", "Claude 3.5 Haiku"),
            AnthropicModelSummary::new("claude-sonnet-4-20250514", "Claude Sonnet 4"),
        ]);

        assert_eq!(response.first_id, "claude-3-5-haiku-20241022");
        assert_eq!(response.last_id, "claude-sonnet-4-20250514");
        assert!(!response.has_more);
    }

    #[test]
    fn model_list_response_serializes_model_json_shape() -> Result<(), Box<dyn Error>> {
        let response = anthropic_model_list_response([AnthropicModelSummary::new(
            "claude-opus-4-20250514",
            "Claude Opus 4",
        )
        .with_created("2025-05-14T00:00:00Z")]);
        let body = serde_json::to_value(response)?;

        assert_eq!(
            body,
            json!({
                "object": "list",
                "data": [
                    {
                        "id": "claude-opus-4-20250514",
                        "type": "model",
                        "display_name": "Claude Opus 4",
                        "created": "2025-05-14T00:00:00Z"
                    }
                ],
                "has_more": false,
                "first_id": "claude-opus-4-20250514",
                "last_id": "claude-opus-4-20250514"
            })
        );
        Ok(())
    }

    #[test]
    fn route_parts_parse_messages_and_count_tokens_dual_entrypoints() {
        let Some(native) = parse_anthropic_route_parts("/anthropic/v1/messages?ignored=true")
        else {
            panic!("native Anthropic messages route should parse");
        };
        let Some(compatible) = parse_anthropic_route_parts("/v1/messages/count_tokens") else {
            panic!("OpenAI-compatible Anthropic count_tokens route should parse");
        };

        assert_eq!(native.base, AnthropicRouteBase::AnthropicPrefix);
        assert_eq!(native.api_version, "v1");
        assert_eq!(native.kind, AnthropicRouteKind::Messages);
        assert_eq!(compatible.base, AnthropicRouteBase::OpenAiCompatible);
        assert_eq!(compatible.api_version, "v1");
        assert_eq!(compatible.kind, AnthropicRouteKind::CountTokens);
    }

    #[test]
    fn route_parts_reject_unknown_or_incomplete_paths() {
        for path in [
            "/anthropic/messages",
            "/anthropic/v1/messages/",
            "/anthropic/v1/messages/count_tokens/extra",
            "/v1/messages/",
            "/v1/messages/count-tokens",
            "/v1/chat/completions",
        ] {
            assert_eq!(parse_anthropic_route_parts(path), None, "{path}");
        }
    }

    #[test]
    fn header_compatibility_extracts_version_and_beta_values() {
        let mut headers = HeaderMap::new();
        headers.insert(
            ANTHROPIC_VERSION_HEADER,
            HeaderValue::from_static(" 2023-06-01 "),
        );
        headers.append(
            ANTHROPIC_BETA_HEADER,
            HeaderValue::from_static("messages-2023-12-15, tools-2024-04-04"),
        );
        headers.append(
            ANTHROPIC_BETA_HEADER,
            HeaderValue::from_static("prompt-caching-2024-07-31"),
        );

        let compatibility = anthropic_header_compatibility(&headers);

        assert_eq!(compatibility.version.as_deref(), Some("2023-06-01"));
        assert_eq!(
            compatibility.betas,
            [
                "messages-2023-12-15",
                "tools-2024-04-04",
                "prompt-caching-2024-07-31"
            ]
        );
    }

    #[test]
    fn header_compatibility_ignores_empty_or_invalid_values() -> Result<(), Box<dyn Error>> {
        let mut headers = HeaderMap::new();
        headers.insert(ANTHROPIC_VERSION_HEADER, HeaderValue::from_static(" "));
        headers.insert(
            ANTHROPIC_BETA_HEADER,
            HeaderValue::from_static(" ,, beta-a, "),
        );
        headers.append(
            ANTHROPIC_BETA_HEADER,
            HeaderValue::from_bytes(b"beta-b\xFF")?,
        );

        let compatibility = anthropic_header_compatibility(&headers);

        assert_eq!(compatibility.version, None);
        assert_eq!(compatibility.betas, ["beta-a"]);
        Ok(())
    }

    // ---- Anthropic error envelope (Go parity) ------------------------------
    //
    // These mirror Go's `InboundTransformer.TransformError`
    // (`llm/transformer/anthropic/inbound.go:151-219`) and the wire shape of
    // `anthropic.AnthropicError` (`llm/transformer/anthropic/model.go:551-557`).
    // Each golden case asserts the exact JSON Go emits for one priority branch.

    #[test]
    fn envelope_serializes_go_anthropic_error_shape() -> Result<(), Box<dyn Error>> {
        // Mirrors Go AnthropicError JSON: { type, request_id, error: { type, message } }.
        let envelope = build_anthropic_error_envelope(
            400,
            "error",
            "invalid_request_error",
            "model is required",
            "req_abc123",
        );
        let body = serde_json::to_value(&envelope)?;

        assert_eq!(
            body,
            json!({
                "type": "error",
                "request_id": "req_abc123",
                "error": {
                    "type": "invalid_request_error",
                    "message": "model is required"
                }
            })
        );
        // status_code is json:"-" in Go; serde(skip) must keep it out of the body.
        assert_eq!(envelope.status_code, 400);
        assert!(
            !body
                .as_object()
                .and_then(|o| o.get("status_code"))
                .is_some(),
            "status_code must not appear in JSON"
        );
        Ok(())
    }

    #[test]
    fn envelope_request_id_serializes_empty_string_for_unwired_context() {
        // Go's TransformError passes `RequestID: ""` when no request context is
        // wired (inbound.go:152-219 every branch). The field is *not* omitempty
        // in Go, so it must still be present in JSON as "".
        let envelope = build_anthropic_error_envelope(
            500,
            "error",
            "internal_server_error",
            "internal server error",
            "",
        );
        let body = serde_json::to_value(&envelope).unwrap_or(Value::Null);

        assert_eq!(body["request_id"], json!(""));
    }

    #[test]
    fn classification_maps_each_go_transform_error_branch() {
        // Go inbound.go:193-206.
        let (status, env, err) = anthropic_error_classification(AnthropicErrorKind::InvalidRequest);
        assert_eq!((status, env, err), (400, "error", "invalid_request_error"));

        // Go inbound.go:163-171.
        let (status, env, err) = anthropic_error_classification(AnthropicErrorKind::InvalidModel);
        assert_eq!(
            (status, env, err),
            (
                ANTHROPIC_INVALID_MODEL_STATUS,
                "error",
                "invalid_model_error"
            )
        );
        assert_eq!(ANTHROPIC_INVALID_MODEL_STATUS, 422);

        // Go inbound.go:152-161 (nil err fallback).
        let (status, env, err) =
            anthropic_error_classification(AnthropicErrorKind::InternalUnspecified);
        assert_eq!((status, env, err), (500, "error", "internal_server_error"));

        // Go inbound.go:208-219 (final fallback).
        let (status, env, err) =
            anthropic_error_classification(AnthropicErrorKind::InternalFallback);
        assert_eq!((status, env, err), (500, "error", "internal_server_error"));
    }

    #[tokio::test]
    async fn error_response_renders_status_and_body_from_classification()
    -> Result<(), Box<dyn Error>> {
        // End-to-end: classification -> envelope -> axum Response.
        let response = anthropic_error_response(
            AnthropicErrorKind::InvalidModel,
            "model claude-foo not found",
            "req_xyz",
        );
        assert_eq!(response.status(), StatusCode::from_u16(422)?);

        let body_bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
        let body: Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(
            body,
            json!({
                "type": "error",
                "request_id": "req_xyz",
                "error": {
                    "type": "invalid_model_error",
                    "message": "model claude-foo not found"
                }
            })
        );
        Ok(())
    }

    // Note: the `not_implemented_response_keeps_go_envelope_frame` test was
    // removed because `create_message` now dispatches through the real
    // orchestrator pipeline (via `dispatch_openai`) instead of returning a
    // static 501 stub. The old test asserted the stub's NOT_IMPLEMENTED
    // response shape; that behavior no longer exists.
}
