use std::any::Any;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use conduit_core::{
    ConduitError, admin_error_json, anthropic_error_json, gemini_error_json, openai_error_json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which wire envelope an error response is rendered in.
///
/// Go picks this per request by delegating to the route's **inbound
/// transformer**: `handlers.<X>Orchestrator.Inbound.TransformError(ctx, err)`
/// (`api/chat.go:55`, `api/doubao.go:73`, `api/openai.go:318/325/389`,
/// `api/upstream_error_policy.go:23`). So an Anthropic client hitting
/// `/v1/messages` gets Anthropic's native envelope, and a Gemini client gets
/// Google's — not OpenAI's.
///
/// The Rust HTTP layer previously had only the admin + OpenAI variants and
/// hardcoded `OpenAiCompatibleJson` on every LLM error path, so Claude/Gemini
/// clients received OpenAI-shaped errors their SDKs cannot parse. The envelope
/// builders live in `conduit-core` (shared with `conduit-transformers`, whose
/// `inbound_error` renders the same shapes) so this layer needs no transformer
/// dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorResponseFormat {
    AdminJson,
    OpenAiCompatibleJson,
    /// Anthropic Messages native envelope — `{type, error:{message,type}, request_id}`.
    AnthropicJson,
    /// Gemini native envelope — `{error:{code,message,status}}`.
    GeminiJson,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorFallbackContext {
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
}

pub fn conduit_error_response(err: &ConduitError, format: ErrorResponseFormat) -> Response {
    let status = StatusCode::from_u16(err.http_status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = match format {
        ErrorResponseFormat::AdminJson => admin_error_json(err),
        ErrorResponseFormat::OpenAiCompatibleJson => openai_error_json(err),
        ErrorResponseFormat::AnthropicJson => anthropic_error_json(err),
        ErrorResponseFormat::GeminiJson => gemini_error_json(err),
    };

    (status, Json(body)).into_response()
}

pub fn fallback_rejection_error(
    rejection: impl ToString,
    context: ErrorFallbackContext,
) -> ConduitError {
    internal_fallback_error(rejection.to_string(), context)
}

pub fn fallback_panic_error(
    payload: &(dyn Any + Send),
    context: ErrorFallbackContext,
) -> ConduitError {
    internal_fallback_error(panic_payload_message(payload), context)
}

pub fn internal_fallback_error(
    reason: impl Into<String>,
    context: ErrorFallbackContext,
) -> ConduitError {
    let mut err = ConduitError::internal(reason.into());

    if let Some(request_id) = context.request_id {
        err = err.with_metadata("request_id", Value::String(request_id));
    }

    if let Some(trace_id) = context.trace_id {
        err = err.with_metadata("trace_id", Value::String(trace_id));
    }

    err
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        return (*message).to_string();
    }

    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }

    "panic captured".to_string()
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use axum::body::to_bytes;
    use serde_json::{Value, json};

    use super::*;

    async fn response_json(response: Response) -> Result<(StatusCode, Value), Box<dyn Error>> {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024).await?;
        let body = serde_json::from_slice::<Value>(&bytes)?;
        Ok((status, body))
    }

    #[tokio::test]
    async fn admin_json_does_not_leak_internal_message() -> Result<(), Box<dyn Error>> {
        let err = ConduitError::internal("stack trace: database password");
        let response = conduit_error_response(&err, ErrorResponseFormat::AdminJson);
        let (status, body) = response_json(response).await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({
                "error": {
                    "type": "internal_error",
                    "message": "Internal server error"
                }
            })
        );
        assert!(!body.to_string().contains("stack trace"));

        Ok(())
    }

    #[tokio::test]
    async fn openai_json_has_fixed_error_fields() -> Result<(), Box<dyn Error>> {
        let err = ConduitError::invalid_request("missing model");
        let response = conduit_error_response(&err, ErrorResponseFormat::OpenAiCompatibleJson);
        let (status, body) = response_json(response).await?;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "missing model");
        assert_eq!(body["error"]["type"], "invalid_request");
        assert_eq!(body["error"]["code"], "invalid_request");
        assert_eq!(
            body["error"].as_object().map(|fields| fields.len()),
            Some(3)
        );

        Ok(())
    }

    #[tokio::test]
    async fn internal_fallback_keeps_log_context() -> Result<(), Box<dyn Error>> {
        let err = fallback_rejection_error(
            "router rejection detail",
            ErrorFallbackContext {
                request_id: Some("req_1".to_string()),
                trace_id: Some("trace_1".to_string()),
            },
        );

        let response = conduit_error_response(&err, ErrorResponseFormat::AdminJson);
        let (status, body) = response_json(response).await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["message"], "Internal server error");
        assert_eq!(err.metadata["request_id"], "req_1");
        assert_eq!(err.metadata["trace_id"], "trace_1");
        assert_eq!(err.message, "router rejection detail");

        Ok(())
    }

    #[test]
    fn panic_fallback_captures_payload_for_logs() {
        let payload = "panic detail";
        let err = fallback_panic_error(
            &payload,
            ErrorFallbackContext {
                request_id: None,
                trace_id: Some("trace_2".to_string()),
            },
        );

        assert_eq!(err.message, "panic detail");
        assert_eq!(err.metadata["trace_id"], "trace_2");
        assert_eq!(err.public_message(), "Internal server error");
    }

    // =====================================================================
    // Native per-protocol envelopes (Go `Inbound.TransformError` parity)
    // =====================================================================
    //
    // Go renders every orchestrator error through the *route's own* inbound
    // transformer (`api/chat.go:55`, `api/upstream_error_policy.go:23`), so a
    // Claude client sees the Anthropic envelope and a Gemini client the Google
    // canonical-status envelope. The Rust HTTP layer previously rendered the
    // OpenAI envelope for every route.

    #[tokio::test]
    async fn anthropic_json_renders_native_envelope() -> Result<(), Box<dyn Error>> {
        let err = ConduitError::invalid_request("max_tokens is required");
        let response = conduit_error_response(&err, ErrorResponseFormat::AnthropicJson);
        let (status, body) = response_json(response).await?;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Anthropic's envelope is `{type, error:{message,type}, request_id}` —
        // note the top-level `type`, which the OpenAI envelope has no concept of.
        assert_eq!(body["type"], "invalid_request_error");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["message"], "max_tokens is required");
        assert_eq!(body["request_id"], "");
        // And it must NOT be the OpenAI shape.
        assert!(
            body["error"].get("code").is_none(),
            "anthropic envelope carries no `code` field, got: {body}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn gemini_json_renders_canonical_status_envelope() -> Result<(), Box<dyn Error>> {
        let err = ConduitError::invalid_request("contents is required");
        let response = conduit_error_response(&err, ErrorResponseFormat::GeminiJson);
        let (status, body) = response_json(response).await?;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        // Google canonical-status envelope: `{error:{code,message,status}}`.
        assert_eq!(body["error"]["code"], 400);
        assert_eq!(body["error"]["message"], "contents is required");
        assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");
        Ok(())
    }

    #[tokio::test]
    async fn gemini_status_tracks_http_status() -> Result<(), Box<dyn Error>> {
        // 429 -> RESOURCE_EXHAUSTED (Go `mapHTTPStatusToGeminiStatus`).
        let err = ConduitError::upstream("rate limited").with_http_status(429);
        let response = conduit_error_response(&err, ErrorResponseFormat::GeminiJson);
        let (_, body) = response_json(response).await?;
        assert_eq!(body["error"]["status"], "RESOURCE_EXHAUSTED");
        assert_eq!(body["error"]["code"], 429);
        Ok(())
    }

    #[tokio::test]
    async fn anthropic_masked_message_does_not_leak_internals() -> Result<(), Box<dyn Error>> {
        // The native envelope must use `public_message()` like every other
        // format, so an internal error stays masked.
        let err = ConduitError::internal("stack trace: db password");
        let response = conduit_error_response(&err, ErrorResponseFormat::AnthropicJson);
        let (status, body) = response_json(response).await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["type"], "api_error");
        assert_eq!(body["error"]["message"], "Internal server error");
        assert!(!body.to_string().contains("stack trace"));
        Ok(())
    }
}
