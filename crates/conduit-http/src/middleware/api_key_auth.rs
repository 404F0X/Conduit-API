//! API key extraction middleware (RUST-P2-002 S11).
//!
//! Ports the key-extraction logic from Go `WithAPIKeyAuth` / `WithGeminiKeyAuth`
//! / `WithOpenAPIAuth` (`conduit/internal/server/middleware/auth.go`) to an axum
//! `from_fn`-compatible async middleware.
//!
//! ## Scope
//!
//! This middleware extracts the raw API key and, when the host wires an
//! [`ApiKeyValidationService`], validates it against the database before
//! inserting both [`ApiKeyExtension`] and [`ValidatedApiKeyMetadata`]. A bare
//! test `AppState` may omit the service and retain extraction-only behavior.
//!
//! ## Extraction priority (mirrors Go `ExtractAPIKeyFromRequest` + `WithGeminiKeyAuth`)
//!
//! 1. `Authorization: Bearer <key>` header (standard OpenAI-compatible path)
//! 2. `X-Goog-Api-Key` header (Gemini-compatible path)
//! 3. `?key=<value>` query parameter (Gemini URL auth)
//!
//! If none yields a non-empty key, the middleware short-circuits with 401.
//!
//! ## Go source contracts
//!
//! - `internal/server/middleware/header.go` — `ExtractAPIKeyFromRequest`
//! - `internal/server/middleware/auth.go` — `WithAPIKeyAuth`, `WithGeminiKeyAuth`,
//!   `WithOpenAPIAuth`

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::AppState;
use crate::api_error::json_error;

/// Header name for the Gemini API key (case-insensitive in `HeaderMap`).
const GEMINI_API_KEY_HEADER: &str = "x-goog-api-key";

/// Public error message when no API key is found (matches Go `ErrAPIKeyRequired`
/// surface: `AbortWithError(c, 401, ...)` with "Invalid API key").
const MISSING_KEY_MESSAGE: &str = "Invalid API key";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Where the API key was extracted from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKeySource {
    /// `Authorization: Bearer <key>` header.
    Bearer,
    /// `X-Goog-Api-Key` header.
    GeminiHeader,
    /// `?key=<value>` URL query parameter.
    QueryParam,
}

/// A raw API key string together with its extraction source.
///
/// "Raw" means the key has been extracted from the HTTP request but has NOT
/// been validated against the database. Downstream handlers receive this via
/// [`ApiKeyExtension`] and are responsible for calling the auth service to
/// authenticate it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawApiKey {
    /// The key value (trimmed, guaranteed non-empty).
    pub value: String,
    /// Where the key was found.
    pub source: ApiKeySource,
}

/// Typed request extension carrying the extracted API key.
///
/// Inserted by [`api_key_auth`] middleware; downstream handlers extract it via
/// `request.extensions().get::<ApiKeyExtension>()` or
/// `axum::Extension<ApiKeyExtension>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyExtension(pub RawApiKey);

impl ApiKeyExtension {
    /// Access the raw API key.
    pub fn raw_key(&self) -> &RawApiKey {
        &self.0
    }

    /// Consume the extension, returning the inner key.
    pub fn into_raw_key(self) -> RawApiKey {
        self.0
    }
}

/// DB-validated API key metadata, inserted as an axum extension once the
/// auth service resolves the raw key to a database entity.
///
/// Mirrors the Go `*ent.APIKey` fields that `WithAPIKeyAuth` stores on the
/// request context via `contexts.WithAPIKey(ctx, apiKey)` +
/// `contexts.WithProjectID(ctx, apiKey.Edges.Project.ID)` — the subset the
/// pipeline needs to enforce model-whitelist, quota, and billing without
/// re-querying the DB.
///
/// Inserted by the handler/service layer AFTER the raw key has been validated
/// against the database. The [`api_key_auth`] middleware only extracts the
/// raw key; this struct carries the validated identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidatedApiKeyMetadata {
    /// Database ID of the API key (Go `apiKey.ID`).
    pub api_key_id: i64,
    /// Human-readable name of the key (Go `apiKey.Name`).
    pub api_key_name: String,
    /// Go API key type: `user`, `service_account`, or `noauth`.
    pub key_type: String,
    /// Authorization scopes attached to the key.
    pub scopes: Vec<String>,
    /// Comma-separated list of model IDs the key's active profile allows.
    /// Empty string means "no restriction" (all models allowed). Mirrors
    /// Go `apiKey.Profiles.ActiveProfile` → `profile.ModelIDs`.
    pub allowed_models: String,
    /// Project ID the key belongs to (Go `apiKey.ProjectID`). `0` when no
    /// project association exists.
    pub project_id: i64,
    /// JSON object mapping client model names to effective model names.
    pub model_mapping: String,
    pub key_channel_ids: Vec<i64>,
    pub key_channel_tags: Vec<String>,
    pub key_channel_tags_match_mode: String,
    pub project_channel_ids: Vec<i64>,
    /// Effective Project channel allow-list per public model key.
    pub project_channels_by_model: std::collections::BTreeMap<String, Vec<i64>>,
    /// Effective canonical model -> channel -> provider model mapping sourced
    /// from enabled channel-model offers.
    pub project_upstream_models_by_model:
        std::collections::BTreeMap<String, std::collections::BTreeMap<i64, String>>,
    pub project_channel_tags: Vec<String>,
    pub project_channel_tags_match_mode: String,
    pub load_balance_strategy: String,
    /// Per-API-key requests-per-minute cap derived from the profile's quota
    /// configuration. `None` means no RPM limit is configured and the quota
    /// middleware will pass through.
    pub quota_rpm: Option<i64>,
    pub max_concurrent_requests: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyValidationError {
    Invalid,
    Internal,
    QuotaExceeded,
}

#[async_trait]
pub trait ApiKeyValidationService: Send + Sync {
    async fn validate(
        &self,
        plaintext_key: &str,
    ) -> Result<ValidatedApiKeyMetadata, ApiKeyValidationError>;
}

/// Well-known metadata key names stamped onto `HttpRequest.metadata` so
/// pipeline middlewares can read API key info without `PersistenceState`.
///
/// Each constant corresponds to one field of [`ValidatedApiKeyMetadata`];
/// the values are serialized as `serde_json::Value::String` / `Number` on
/// the `HttpRequest.metadata` map (`BTreeMap<String, Value>`).
pub mod api_key_meta_keys {
    /// `i64` — database ID of the API key.
    pub const API_KEY_ID: &str = "api_key_id";
    /// `String` — human-readable name of the key.
    pub const API_KEY_NAME: &str = "api_key_name";
    /// `String` — comma-separated allowed model IDs (empty = unrestricted).
    pub const API_KEY_ALLOWED_MODELS: &str = "api_key_allowed_models";
    /// `i64` — project ID the key belongs to (`0` = none).
    pub const API_KEY_PROJECT_ID: &str = "api_key_project_id";
    /// JSON object used by the model-mapping pipeline middleware.
    pub const API_KEY_MODEL_MAPPING: &str = "api_key_model_mapping";
    pub const KEY_CHANNEL_IDS: &str = "api_key_channel_ids";
    pub const KEY_CHANNEL_TAGS: &str = "api_key_channel_tags";
    pub const KEY_CHANNEL_TAGS_MATCH_MODE: &str = "api_key_channel_tags_match_mode";
    pub const PROJECT_CHANNEL_IDS: &str = "project_channel_ids";
    pub const PROJECT_CHANNELS_BY_MODEL: &str = "project_channels_by_model";
    pub const PROJECT_UPSTREAM_MODELS_BY_MODEL: &str = "project_upstream_models_by_model";
    pub const PROJECT_CHANNEL_TAGS: &str = "project_channel_tags";
    pub const PROJECT_CHANNEL_TAGS_MATCH_MODE: &str = "project_channel_tags_match_mode";
    pub const LOAD_BALANCE_STRATEGY: &str = "api_key_load_balance_strategy";
    /// `i64` — per-API-key RPM cap from the profile quota configuration.
    pub const API_KEY_QUOTA_RPM: &str = "api_key_quota_rpm";
    pub const API_KEY_MAX_CONCURRENT: &str = "api_key_max_concurrent";
}

// ---------------------------------------------------------------------------
// Pure extraction logic
// ---------------------------------------------------------------------------

/// Extract an API key from the request's headers and query string.
///
/// Priority (mirrors Go):
/// 1. `Authorization: Bearer <key>` — strips the `Bearer ` prefix, trims.
/// 2. `X-Goog-Api-Key` header — raw value, trimmed.
/// 3. `?key=<value>` query parameter — URL-decoded by the URI parser, trimmed.
///
/// Returns `None` if no source yields a non-empty key.
pub fn extract_api_key(headers: &HeaderMap, uri: &Uri) -> Option<RawApiKey> {
    // 1) Authorization: Bearer <key>
    if let Some(key) = extract_bearer_key(headers) {
        return Some(RawApiKey {
            value: key,
            source: ApiKeySource::Bearer,
        });
    }

    // 2) X-Goog-Api-Key header
    if let Some(key) = extract_gemini_header_key(headers) {
        return Some(RawApiKey {
            value: key,
            source: ApiKeySource::GeminiHeader,
        });
    }

    // 3) ?key=<value> query param
    if let Some(key) = extract_query_key(uri) {
        return Some(RawApiKey {
            value: key,
            source: ApiKeySource::QueryParam,
        });
    }

    None
}

/// Extract Bearer token from `Authorization` header.
///
/// Mirrors Go `ExtractAPIKeyFromRequest` with `RequireBearer: false` for the
/// default config — the `Authorization` header is checked for the `Bearer `
/// prefix; if it has a different prefix (from `AllowedPrefixes`), Go strips
/// that too. For simplicity this middleware only handles `Bearer ` (the
/// dominant path for OpenAI-compatible clients); other prefixes are rare and
/// can be added if needed.
///
/// Also handles the case where Authorization header has no "Bearer " prefix:
/// in that scenario, the Go code with default config (`RequireBearer: false`)
/// tries `AllowedPrefixes` and if none match, uses the raw value. We mirror
/// this by falling through to other sources (returning None here), letting
/// X-Goog-Api-Key or query param pick it up.
fn extract_bearer_key(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;

    // Must have "Bearer " prefix for this source
    if !raw.starts_with("Bearer ") {
        return None;
    }

    let key = raw.get("Bearer ".len()..)?.trim();
    if key.is_empty() {
        return None;
    }

    Some(key.to_string())
}

/// Extract key from `X-Goog-Api-Key` header (Gemini-compatible).
fn extract_gemini_header_key(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(GEMINI_API_KEY_HEADER)?.to_str().ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Extract key from `?key=<value>` query parameter.
///
/// Mirrors Go `c.Query("key")` in `WithGeminiKeyAuth`.
fn extract_query_key(uri: &Uri) -> Option<String> {
    let query = uri.query()?;
    // Simple query parsing without pulling in a full query-string crate.
    // Go's `c.Query("key")` returns the first occurrence of `key=...`.
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "key" {
            // Percent-decode the value (basic: only %XX sequences)
            let decoded = percent_decode(v);
            let trimmed = decoded.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

/// Minimal percent-decoding for query parameter values.
///
/// Handles `%XX` hex pairs (case-insensitive) and `+` as space. Does not
/// pull in the `percent-encoding` crate to keep dependencies minimal.
fn percent_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            out.push(' ');
            i += 1;
        } else if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex_digit(bytes[i + 1]);
            let lo = hex_digit(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h << 4 | l) as char);
                i += 3;
            } else {
                out.push('%');
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// API key extraction and validation middleware.
///
/// Extracts the API key from the incoming request using [`extract_api_key`].
/// If a key is found, inserts [`ApiKeyExtension`] into request extensions and
/// calls `next`. If no key is found, short-circuits with a 401 JSON error
/// matching the Go `AbortWithError(c, 401, "Invalid API key")` shape.
///
/// Wire via:
/// ```ignore
/// Router::new()
///     .route("/v1/chat/completions", post(handler))
///     .route_layer(axum::middleware::from_fn(api_key_auth))
/// ```
pub async fn api_key_auth(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(raw_key) = extract_api_key(request.headers(), request.uri()) else {
        return json_error(StatusCode::UNAUTHORIZED, MISSING_KEY_MESSAGE);
    };

    // P-24 (fail closed): Go's `WithAPIKeyAuth` is *always* constructed with a
    // live `AuthService` — there is no "no validator" state on the LLM routes.
    // The `Option` here is only a Rust testability seam. If a production host
    // reaches this middleware without a validator wired it is misconfigured, so
    // we MUST reject (500) rather than admit an unvalidated key. Hosts/tests
    // that intentionally want extraction *without* validation use the explicit
    // [`extract_api_key_only`] layer instead.
    let Some(service) = state.services().api_key_validation_service() else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "API key validation unavailable",
        );
    };

    let mut request = request;
    match service.validate(&raw_key.value).await {
        Ok(metadata) => {
            request.extensions_mut().insert(metadata);
        }
        Err(ApiKeyValidationError::Invalid) => {
            return json_error(StatusCode::UNAUTHORIZED, MISSING_KEY_MESSAGE);
        }
        Err(ApiKeyValidationError::QuotaExceeded) => {
            // Quota exhaustion is retryable after the configured window rolls
            // over and must be distinguishable from a permanent authorization
            // denial. Match the domain `QuotaExhausted` mapping and standard
            // OpenAI-compatible semantics with HTTP 429.
            return json_error(StatusCode::TOO_MANY_REQUESTS, "API key quota exceeded");
        }
        Err(ApiKeyValidationError::Internal) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to validate API key",
            );
        }
    }
    request.extensions_mut().insert(ApiKeyExtension(raw_key));
    next.run(request).await
}

/// Extraction-only API-key layer: pulls the key out of the request and inserts
/// [`ApiKeyExtension`] **without validating it**, then continues (missing key
/// still short-circuits with 401).
///
/// This is deliberately separate from [`api_key_auth`] (P-24): it exists for
/// hosts that validate the key in a *downstream* layer and for extraction-
/// focused tests. **Never** place this in front of an LLM handler as the only
/// guard — it performs no authentication. [`api_key_auth`] is the fail-closed
/// production entry point.
pub async fn extract_api_key_only(request: Request<Body>, next: Next) -> Response {
    let Some(raw_key) = extract_api_key(request.headers(), request.uri()) else {
        return json_error(StatusCode::UNAUTHORIZED, MISSING_KEY_MESSAGE);
    };
    let mut request = request;
    request.extensions_mut().insert(ApiKeyExtension(raw_key));
    next.run(request).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{HeaderValue, Request, StatusCode, Uri, header};
    use axum::middleware::{from_fn, from_fn_with_state};
    use axum::routing::get;
    use tower::Service;

    use super::*;

    /// Dummy handler that confirms the middleware passed the key through.
    async fn echo_key(
        axum::Extension(ext): axum::Extension<ApiKeyExtension>,
    ) -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({
            "key": ext.raw_key().value,
            "source": format!("{:?}", ext.raw_key().source),
        }))
    }

    // Extraction-focused tests exercise the pure key-parsing path, so they use
    // the explicit `extract_api_key_only` layer (no validator needed). The
    // fail-closed `api_key_auth` path is covered by
    // `missing_validator_fails_closed` + the `validated_router` tests.
    fn build_router() -> Router {
        Router::new()
            .route("/api/test", get(echo_key))
            .route_layer(from_fn(extract_api_key_only))
    }

    #[tokio::test]
    async fn missing_validator_fails_closed() -> Result<(), Box<dyn Error>> {
        // Production `api_key_auth` with NO validator wired + a present key must
        // 500 (fail closed), never admit the unvalidated key (P-24).
        let mut router = Router::new()
            .route("/api/test", get(echo_key))
            .route_layer(from_fn_with_state(AppState::default(), api_key_auth));
        let request = Request::builder()
            .uri("/api/test")
            .header(header::AUTHORIZATION, "Bearer conduit-unvalidated")
            .body(Body::empty())?;
        let response = router.call(request).await?;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        Ok(())
    }

    struct FakeValidationService {
        result: Result<ValidatedApiKeyMetadata, ApiKeyValidationError>,
    }

    #[async_trait]
    impl ApiKeyValidationService for FakeValidationService {
        async fn validate(
            &self,
            _plaintext_key: &str,
        ) -> Result<ValidatedApiKeyMetadata, ApiKeyValidationError> {
            self.result.clone()
        }
    }

    async fn echo_validated(
        axum::Extension(meta): axum::Extension<ValidatedApiKeyMetadata>,
    ) -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({
            "id": meta.api_key_id,
            "mapping": serde_json::from_str::<serde_json::Value>(&meta.model_mapping)
                .unwrap_or_default(),
        }))
    }

    fn validated_router(result: Result<ValidatedApiKeyMetadata, ApiKeyValidationError>) -> Router {
        let services = crate::AppServices::new()
            .with_api_key_validation_service(Arc::new(FakeValidationService { result }));
        let state = AppState::new(
            Arc::new(conduit_config::AppConfig::default()),
            Arc::new(services),
        );
        Router::new()
            .route("/api/test", get(echo_validated))
            .route_layer(from_fn_with_state(state.clone(), api_key_auth))
            .with_state(state)
    }

    #[tokio::test]
    async fn production_validator_inserts_validated_metadata() -> Result<(), Box<dyn Error>> {
        let mut router = validated_router(Ok(ValidatedApiKeyMetadata {
            api_key_id: 42,
            api_key_name: "production".to_string(),
            allowed_models: "gpt-4".to_string(),
            project_id: 7,
            model_mapping: r#"{"gpt-4":"gpt-4o"}"#.to_string(),
            ..ValidatedApiKeyMetadata::default()
        }));
        let request = Request::builder()
            .uri("/api/test")
            .header(header::AUTHORIZATION, "Bearer conduit-valid")
            .body(Body::empty())?;
        let response = router.call(request).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 4096).await?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(json["id"], 42);
        assert_eq!(json["mapping"]["gpt-4"], "gpt-4o");
        Ok(())
    }

    #[tokio::test]
    async fn production_validator_rejects_invalid_key() -> Result<(), Box<dyn Error>> {
        let mut router = validated_router(Err(ApiKeyValidationError::Invalid));
        let request = Request::builder()
            .uri("/api/test")
            .header(header::AUTHORIZATION, "Bearer conduit-invalid")
            .body(Body::empty())?;
        let response = router.call(request).await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn production_validator_reports_quota_exhaustion_as_429() -> Result<(), Box<dyn Error>> {
        let mut router = validated_router(Err(ApiKeyValidationError::QuotaExceeded));
        let request = Request::builder()
            .uri("/api/test")
            .header(header::AUTHORIZATION, "Bearer conduit-exhausted")
            .body(Body::empty())?;
        let response = router.call(request).await?;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        Ok(())
    }

    // --- Test: Bearer token extracted from Authorization header ---------------

    #[tokio::test]
    async fn bearer_token_extracted_from_authorization_header() -> Result<(), Box<dyn Error>> {
        let mut router = build_router();

        let request = Request::builder()
            .method("GET")
            .uri("/api/test")
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_static("Bearer sk-test-key-123"),
            )
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(body["key"], "sk-test-key-123");
        assert_eq!(body["source"], "Bearer");
        Ok(())
    }

    // --- Test: Gemini key extracted from X-Goog-Api-Key header ----------------

    #[tokio::test]
    async fn gemini_key_extracted_from_header() -> Result<(), Box<dyn Error>> {
        let mut router = build_router();

        let request = Request::builder()
            .method("GET")
            .uri("/api/test")
            .header(
                "x-goog-api-key",
                HeaderValue::from_static("AIzaSy-gemini-key"),
            )
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(body["key"], "AIzaSy-gemini-key");
        assert_eq!(body["source"], "GeminiHeader");
        Ok(())
    }

    // --- Test: Query param key extracted -------------------------------------

    #[tokio::test]
    async fn query_param_key_extracted() -> Result<(), Box<dyn Error>> {
        let mut router = build_router();

        let request = Request::builder()
            .method("GET")
            .uri("/api/test?key=my-query-key&other=val")
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(body["key"], "my-query-key");
        assert_eq!(body["source"], "QueryParam");
        Ok(())
    }

    // --- Test: Missing key returns 401 ---------------------------------------

    #[tokio::test]
    async fn missing_key_returns_401() -> Result<(), Box<dyn Error>> {
        let mut router = build_router();

        let request = Request::builder()
            .method("GET")
            .uri("/api/test")
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body_bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(
            body,
            serde_json::json!({
                "error": {
                    "type": "Unauthorized",
                    "message": "Invalid API key",
                }
            })
        );
        Ok(())
    }

    // --- Test: Bearer takes priority over query param -------------------------

    #[tokio::test]
    async fn bearer_takes_priority_over_query_param() -> Result<(), Box<dyn Error>> {
        let mut router = build_router();

        let request = Request::builder()
            .method("GET")
            .uri("/api/test?key=query-key")
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_static("Bearer bearer-key"),
            )
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(body["key"], "bearer-key");
        assert_eq!(body["source"], "Bearer");
        Ok(())
    }

    // --- Test: Malformed Authorization (no Bearer prefix) falls through -------

    #[tokio::test]
    async fn malformed_auth_header_falls_through_to_other_sources() -> Result<(), Box<dyn Error>> {
        let mut router = build_router();

        // Authorization has "Token " prefix (not "Bearer "), but X-Goog-Api-Key is present
        let request = Request::builder()
            .method("GET")
            .uri("/api/test")
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_static("Token some-token"),
            )
            .header(
                "x-goog-api-key",
                HeaderValue::from_static("gemini-fallback"),
            )
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(body["key"], "gemini-fallback");
        assert_eq!(body["source"], "GeminiHeader");
        Ok(())
    }

    // --- Test: Blank Bearer value falls through to next source ----------------

    #[tokio::test]
    async fn blank_bearer_falls_through_to_query_param() -> Result<(), Box<dyn Error>> {
        let mut router = build_router();

        let request = Request::builder()
            .method("GET")
            .uri("/api/test?key=fallback-query")
            .header(header::AUTHORIZATION, HeaderValue::from_static("Bearer   "))
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(body["key"], "fallback-query");
        assert_eq!(body["source"], "QueryParam");
        Ok(())
    }

    // --- Test: Gemini header takes priority over query param ------------------

    #[tokio::test]
    async fn gemini_header_takes_priority_over_query_param() -> Result<(), Box<dyn Error>> {
        let mut router = build_router();

        let request = Request::builder()
            .method("GET")
            .uri("/api/test?key=query-key")
            .header(
                "x-goog-api-key",
                HeaderValue::from_static("gemini-header-key"),
            )
            .body(Body::empty())?;
        let response = router.call(request).await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body_bytes = axum::body::to_bytes(response.into_body(), 4096).await?;
        let body: serde_json::Value = serde_json::from_slice(&body_bytes)?;
        assert_eq!(body["key"], "gemini-header-key");
        assert_eq!(body["source"], "GeminiHeader");
        Ok(())
    }

    // --- Pure function unit tests --------------------------------------------

    #[test]
    fn extract_api_key_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer sk-abc123"),
        );
        let uri: Uri = "/test".parse().unwrap_or_default();

        let result = extract_api_key(&headers, &uri);
        assert_eq!(
            result,
            Some(RawApiKey {
                value: "sk-abc123".to_string(),
                source: ApiKeySource::Bearer,
            })
        );
    }

    #[test]
    fn extract_api_key_gemini_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-goog-api-key", HeaderValue::from_static("AIzaSy-test"));
        let uri: Uri = "/test".parse().unwrap_or_default();

        let result = extract_api_key(&headers, &uri);
        assert_eq!(
            result,
            Some(RawApiKey {
                value: "AIzaSy-test".to_string(),
                source: ApiKeySource::GeminiHeader,
            })
        );
    }

    #[test]
    fn extract_api_key_query_param() {
        let headers = HeaderMap::new();
        let uri: Uri = "/test?key=qk-123&foo=bar".parse().unwrap_or_default();

        let result = extract_api_key(&headers, &uri);
        assert_eq!(
            result,
            Some(RawApiKey {
                value: "qk-123".to_string(),
                source: ApiKeySource::QueryParam,
            })
        );
    }

    #[test]
    fn extract_api_key_none_when_all_empty() {
        let headers = HeaderMap::new();
        let uri: Uri = "/test?other=value".parse().unwrap_or_default();

        let result = extract_api_key(&headers, &uri);
        assert_eq!(result, None);
    }

    #[test]
    fn extract_api_key_empty_query_key_returns_none() {
        let headers = HeaderMap::new();
        let uri: Uri = "/test?key=".parse().unwrap_or_default();

        let result = extract_api_key(&headers, &uri);
        assert_eq!(result, None);
    }

    #[test]
    fn percent_decode_handles_encoded_chars() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("key%3Dvalue"), "key=value");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("no%encoding"), "no%encoding"); // invalid %en
        assert_eq!(percent_decode("plain"), "plain");
    }
}
