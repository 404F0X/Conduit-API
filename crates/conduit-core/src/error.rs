use std::collections::BTreeMap;
use std::error::Error as StdError;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

const SAFE_PROVIDER_HEADER_NAMES: &[&str] = &[
    "openai-processing-ms",
    "request-id",
    "retry-after",
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
    "x-ratelimit-reset",
    "x-request-id",
];

pub const ERROR_RESPONSE_BODY_METADATA: &str = "conduit.error_response.body";
pub const ERROR_RESPONSE_TYPE_METADATA: &str = "conduit.error_response.type";
pub const ERROR_RESPONSE_REWRITE_CHANNEL_METADATA: &str =
    "conduit.error_response.rewrite_channel_id";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidRequest,
    InvalidModel,
    InvalidResponse,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    RateLimited,
    QuotaExhausted,
    ChannelQueueTimeout,
    Upstream,
    Timeout,
    Db,
    Config,
    Internal,
}

impl ErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidModel => "invalid_model",
            Self::InvalidResponse => "invalid_response",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::RateLimited => "rate_limited",
            Self::QuotaExhausted => "quota_exhausted",
            Self::ChannelQueueTimeout => "channel_queue_timeout",
            Self::Upstream => "upstream_error",
            Self::Timeout => "timeout",
            Self::Db => "db_error",
            Self::Config => "config_error",
            Self::Internal => "internal_error",
        }
    }

    pub const fn default_http_status(self) -> u16 {
        match self {
            Self::InvalidRequest | Self::InvalidModel | Self::InvalidResponse => 400,
            Self::Unauthorized => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::Conflict => 409,
            Self::RateLimited | Self::QuotaExhausted => 429,
            Self::ChannelQueueTimeout | Self::Timeout => 504,
            Self::Upstream => 502,
            Self::Db | Self::Config | Self::Internal => 500,
        }
    }

    pub const fn default_safe_message(self) -> &'static str {
        match self {
            Self::InvalidRequest => "Invalid request",
            Self::InvalidModel => "Invalid model",
            Self::InvalidResponse => "Invalid response",
            Self::Unauthorized => "Unauthorized",
            Self::Forbidden => "Forbidden",
            Self::NotFound => "Not found",
            Self::Conflict => "Conflict",
            Self::RateLimited => "Rate limited",
            Self::QuotaExhausted => "Quota exhausted",
            Self::ChannelQueueTimeout => "Channel queue timeout",
            Self::Upstream => "Upstream provider error",
            Self::Timeout => "Request timed out",
            Self::Db | Self::Config | Self::Internal => "Internal server error",
        }
    }

    pub const fn api_mapping(self) -> ApiErrorMapping {
        api_error_mapping(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ApiErrorMapping {
    pub code: &'static str,
    pub http_status: u16,
    pub safe_message: &'static str,
}

pub const fn api_error_mapping(kind: ErrorKind) -> ApiErrorMapping {
    ApiErrorMapping {
        code: kind.as_str(),
        http_status: kind.default_http_status(),
        safe_message: kind.default_safe_message(),
    }
}

pub trait ApiErrorMappable {
    fn api_error_mapping(&self) -> ApiErrorMapping;
}

impl ApiErrorMappable for ErrorKind {
    fn api_error_mapping(&self) -> ApiErrorMapping {
        api_error_mapping(*self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamErrorPolicyMode {
    Passthrough,
    Hidden,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamErrorPolicy {
    pub mode: UpstreamErrorPolicyMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_message: Option<String>,
}

impl UpstreamErrorPolicy {
    pub fn passthrough() -> Self {
        Self {
            mode: UpstreamErrorPolicyMode::Passthrough,
            custom_message: None,
        }
    }

    pub fn hidden() -> Self {
        Self {
            mode: UpstreamErrorPolicyMode::Hidden,
            custom_message: None,
        }
    }

    pub fn custom(message: impl Into<String>) -> Self {
        Self {
            mode: UpstreamErrorPolicyMode::Custom,
            custom_message: Some(message.into()),
        }
    }
}

#[derive(Debug, Error)]
#[error("{kind:?}: {message}")]
pub struct ConduitError {
    pub kind: ErrorKind,
    pub message: String,
    #[source]
    pub source: Option<Box<dyn StdError + Send + Sync + 'static>>,
    pub http_status: u16,
    pub provider_status: Option<u16>,
    pub code: Option<String>,
    pub safe_message: Option<String>,
    pub metadata: BTreeMap<String, Value>,
    pub provider_body: Option<Value>,
    pub provider_headers_subset: BTreeMap<String, String>,
}

impl ConduitError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
            http_status: kind.default_http_status(),
            provider_status: None,
            code: Some(kind.as_str().to_string()),
            safe_message: Some(kind.default_safe_message().to_string()),
            metadata: BTreeMap::new(),
            provider_body: None,
            provider_headers_subset: BTreeMap::new(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        let message = message.into();
        Self::new(ErrorKind::InvalidRequest, message.clone()).with_safe_message(message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unauthorized, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Forbidden, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }

    pub fn rate_limited(message: impl Into<String>) -> Self {
        let message = message.into();
        Self::new(ErrorKind::RateLimited, message.clone()).with_safe_message(message)
    }

    pub fn quota_exhausted(message: impl Into<String>) -> Self {
        let message = message.into();
        Self::new(ErrorKind::QuotaExhausted, message.clone())
            .with_code("quota_exhausted")
            .with_safe_message(message)
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Upstream, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    pub fn with_source<E>(mut self, source: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        self.source = Some(Box::new(source));
        self
    }

    pub fn with_http_status(mut self, status: u16) -> Self {
        self.http_status = status;
        self
    }

    pub fn with_provider_status(mut self, status: u16) -> Self {
        self.provider_status = Some(status);
        self
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_safe_message(mut self, message: impl Into<String>) -> Self {
        self.safe_message = Some(message.into());
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    pub fn with_provider_body(mut self, body: Value) -> Self {
        self.provider_body = Some(body);
        self
    }

    pub fn with_provider_headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: Into<String>,
    {
        self.provider_headers_subset = filter_provider_headers(headers);
        self
    }

    pub fn error_type(&self) -> &str {
        self.code.as_deref().unwrap_or_else(|| self.kind.as_str())
    }

    pub fn public_message(&self) -> &str {
        self.safe_message
            .as_deref()
            .unwrap_or_else(|| self.kind.default_safe_message())
    }

    pub fn hide_upstream_details(mut self) -> Self {
        self.safe_message = Some(ErrorKind::Upstream.default_safe_message().to_string());
        self.provider_body = None;
        self.provider_headers_subset.clear();
        self.metadata.remove(ERROR_RESPONSE_BODY_METADATA);
        self
    }

    pub fn with_custom_upstream_message(mut self, message: impl Into<String>) -> Self {
        self.safe_message = Some(message.into());
        self.provider_body = None;
        self.provider_headers_subset.clear();
        self.metadata.remove(ERROR_RESPONSE_BODY_METADATA);
        self
    }
}

pub fn openai_error_json(err: &ConduitError) -> Value {
    if let Some(body) = custom_error_response_body(err) {
        return body.clone();
    }
    let error_type = custom_error_response_type(err).unwrap_or_else(|| err.error_type());
    json!({
        "error": {
            "message": err.public_message(),
            "type": error_type,
            "code": err.code.as_deref().unwrap_or(error_type),
        }
    })
}

/// Build the native **Anthropic** error envelope for an `ConduitError`.
///
/// Mirrors Go `(*anthropic.InboundTransformer).TransformError`
/// (`conduit/llm/transformer/anthropic/inbound.go:151-220`):
///
/// ```json
/// { "type": "<type>", "error": { "message": "...", "type": "<type>" }, "request_id": "..." }
/// ```
///
/// Lives here (not in `conduit-transformers`) so the HTTP layer can render it
/// per inbound route: Go picks the envelope via the route's own inbound
/// transformer (`orch.Inbound.TransformError`, `api/upstream_error_policy.go:23`),
/// and `conduit-http` does not depend on the transformers crate.
pub fn anthropic_error_json(err: &ConduitError) -> Value {
    if let Some(body) = custom_error_response_body(err) {
        return body.clone();
    }
    let error_type = custom_error_response_type(err)
        .unwrap_or_else(|| anthropic_error_type_for_status(err.http_status));
    let request_id = err
        .metadata
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "type": error_type,
        "error": {
            "message": err.public_message(),
            "type": error_type,
        },
        "request_id": request_id,
    })
}

/// Build the native **Gemini** error envelope for an `ConduitError`.
///
/// Mirrors Go `(*gemini.InboundTransformer).TransformError`
/// (`conduit/llm/transformer/gemini/inbound.go:114-190`):
///
/// ```json
/// { "error": { "code": <http_status>, "message": "...", "status": "<STATUS>" } }
/// ```
pub fn gemini_error_json(err: &ConduitError) -> Value {
    if let Some(body) = custom_error_response_body(err) {
        return body.clone();
    }
    let error_status = custom_error_response_type(err)
        .unwrap_or_else(|| map_http_status_to_gemini_status(err.http_status));
    json!({
        "error": {
            "code": err.http_status,
            "message": err.public_message(),
            "status": error_status,
        }
    })
}

pub fn custom_error_response_body(err: &ConduitError) -> Option<&Value> {
    err.metadata.get(ERROR_RESPONSE_BODY_METADATA)
}

pub fn custom_error_response_type(err: &ConduitError) -> Option<&str> {
    err.metadata
        .get(ERROR_RESPONSE_TYPE_METADATA)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

/// HTTP status -> Anthropic error-type vocabulary.
///
/// Mirrors the per-status branches of Go's anthropic `TransformError`.
/// `invalid_model_error` is the `ErrInvalidModel` branch (422).
pub fn anthropic_error_type_for_status(http_status: u16) -> &'static str {
    match http_status {
        400 | 409 => "invalid_request_error",
        422 => "invalid_model_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        // Anthropic signals overload with 529; Go maps the 5xx gateway family to
        // `overloaded_error` (`outbound_stream.go:386-442`).
        502 | 503 | 504 | 529 => "overloaded_error",
        _ => "api_error",
    }
}

/// HTTP status -> Gemini canonical status string.
///
/// Mirrors Go `mapHTTPStatusToGeminiStatus`
/// (`conduit/llm/transformer/gemini/inbound.go:192-216`).
pub fn map_http_status_to_gemini_status(status_code: u16) -> &'static str {
    match status_code {
        400 => "INVALID_ARGUMENT",
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        409 => "ALREADY_EXISTS",
        429 => "RESOURCE_EXHAUSTED",
        500 => "INTERNAL",
        501 => "UNIMPLEMENTED",
        503 => "UNAVAILABLE",
        _ => "UNKNOWN",
    }
}

pub fn admin_error_json(err: &ConduitError) -> Value {
    if let Some(body) = custom_error_response_body(err) {
        return body.clone();
    }
    json!({
        "error": {
            "type": custom_error_response_type(err).unwrap_or_else(|| err.error_type()),
            "message": err.public_message(),
        }
    })
}

pub fn filter_provider_headers<I, K, V>(headers: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: Into<String>,
{
    headers
        .into_iter()
        .filter_map(|(name, value)| {
            let normalized = name.as_ref().to_ascii_lowercase();
            SAFE_PROVIDER_HEADER_NAMES
                .contains(&normalized.as_str())
                .then(|| (normalized, value.into()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_request_maps_to_openai_json() {
        let err = ConduitError::invalid_request("missing model");
        let body = openai_error_json(&err);

        assert_eq!(err.http_status, 400);
        assert_eq!(body["error"]["message"], "missing model");
        assert_eq!(body["error"]["type"], "invalid_request");
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    #[test]
    fn quota_exhausted_preserves_code_and_type() {
        let err = ConduitError::quota_exhausted("monthly quota exhausted");
        let body = openai_error_json(&err);

        assert_eq!(err.http_status, 429);
        assert_eq!(body["error"]["message"], "monthly quota exhausted");
        assert_eq!(body["error"]["type"], "quota_exhausted");
        assert_eq!(body["error"]["code"], "quota_exhausted");
    }

    #[test]
    fn admin_json_uses_only_type_and_message() {
        let err = ConduitError::internal("database password leaked in internal log");
        let body = admin_error_json(&err);

        assert_eq!(
            body,
            json!({
                "error": {
                    "type": "internal_error",
                    "message": "Internal server error"
                }
            })
        );
    }

    #[test]
    fn provider_header_filter_keeps_only_safe_headers() {
        let filtered = filter_provider_headers([
            ("Retry-After", "10"),
            ("Authorization", "secret"),
            ("X-Request-Id", "req_123"),
        ]);

        assert_eq!(filtered.get("retry-after"), Some(&"10".to_string()));
        assert_eq!(filtered.get("x-request-id"), Some(&"req_123".to_string()));
        assert!(!filtered.contains_key("authorization"));
    }

    #[test]
    fn core_api_error_mappings_are_stable() {
        let cases = [
            (ErrorKind::InvalidRequest, "invalid_request", 400),
            (ErrorKind::Unauthorized, "unauthorized", 401),
            (ErrorKind::Forbidden, "forbidden", 403),
            (ErrorKind::NotFound, "not_found", 404),
            (ErrorKind::RateLimited, "rate_limited", 429),
            (ErrorKind::QuotaExhausted, "quota_exhausted", 429),
            (ErrorKind::Internal, "internal_error", 500),
        ];

        for (kind, code, status) in cases {
            let mapping = api_error_mapping(kind);

            assert_eq!(mapping.code, code);
            assert_eq!(mapping.http_status, status);
            assert_eq!(kind.api_mapping(), mapping);
            assert_eq!(kind.api_error_mapping(), mapping);
        }
    }

    #[test]
    fn rate_limited_uses_public_message_and_stable_code() {
        let err = ConduitError::rate_limited("slow down");
        let body = openai_error_json(&err);

        assert_eq!(err.http_status, 429);
        assert_eq!(err.error_type(), "rate_limited");
        assert_eq!(body["error"]["message"], "slow down");
        assert_eq!(body["error"]["code"], "rate_limited");
    }
}
