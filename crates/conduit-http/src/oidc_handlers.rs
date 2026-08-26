//! OIDC REST handlers (RUST-P11-003 S03).
//!
//! Ports the gin handler bodies of `conduit/internal/server/api/oidc.go`
//! (225 lines): `NewOIDCHandlers` (warn predicate), `RegisterRoutes` (paths
//! wired in [`crate::router`]), `GetProviders`, `GetAuthorizeURL`,
//! `GetLinkAuthorizeURL`, `Callback`, `getBaseURL` and `Exchange`.
//!
//! Routes (Go `RegisterRoutes` oidc.go:45-52 under the `/oauth` group
//! routes.go:91-94, plus routes.go:77/120):
//!
//! | method | path                             | handler |
//! |--------|----------------------------------|---------|
//! | GET    | `/oauth/oidc/providers`          | [`get_providers`] |
//! | GET    | `/oauth/oidc/authorize/{provider}` | [`get_authorize_url`] |
//! | GET    | `/oauth/oidc/callback`           | [`callback`] |
//! | GET    | `/oauth/oidc/callback/{provider}`| [`callback_with_provider`] |
//! | POST   | `/oauth/oidc/exchange`           | [`exchange`] |
//! | GET    | `/admin/oidc/link/{provider}`    | [`get_link_authorize_url`] |
//!
//! Every error body in oidc.go is the **flat** `gin.H{"error": "<msg>"}`
//! shape — NOT the `JSONError` envelope used by api/system.go / api/auth.go.
//!
//! The handlers talk to the minimal [`OidcService`] trait (defined here, per
//! the S10 pattern); the Go handler struct holds `*biz.OIDCService` +
//! `*biz.AuthService` (oidc.go:19-23) and the trait surface is exactly the
//! union of the methods the handlers call on those two services.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::app_state::AppState;

/// Go warning emitted by `NewOIDCHandlers` (oidc.go:35) when
/// [`should_warn_missing_public_url`] is true. Exposed so the host binary can
/// log the Go-verbatim text at wiring time (this crate carries no logger).
pub const MISSING_PUBLIC_URL_WARNING: &str = "OIDC is enabled but server.public_url is not \
     configured. This is insecure and can lead to Host header injection attacks in production.";

/// `NewOIDCHandlers` warn predicate (oidc.go:34-36):
/// `params.OIDCService.CountProviders() > 0 && params.PublicURL == ""`.
pub fn should_warn_missing_public_url(provider_count: usize, public_url: &str) -> bool {
    provider_count > 0 && public_url.is_empty()
}

/// `biz.ProviderInfo` — ported 1:1 from Go (`biz/oidc.go:38-52`). Note the
/// json tags are **snake_case** (not camelCase like most API structs):
///
/// ```text
/// ID               string `json:"id"`
/// Name             string `json:"name"`
/// DisplayName      string `json:"display_name"`
/// JITEnabled       bool   `json:"jit_enabled"`
/// IconURL          string `json:"icon_url"`
/// ButtonColor      string `json:"button_color"`
/// Active           bool   `json:"active"`
/// OIDCLoginOnly    bool   `json:"oidc_login_only"`
/// LastCheck        int64  `json:"last_check,omitempty"`
/// IsLinked         bool   `json:"is_linked"`
/// LinkedIdentityID string `json:"linked_identity_id,omitempty"`
/// LinkedEmail      string `json:"linked_email,omitempty"`
/// ```
///
/// Rust snake_case field names match the tags verbatim, so no serde rename is
/// needed; `omitempty` maps to `skip_serializing_if` on the Go zero value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub jit_enabled: bool,
    #[serde(default)]
    pub icon_url: String,
    #[serde(default)]
    pub button_color: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub oidc_login_only: bool,
    /// `json:"last_check,omitempty"` — Go int64 zero value is omitted.
    #[serde(default, skip_serializing_if = "i64_is_zero")]
    pub last_check: i64,
    #[serde(default)]
    pub is_linked: bool,
    /// `json:"linked_identity_id,omitempty"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub linked_identity_id: String,
    /// `json:"linked_email,omitempty"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub linked_email: String,
}

/// `omitempty` predicate for Go's `int64` zero value.
fn i64_is_zero(value: &i64) -> bool {
    *value == 0
}

/// User handed back by [`OidcService::exchange_code`]. Stands in for Go's
/// `*ent.User` at this boundary:
///
/// * `id` — the numeric ent user id that feeds `GenerateJWTToken`
///   (`biz/auth.go:100-119`, numeric `user_id` claim);
/// * `user` — the `ent.User` JSON exactly as Go marshals it into the Exchange
///   response (`oidc.go:219-224` embeds the ent entity verbatim). The concrete
///   service owns that shape; the handler passes it through untouched.
#[derive(Debug, Clone, PartialEq)]
pub struct OidcExchangedUser {
    pub id: i64,
    pub user: Value,
}

/// Minimal OIDC-service trait consumed by the handlers. Stands in for the two
/// Go services the handler struct holds (oidc.go:19-23) — only the members
/// `api/oidc.go` actually touches:
///
/// * [`count_providers`](Self::count_providers) ← `OIDCService.CountProviders`
///   (`biz/oidc.go:338-340`);
/// * [`authenticate_jwt_token`](Self::authenticate_jwt_token) ←
///   `AuthService.AuthenticateJWTToken` (`biz/auth.go:160`); the handlers only
///   use the returned user as context identity, so the trait yields the
///   numeric user id;
/// * [`get_providers`](Self::get_providers) ← `OIDCService.GetProviders`
///   (`biz/oidc.go:407-460`); the optional user id mirrors the
///   `contexts.GetUser(ctx)` enrichment that drives `is_linked`
///   (biz/oidc.go:413-422 only reads `u.ID`);
/// * [`get_authorize_url`](Self::get_authorize_url) ←
///   `OIDCService.GetAuthorizeURL` (`biz/oidc.go:508`) → `(authURL, state)`;
/// * [`get_link_authorize_url`](Self::get_link_authorize_url) ←
///   `OIDCService.GetLinkAuthorizeURL` (`biz/oidc.go:601`) → `(authURL, state)`;
/// * [`callback`](Self::callback) ← `OIDCService.Callback` (`biz/oidc.go:616`)
///   → `(exchangeCode, intent)` where intent is `"login"` or `"link"`;
/// * [`exchange_code`](Self::exchange_code) ← `OIDCService.ExchangeCode`
///   (`biz/oidc.go:1252`);
/// * [`generate_jwt_token`](Self::generate_jwt_token) ←
///   `AuthService.GenerateJWTToken` (`biz/auth.go:100`).
///
/// Error payloads are plain `String`s: the Go handlers only ever embed
/// `err.Error()` into the flat `{"error": ...}` body (or substring-match it,
/// oidc.go:203), so no richer error type is needed at this boundary.
#[async_trait::async_trait]
pub trait OidcService: Send + Sync {
    fn count_providers(&self) -> usize;
    async fn authenticate_jwt_token(&self, token: &str) -> Result<i64, String>;
    async fn get_providers(&self, user_id: Option<i64>) -> Vec<ProviderInfo>;
    async fn get_authorize_url(
        &self,
        provider: &str,
        base_url: &str,
    ) -> Result<(String, String), String>;
    async fn get_link_authorize_url(
        &self,
        provider: &str,
        base_url: &str,
        user_id: i64,
    ) -> Result<(String, String), String>;
    async fn callback(
        &self,
        provider: &str,
        code: &str,
        state: &str,
        base_url: &str,
    ) -> Result<(String, String), String>;
    async fn exchange_code(&self, code: &str) -> Result<OidcExchangedUser, String>;
    async fn generate_jwt_token(&self, user: &OidcExchangedUser) -> Result<String, String>;
}

/// `ExchangeRequest` — the anonymous bind struct in Go `Exchange`
/// (oidc.go:192-194): `Code string \`json:"code" binding:"required"\``.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct ExchangeRequest {
    #[serde(default)]
    pub code: String,
}

/// `h.getBaseURL(c)` (oidc.go:176-189), pure form:
///
/// * `server.public_url` wins when non-empty, with one trailing `/` trimmed
///   (`strings.TrimSuffix(publicURL, "/")`, oidc.go:177-179);
/// * otherwise `scheme://host` where scheme is `https` iff
///   `X-Forwarded-Proto == "https"` and `http` otherwise (oidc.go:183-188).
///   Go additionally checks `c.Request.TLS != nil`, which has no equivalent
///   here: this crate's server (like Go's `ListenAndServe` path) terminates
///   plain HTTP only.
pub fn resolve_base_url(public_url: &str, x_forwarded_proto: Option<&str>, host: &str) -> String {
    if !public_url.is_empty() {
        return public_url
            .strip_suffix('/')
            .unwrap_or(public_url)
            .to_string();
    }

    let scheme = if x_forwarded_proto == Some("https") {
        "https"
    } else {
        "http"
    };
    format!("{scheme}://{host}")
}

/// Go `url.QueryEscape` (used at oidc.go:161): unreserved bytes
/// (`A-Z a-z 0-9 - _ . ~`) pass through, space becomes `+`, everything else
/// is `%XX` with uppercase hex.
pub fn query_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                escaped.push(byte as char);
            }
            b' ' => escaped.push('+'),
            _ => escaped.push_str(&format!("%{byte:02X}")),
        }
    }
    escaped
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /oauth/oidc/providers` — Go `OIDCHandlers.GetProviders`
/// (oidc.go:54-68).
///
/// * A `Bearer <token>` Authorization header is authenticated best-effort; on
///   success the user identity enriches the provider list with `is_linked`
///   data, on failure the request proceeds anonymously (oidc.go:58-63).
/// * Response is always `200 {"data": <providers>}` (oidc.go:65-67). With no
///   configured providers Go marshals the nil slice as JSON `null`
///   (`var providers []ProviderInfo`, biz/oidc.go:408) — mirrored by mapping
///   an empty list to `null`.
pub async fn get_providers(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(service) = state.services().oidc_service() else {
        // Rust-only skeleton state: no wired service behaves like a deployment
        // with zero configured providers (nil slice -> null).
        return (StatusCode::OK, Json(json!({ "data": Value::Null }))).into_response();
    };

    // oidc.go:57-63 — optional context enrichment; errors are swallowed.
    let mut user_id = None;
    if let Some(token) = bearer_token(&headers)
        && let Ok(id) = service.authenticate_jwt_token(token).await
    {
        user_id = Some(id);
    }

    let providers = service.get_providers(user_id).await;
    let data = if providers.is_empty() {
        Value::Null
    } else {
        json!(providers)
    };

    (StatusCode::OK, Json(json!({ "data": data }))).into_response()
}

/// `GET /oauth/oidc/authorize/{provider}` — Go `OIDCHandlers.GetAuthorizeURL`
/// (oidc.go:70-92).
///
/// | condition        | status | body |
/// |------------------|--------|------|
/// | empty provider   | 400    | `{"error":"Provider is required"}` (oidc.go:72-75; unreachable via routing in both stacks, ported for body parity) |
/// | service error    | 500    | `{"error":"<err>"}` (oidc.go:81-84) |
/// | success          | 200    | `{"data":{"url":"<authURL>","state":"<state>"}}` (oidc.go:86-91) |
pub async fn get_authorize_url(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Response {
    if provider.is_empty() {
        return flat_error(StatusCode::BAD_REQUEST, "Provider is required");
    }

    let Some(service) = state.services().oidc_service() else {
        // Rust-only skeleton state; degrades to the Go 500 error branch.
        return flat_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "OIDC service is not configured",
        );
    };

    // oidc.go:77-78 — config public URL wins over the request host.
    let base_url = request_base_url(&state, &headers);

    // oidc.go:80-91.
    match service.get_authorize_url(&provider, &base_url).await {
        Ok((url, oidc_state)) => (
            StatusCode::OK,
            Json(json!({ "data": { "url": url, "state": oidc_state } })),
        )
            .into_response(),
        Err(err) => flat_error(StatusCode::INTERNAL_SERVER_ERROR, err),
    }
}

/// `GET /admin/oidc/link/{provider}` — Go `OIDCHandlers.GetLinkAuthorizeURL`
/// (oidc.go:94-125, mounted at routes.go:120 inside the JWT-authenticated
/// admin group).
///
/// Go relies on `middleware.WithJWTAuth` (routes.go:96) to authenticate the
/// Bearer token and stash the user in the request context; the handler then
/// re-checks `contexts.GetUser` (oidc.go:101-106). The Rust router has no auth
/// middleware yet, so the Bearer authentication is inlined here: missing *and*
/// invalid tokens both collapse to the handler's own
/// `401 {"error":"Unauthorized"}` guard. (Transitional deviation: in Go an
/// invalid token is rejected earlier by the middleware with the JSONError
/// envelope, middleware/error.go:12-20.)
///
/// | condition        | status | body |
/// |------------------|--------|------|
/// | empty provider   | 400    | `{"error":"Provider is required"}` (oidc.go:96-99) |
/// | no user          | 401    | `{"error":"Unauthorized"}` (oidc.go:102-106) |
/// | service error    | 500    | `{"error":"<err>"}` (oidc.go:114-117) |
/// | success          | 200    | `{"data":{"url":..., "state":...}}` (oidc.go:119-124) |
pub async fn get_link_authorize_url(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Response {
    if provider.is_empty() {
        return flat_error(StatusCode::BAD_REQUEST, "Provider is required");
    }

    // Unwired service (Rust-only state) cannot authenticate anyone -> the
    // "no user in context" branch.
    let Some(service) = state.services().oidc_service() else {
        return flat_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    };

    // Inlined WithJWTAuth + oidc.go:101-108 (`user.ID`).
    let user_id = match bearer_token(&headers) {
        Some(token) => service.authenticate_jwt_token(token).await.ok(),
        None => None,
    };
    let Some(user_id) = user_id else {
        return flat_error(StatusCode::UNAUTHORIZED, "Unauthorized");
    };

    // oidc.go:110-111.
    let base_url = request_base_url(&state, &headers);

    // oidc.go:113-124.
    match service
        .get_link_authorize_url(&provider, &base_url, user_id)
        .await
    {
        Ok((url, oidc_state)) => (
            StatusCode::OK,
            Json(json!({ "data": { "url": url, "state": oidc_state } })),
        )
            .into_response(),
        Err(err) => flat_error(StatusCode::INTERNAL_SERVER_ERROR, err),
    }
}

/// `GET /oauth/oidc/callback` — Go `OIDCHandlers.Callback` without the
/// `:provider` segment (oidc.go:49, 127-142 single-provider fallback).
pub async fn callback(State(state): State<AppState>, uri: Uri, headers: HeaderMap) -> Response {
    callback_impl(state, String::new(), &uri, &headers).await
}

/// `GET /oauth/oidc/callback/{provider}` — Go `OIDCHandlers.Callback`
/// (oidc.go:50, 127-174).
pub async fn callback_with_provider(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    callback_impl(state, provider, &uri, &headers).await
}

/// Shared body of Go `OIDCHandlers.Callback` (oidc.go:127-174).
///
/// | condition                      | result |
/// |--------------------------------|--------|
/// | no provider, `CountProviders()==1` | fall back to `GetProviders()[0].ID` (oidc.go:129-136) |
/// | provider still empty           | 400 `{"error":"Provider is required"}` (oidc.go:138-141) |
/// | `error` query param non-empty  | 400 `{"error":"<error_description>"}` — the description verbatim, possibly empty (oidc.go:144-151) |
/// | missing code or state          | 400 `{"error":"Code and state are required"}` (oidc.go:153-156) |
/// | service `Callback` error       | 302 → `{base}/oauth/oidc/idp-callback?error=auth_failed&error_description=<QueryEscape(err)>` (oidc.go:158-164) |
/// | intent == "link"               | 302 → `{base}/settings/profile?oidc_link=success` (oidc.go:168-171) |
/// | otherwise (login)              | 302 → `{base}/oauth/oidc/idp-callback?code=<exchangeCode>` — raw concatenation, no escaping, mirroring Go (oidc.go:173) |
async fn callback_impl(
    state: AppState,
    mut provider: String,
    uri: &Uri,
    headers: &HeaderMap,
) -> Response {
    let service = state.services().oidc_service();

    // oidc.go:128-142 — single-provider fallback. The context on this route
    // carries no user (the /oauth group is unauthenticated), hence None.
    if provider.is_empty() {
        if let Some(service) = service
            && service.count_providers() == 1
        {
            let providers = service.get_providers(None).await;
            if let Some(first) = providers.first() {
                provider = first.id.clone();
            }
        }

        if provider.is_empty() {
            return flat_error(StatusCode::BAD_REQUEST, "Provider is required");
        }
    }

    // oidc.go:144-146 — gin `c.Query` returns the first value.
    let raw_query = uri.query().unwrap_or("");
    let code = query_value(raw_query, "code");
    let state_param = query_value(raw_query, "state");
    let error_param = query_value(raw_query, "error");

    // oidc.go:148-151 — provider error short-circuits; the response carries
    // `error_description` verbatim (an empty description yields {"error":""}).
    if !error_param.is_empty() {
        return flat_error(
            StatusCode::BAD_REQUEST,
            query_value(raw_query, "error_description"),
        );
    }

    // oidc.go:153-156.
    if code.is_empty() || state_param.is_empty() {
        return flat_error(StatusCode::BAD_REQUEST, "Code and state are required");
    }

    // oidc.go:158-166 — Go resolves the base URL identically in every branch.
    let base_url = request_base_url(&state, headers);

    let outcome = match service {
        Some(service) => {
            service
                .callback(&provider, &code, &state_param, &base_url)
                .await
        }
        // Rust-only skeleton state; degrades to the Go error-redirect branch.
        None => Err("OIDC service is not configured".to_string()),
    };

    let (exchange_code, intent) = match outcome {
        Ok(outcome) => outcome,
        Err(err) => {
            // oidc.go:159-164.
            return redirect_found(&format!(
                "{base_url}/oauth/oidc/idp-callback?error=auth_failed&error_description={}",
                query_escape(&err)
            ));
        }
    };

    // oidc.go:168-171.
    if intent == "link" {
        return redirect_found(&format!("{base_url}/settings/profile?oidc_link=success"));
    }

    // oidc.go:173 — plain string concatenation, no escaping, as in Go.
    redirect_found(&format!(
        "{base_url}/oauth/oidc/idp-callback?code={exchange_code}"
    ))
}

/// `POST /oauth/oidc/exchange` — Go `OIDCHandlers.Exchange` (oidc.go:191-225).
///
/// | condition                                   | status | body |
/// |---------------------------------------------|--------|------|
/// | bind failure                                | 400    | `{"error":"<bind err>"}` (oidc.go:195-198) |
/// | exchange error containing "invalid or expired" | 400 | `{"error":"<err>"}` (oidc.go:202-206) |
/// | other exchange error                        | 500    | `{"error":"<err>"}` (oidc.go:208) |
/// | token generation error                      | 500    | `{"error":"Failed to generate token: <err>"}` (oidc.go:213-217) |
/// | success                                     | 200    | `{"data":{"token":"<jwt>","user":<ent.User JSON>}}` (oidc.go:219-224) |
pub async fn exchange(
    State(state): State<AppState>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    // oidc.go:195-198 — ShouldBindJSON failure embeds the binding error text.
    let request = match body
        .map_err(|err| err.to_string())
        .and_then(|bytes| bind_exchange_request(&bytes))
    {
        Ok(request) => request,
        Err(err) => return flat_error(StatusCode::BAD_REQUEST, err),
    };

    let Some(service) = state.services().oidc_service() else {
        // Rust-only skeleton state; degrades to the Go 500 error branch.
        return flat_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "OIDC service is not configured",
        );
    };

    // oidc.go:200-211 — "invalid or expired" exchange errors map to 400.
    let user = match service.exchange_code(&request.code).await {
        Ok(user) => user,
        Err(err) if err.contains("invalid or expired") => {
            return flat_error(StatusCode::BAD_REQUEST, err);
        }
        Err(err) => return flat_error(StatusCode::INTERNAL_SERVER_ERROR, err),
    };

    // oidc.go:213-217.
    let token = match service.generate_jwt_token(&user).await {
        Ok(token) => token,
        Err(err) => {
            return flat_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to generate token: {err}"),
            );
        }
    };

    // oidc.go:219-224 — the ent.User JSON is embedded verbatim.
    (
        StatusCode::OK,
        Json(json!({ "data": { "token": token, "user": user.user } })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// gin `ShouldBindJSON` for the Exchange request (oidc.go:192-198).
///
/// Error strings captured from the real Go stack (go1.26 + gin, anonymous
/// struct `Code string \`json:"code" binding:"required"\``):
///
/// * empty body → `"EOF"`;
/// * missing/empty `code` → `"Key: 'Code' Error:Field validation for 'Code'
///   failed on the 'required' tag"`;
/// * malformed JSON / type mismatch → Go json-decoder text (e.g.
///   `"unexpected EOF"`, `"json: cannot unmarshal number into Go struct field
///   .code of type string"`) — input-dependent and decoder-internal, so those
///   branches carry the serde_json message instead (same 400 +
///   `{"error": string}` shape).
fn bind_exchange_request(bytes: &[u8]) -> Result<ExchangeRequest, String> {
    if bytes.is_empty() {
        return Err("EOF".to_string());
    }

    let request: ExchangeRequest = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;

    if request.code.is_empty() {
        return Err(
            "Key: 'Code' Error:Field validation for 'Code' failed on the 'required' tag"
                .to_string(),
        );
    }

    Ok(request)
}

/// `c.JSON(status, gin.H{"error": message})` — the flat error shape used by
/// every oidc.go branch (NOT the api/error.go `JSONError` envelope).
fn flat_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

/// gin `c.Redirect(http.StatusFound, location)` — 302 + `Location`.
///
/// (Go's `http.Redirect` also writes a decorative `<a href>Found</a>` HTML
/// body for GET requests; the wire contract is the status + Location header,
/// so the body is left empty here.)
fn redirect_found(location: &str) -> Response {
    match Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, location)
        .body(Body::empty())
    {
        Ok(response) => response,
        // Location value not header-encodable (the Go side would fail at write
        // time as well); degrade to a bare 500.
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `Authorization: Bearer <token>` extraction — Go
/// `strings.CutPrefix(authHeader, "Bearer ")` (oidc.go:58-59): exact prefix,
/// case-sensitive, single space.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

/// `h.getBaseURL(c)` over live request parts (oidc.go:176-189): config
/// `server.public_url` (Go fx `name:"public_url"` ← `server.Config.PublicURL`,
/// server.go:100) wins; otherwise scheme://Host from the request headers.
fn request_base_url(state: &AppState, headers: &HeaderMap) -> String {
    let x_forwarded_proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok());
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    resolve_base_url(&state.config().server.public_url, x_forwarded_proto, host)
}

/// gin `c.Query(name)` over the raw query string: pairs split on `&`,
/// percent-decoded with `+` as space, the FIRST occurrence wins
/// (`url.Values[name][0]`). Mirroring `net/url.ParseQuery`, pairs containing
/// `;` or an invalid percent-escape are dropped (gin ignores the parse error).
fn query_value(raw_query: &str, name: &str) -> String {
    for segment in raw_query.split('&') {
        if segment.is_empty() || segment.contains(';') {
            continue;
        }
        let (raw_key, raw_value) = segment.split_once('=').unwrap_or((segment, ""));
        let Some(key) = percent_decode_component(raw_key) else {
            continue;
        };
        if key != name {
            continue;
        }
        let Some(value) = percent_decode_component(raw_value) else {
            continue;
        };
        return value;
    }
    String::new()
}

/// `url.QueryUnescape` subset: `+` → space, `%XX` → byte; invalid escapes or
/// non-UTF-8 results reject the component (the caller then drops the pair,
/// like Go's ParseQuery error path).
fn percent_decode_component(component: &str) -> Option<String> {
    let bytes = component.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' => {
                let high = hex_value(*bytes.get(index + 1)?)?;
                let low = hex_value(*bytes.get(index + 2)?)?;
                decoded.push(high << 4 | low);
                index += 3;
            }
            other => {
                decoded.push(other);
                index += 1;
            }
        }
    }

    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::{Method, Request};
    use conduit_config::AppConfig;
    use tower::Service;

    use super::*;
    use crate::app_state::AppServices;
    use crate::router::build_router;

    /// Configurable fake standing in for `biz.OIDCService` + `biz.AuthService`.
    #[derive(Default)]
    struct FakeOidcService {
        providers: Vec<ProviderInfo>,
        fail_authorize: bool,
        fail_token: bool,
        callback_result: Option<Result<(String, String), String>>,
        seen_get_providers_user: Mutex<Option<Option<i64>>>,
        seen_authorize: Mutex<Option<(String, String)>>,
        seen_link: Mutex<Option<(String, String, i64)>>,
        seen_callback: Mutex<Option<(String, String, String, String)>>,
    }

    fn record<T>(slot: &Mutex<Option<T>>, value: T) {
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(value);
        }
    }

    fn seen<T: Clone>(slot: &Mutex<Option<T>>) -> Option<T> {
        match slot.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        }
    }

    #[async_trait::async_trait]
    impl OidcService for FakeOidcService {
        fn count_providers(&self) -> usize {
            self.providers.len()
        }

        async fn authenticate_jwt_token(&self, token: &str) -> Result<i64, String> {
            // The public `/oauth/oidc/providers` route (no middleware) still
            // uses the "good-token" sentinel for its anonymous-vs-identified
            // branch. The guarded `/admin/oidc/link/{provider}` route sits
            // behind `jwt_admin_auth`, which only forwards HS256-valid tokens;
            // mirror Go's shared `biz.AuthService` by decoding those here so
            // the handler's own `contexts.GetUser` re-check (oidc.go:101-108)
            // sees the same user id the middleware authenticated.
            if token == "good-token" {
                return Ok(7);
            }
            match conduit_auth::jwt::decode_hs256(token, TEST_JWT_SECRET) {
                Ok(claims) => Ok(claims.user_id),
                Err(_) => Err("invalid jwt".to_string()),
            }
        }

        async fn get_providers(&self, user_id: Option<i64>) -> Vec<ProviderInfo> {
            record(&self.seen_get_providers_user, user_id);
            let mut providers = self.providers.clone();
            // Mimic biz is_linked enrichment (biz/oidc.go:450-454) when a user
            // identity is present.
            if user_id.is_some()
                && let Some(first) = providers.first_mut()
            {
                first.is_linked = true;
                first.linked_identity_id = "gid://conduit/OIDCIdentity/3".to_string();
                first.linked_email = "ada@example.com".to_string();
            }
            providers
        }

        async fn get_authorize_url(
            &self,
            provider: &str,
            base_url: &str,
        ) -> Result<(String, String), String> {
            record(
                &self.seen_authorize,
                (provider.to_string(), base_url.to_string()),
            );
            if self.fail_authorize {
                return Err("provider discovery failed".to_string());
            }
            if !self.providers.iter().any(|info| info.id == provider) {
                return Err(format!("OIDC provider not found: {provider}"));
            }
            Ok((
                format!("https://idp.example/authorize?provider={provider}"),
                "state-123".to_string(),
            ))
        }

        async fn get_link_authorize_url(
            &self,
            provider: &str,
            base_url: &str,
            user_id: i64,
        ) -> Result<(String, String), String> {
            record(
                &self.seen_link,
                (provider.to_string(), base_url.to_string(), user_id),
            );
            if self.fail_authorize {
                return Err("provider discovery failed".to_string());
            }
            Ok((
                "https://idp.example/authorize?link=1".to_string(),
                "state-link".to_string(),
            ))
        }

        async fn callback(
            &self,
            provider: &str,
            code: &str,
            state: &str,
            base_url: &str,
        ) -> Result<(String, String), String> {
            record(
                &self.seen_callback,
                (
                    provider.to_string(),
                    code.to_string(),
                    state.to_string(),
                    base_url.to_string(),
                ),
            );
            self.callback_result
                .clone()
                .unwrap_or_else(|| Ok(("exchange-code-1".to_string(), "login".to_string())))
        }

        async fn exchange_code(&self, code: &str) -> Result<OidcExchangedUser, String> {
            match code {
                "good-code" => Ok(OidcExchangedUser {
                    id: 7,
                    user: json!({"id": 7, "email": "ada@example.com", "firstName": "Ada"}),
                }),
                "expired-code" => Err("exchange code is invalid or expired".to_string()),
                _ => Err("cache down".to_string()),
            }
        }

        async fn generate_jwt_token(&self, user: &OidcExchangedUser) -> Result<String, String> {
            if self.fail_token {
                return Err("no secret key".to_string());
            }
            Ok(format!("jwt-for-{}", user.id))
        }
    }

    fn sample_provider() -> ProviderInfo {
        ProviderInfo {
            id: "google".to_string(),
            name: "google".to_string(),
            display_name: "Google SSO".to_string(),
            jit_enabled: true,
            icon_url: "https://icon.example/g.png".to_string(),
            button_color: "#4285F4".to_string(),
            active: true,
            oidc_login_only: false,
            last_check: 0,
            is_linked: false,
            linked_identity_id: String::new(),
            linked_email: String::new(),
        }
    }

    fn app_with(service: Arc<FakeOidcService>) -> Router {
        app_with_public_url(service, "")
    }

    /// Shared HS256 secret for the admin-group JWT guard in these tests.
    ///
    /// `/admin/oidc/link/{provider}` lives under Go's `adminGroup`
    /// (`middleware.WithJWTAuth`, routes.go:96); the Rust router mounts it
    /// behind `jwt_admin_auth`, which resolves its signing secret from
    /// `config.api_auth.jwt_secret`. The public `/oauth/oidc/*` routes carry no
    /// guard, so setting this secret is harmless for those fixtures.
    const TEST_JWT_SECRET: &str = "oidc-link-test-secret";

    /// Mint a valid HS256 bearer token accepted by the admin JWT guard for the
    /// given user id, signed with [`TEST_JWT_SECRET`].
    fn mint_admin_jwt(user_id: i64) -> String {
        use conduit_auth::jwt::{Claims, encode_hs256};
        encode_hs256(
            &Claims::new(user_id, format!("user:{user_id}")),
            TEST_JWT_SECRET,
        )
        .unwrap_or_default()
    }

    fn app_with_public_url(service: Arc<FakeOidcService>, public_url: &str) -> Router {
        let mut config = AppConfig::default();
        config.server.public_url = public_url.to_string();
        // Wire the secret the admin JWT guard reads (routes.go:96 adminGroup)
        // so the guarded `/admin/oidc/link/{provider}` route can be reached
        // with a token minted by `mint_admin_jwt`.
        config.api_auth.jwt_secret = Some(TEST_JWT_SECRET.to_string());
        let services = AppServices::new().with_oidc_service(service);
        build_router(AppState::new(Arc::new(config), Arc::new(services)))
    }

    async fn call(
        app: &mut Router,
        request: Request<Body>,
    ) -> Result<(StatusCode, HeaderMap, Vec<u8>), Box<dyn StdError>> {
        let response = app.call(request).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        Ok((status, headers, bytes.to_vec()))
    }

    async fn call_json(
        app: &mut Router,
        request: Request<Body>,
    ) -> Result<(StatusCode, Value), Box<dyn StdError>> {
        let (status, _, bytes) = call(app, request).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    fn get(uri: &str) -> Result<Request<Body>, Box<dyn StdError>> {
        Ok(Request::builder()
            .uri(uri)
            .header(header::HOST, "gateway.test")
            .body(Body::empty())?)
    }

    fn location(headers: &HeaderMap) -> String {
        headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    // ---- ProviderInfo serde (biz/oidc.go:38-52) ---------------------------

    /// Golden JSON shape: snake_case tags, `omitempty` on last_check /
    /// linked_identity_id / linked_email only.
    #[test]
    fn provider_info_matches_go_snake_case_tags_and_omitempty() -> Result<(), Box<dyn StdError>> {
        let value = serde_json::to_value(sample_provider())?;
        assert_eq!(
            value,
            json!({
                "id": "google",
                "name": "google",
                "display_name": "Google SSO",
                "jit_enabled": true,
                "icon_url": "https://icon.example/g.png",
                "button_color": "#4285F4",
                "active": true,
                "oidc_login_only": false,
                "is_linked": false,
            })
        );

        // Inactive + linked provider: the omitempty fields materialize
        // (biz/oidc.go:443-454).
        let mut linked = sample_provider();
        linked.active = false;
        linked.last_check = 1_720_000_000;
        linked.is_linked = true;
        linked.linked_identity_id = "gid://conduit/OIDCIdentity/3".to_string();
        linked.linked_email = "ada@example.com".to_string();
        let value = serde_json::to_value(linked)?;
        assert_eq!(value["last_check"], 1_720_000_000);
        assert_eq!(value["linked_identity_id"], "gid://conduit/OIDCIdentity/3");
        assert_eq!(value["linked_email"], "ada@example.com");
        assert_eq!(value["active"], false);
        Ok(())
    }

    // ---- GET /oauth/oidc/providers (oidc.go:54-68) ------------------------

    /// Anonymous request: 200 {"data":[...]} and the service sees no user.
    #[tokio::test]
    async fn providers_returns_data_list() -> Result<(), Box<dyn StdError>> {
        let service = Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        });
        let mut app = app_with(service.clone());
        let (status, body) = call_json(&mut app, get("/oauth/oidc/providers")?).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"][0]["id"], "google");
        assert_eq!(body["data"][0]["display_name"], "Google SSO");
        assert_eq!(body["data"][0]["is_linked"], false);
        assert_eq!(seen(&service.seen_get_providers_user), Some(None));
        Ok(())
    }

    /// A valid Bearer token enriches the context (oidc.go:58-63): the service
    /// sees the user id and the linked fields appear.
    #[tokio::test]
    async fn providers_bearer_token_enables_is_linked() -> Result<(), Box<dyn StdError>> {
        let service = Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        });
        let mut app = app_with(service.clone());
        let request = Request::builder()
            .uri("/oauth/oidc/providers")
            .header(header::HOST, "gateway.test")
            .header(header::AUTHORIZATION, "Bearer good-token")
            .body(Body::empty())?;
        let (status, body) = call_json(&mut app, request).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"][0]["is_linked"], true);
        assert_eq!(
            body["data"][0]["linked_identity_id"],
            "gid://conduit/OIDCIdentity/3"
        );
        assert_eq!(body["data"][0]["linked_email"], "ada@example.com");
        assert_eq!(seen(&service.seen_get_providers_user), Some(Some(7)));
        Ok(())
    }

    /// An invalid Bearer token is swallowed (`err == nil` gate, oidc.go:60-62):
    /// the request proceeds anonymously with 200.
    #[tokio::test]
    async fn providers_invalid_bearer_stays_anonymous() -> Result<(), Box<dyn StdError>> {
        let service = Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        });
        let mut app = app_with(service.clone());
        let request = Request::builder()
            .uri("/oauth/oidc/providers")
            .header(header::HOST, "gateway.test")
            .header(header::AUTHORIZATION, "Bearer forged")
            .body(Body::empty())?;
        let (status, body) = call_json(&mut app, request).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"][0]["is_linked"], false);
        assert_eq!(seen(&service.seen_get_providers_user), Some(None));
        Ok(())
    }

    /// Zero configured providers marshal as `{"data":null}` — Go's nil slice
    /// (`var providers []ProviderInfo`, biz/oidc.go:408). The unwired-service
    /// skeleton state behaves identically.
    #[tokio::test]
    async fn providers_empty_serializes_null() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeOidcService::default()));
        let (status, body) = call_json(&mut app, get("/oauth/oidc/providers")?).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "data": null }));

        let mut app = build_router(AppState::default());
        let (status, body) = call_json(&mut app, get("/oauth/oidc/providers")?).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "data": null }));
        Ok(())
    }

    // ---- GET /oauth/oidc/authorize/{provider} (oidc.go:70-92) -------------

    /// Happy path: 200 {"data":{"url","state"}} and the service receives the
    /// request-host base URL (public_url empty → "http://<Host>").
    #[tokio::test]
    async fn authorize_url_happy_path() -> Result<(), Box<dyn StdError>> {
        let service = Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        });
        let mut app = app_with(service.clone());
        let (status, body) = call_json(&mut app, get("/oauth/oidc/authorize/google")?).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({"data": {
                "url": "https://idp.example/authorize?provider=google",
                "state": "state-123",
            }})
        );
        assert_eq!(
            seen(&service.seen_authorize),
            Some(("google".to_string(), "http://gateway.test".to_string()))
        );
        Ok(())
    }

    /// getBaseURL (oidc.go:176-189): configured public_url wins with one
    /// trailing "/" trimmed; X-Forwarded-Proto=https upgrades the fallback.
    #[tokio::test]
    async fn authorize_url_base_url_resolution() -> Result<(), Box<dyn StdError>> {
        // public_url with trailing slash → trimmed once.
        let service = Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        });
        let mut app = app_with_public_url(service.clone(), "https://ax.example/");
        let (status, _) = call_json(&mut app, get("/oauth/oidc/authorize/google")?).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            seen(&service.seen_authorize).map(|(_, base)| base),
            Some("https://ax.example".to_string())
        );

        // X-Forwarded-Proto: https → https scheme on the Host fallback.
        let service = Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        });
        let mut app = app_with(service.clone());
        let request = Request::builder()
            .uri("/oauth/oidc/authorize/google")
            .header(header::HOST, "gateway.test")
            .header("X-Forwarded-Proto", "https")
            .body(Body::empty())?;
        let (status, _) = call_json(&mut app, request).await?;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            seen(&service.seen_authorize).map(|(_, base)| base),
            Some("https://gateway.test".to_string())
        );
        Ok(())
    }

    /// Service errors map to 500 {"error": err} (oidc.go:81-84) — including
    /// the unknown-provider error, which biz reports as a plain error.
    #[tokio::test]
    async fn authorize_url_errors_return_500_flat_error() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            fail_authorize: true,
            ..FakeOidcService::default()
        }));
        let (status, body) = call_json(&mut app, get("/oauth/oidc/authorize/google")?).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, json!({ "error": "provider discovery failed" }));

        // Unknown provider.
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        }));
        let (status, body) = call_json(&mut app, get("/oauth/oidc/authorize/ghost")?).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, json!({ "error": "OIDC provider not found: ghost" }));
        Ok(())
    }

    // ---- GET /admin/oidc/link/{provider} (oidc.go:94-125) -----------------

    /// Missing or invalid Bearer token → 401.
    ///
    /// The route is now mounted under Go's `adminGroup` JWT guard
    /// (`middleware.WithJWTAuth`, routes.go:96), ported as the `jwt_admin_auth`
    /// middleware. That middleware rejects missing/invalid tokens *before* the
    /// handler runs, emitting the Go `AbortWithError` JSONError envelope
    /// (`{"error":{"type":"Unauthorized","message":"Invalid token"}}`,
    /// middleware/error.go:12-20) rather than the handler's transitional flat
    /// `{"error":"Unauthorized"}` (oidc.go:102-106, now unreachable for these
    /// two branches). This matches Go, where `WithJWTAuth` owns the rejection.
    #[tokio::test]
    async fn link_requires_authenticated_user() -> Result<(), Box<dyn StdError>> {
        let envelope = json!({
            "error": { "type": "Unauthorized", "message": "Invalid token" }
        });

        // No Authorization header.
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        }));
        let (status, body) = call_json(&mut app, get("/admin/oidc/link/google")?).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, envelope);

        // Invalid token (not a valid HS256 JWT signed with the guard secret).
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        }));
        let request = Request::builder()
            .uri("/admin/oidc/link/google")
            .header(header::HOST, "gateway.test")
            .header(header::AUTHORIZATION, "Bearer forged")
            .body(Body::empty())?;
        let (status, body) = call_json(&mut app, request).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body, envelope);
        Ok(())
    }

    /// Happy path: 200 {"data":{url,state}} and the service receives the
    /// authenticated user id (oidc.go:108-124).
    #[tokio::test]
    async fn link_happy_path_passes_user_id() -> Result<(), Box<dyn StdError>> {
        let service = Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        });
        let mut app = app_with(service.clone());
        // Mint a valid HS256 token for user 7: the `jwt_admin_auth` guard
        // (routes.go:96 adminGroup) must accept it, and the fake service's
        // `authenticate_jwt_token` decodes the same secret back to user 7 — so
        // the handler stashes the id the middleware authenticated (Go's
        // `contexts.GetUser`, oidc.go:101-108).
        let request = Request::builder()
            .uri("/admin/oidc/link/google")
            .header(header::HOST, "gateway.test")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", mint_admin_jwt(7)),
            )
            .body(Body::empty())?;
        let (status, body) = call_json(&mut app, request).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({"data": {
                "url": "https://idp.example/authorize?link=1",
                "state": "state-link",
            }})
        );
        assert_eq!(
            seen(&service.seen_link),
            Some((
                "google".to_string(),
                "http://gateway.test".to_string(),
                7_i64
            ))
        );
        Ok(())
    }

    /// Service errors map to 500 {"error": err} (oidc.go:114-117).
    #[tokio::test]
    async fn link_service_error_returns_500() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            fail_authorize: true,
            ..FakeOidcService::default()
        }));
        let request = Request::builder()
            .uri("/admin/oidc/link/google")
            .header(header::HOST, "gateway.test")
            // Valid guard token (routes.go:96 adminGroup) so the request
            // reaches the handler, which then surfaces the service error.
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", mint_admin_jwt(7)),
            )
            .body(Body::empty())?;
        let (status, body) = call_json(&mut app, request).await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, json!({ "error": "provider discovery failed" }));
        Ok(())
    }

    // ---- GET /oauth/oidc/callback[/{provider}] (oidc.go:127-174) ----------

    /// Login flow: 302 → {base}/oauth/oidc/idp-callback?code=<exchangeCode>
    /// (oidc.go:173) and the service receives (provider, code, state, base).
    #[tokio::test]
    async fn callback_login_redirects_with_exchange_code() -> Result<(), Box<dyn StdError>> {
        let service = Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        });
        let mut app = app_with(service.clone());
        let (status, headers, _) = call(
            &mut app,
            get("/oauth/oidc/callback/google?code=abc&state=xyz")?,
        )
        .await?;

        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(
            location(&headers),
            "http://gateway.test/oauth/oidc/idp-callback?code=exchange-code-1"
        );
        assert_eq!(
            seen(&service.seen_callback),
            Some((
                "google".to_string(),
                "abc".to_string(),
                "xyz".to_string(),
                "http://gateway.test".to_string()
            ))
        );
        Ok(())
    }

    /// Link intent: 302 → {base}/settings/profile?oidc_link=success
    /// (oidc.go:168-171; biz returns "" as the exchange code for link flows).
    #[tokio::test]
    async fn callback_link_intent_redirects_to_settings() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            callback_result: Some(Ok((String::new(), "link".to_string()))),
            ..FakeOidcService::default()
        }));
        let (status, headers, _) = call(
            &mut app,
            get("/oauth/oidc/callback/google?code=abc&state=xyz")?,
        )
        .await?;

        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(
            location(&headers),
            "http://gateway.test/settings/profile?oidc_link=success"
        );
        Ok(())
    }

    /// IdP-reported error: 400 with `error_description` verbatim
    /// (oidc.go:148-151) — including the empty-description case.
    #[tokio::test]
    async fn callback_provider_error_returns_error_description() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        }));
        let (status, body) = call_json(
            &mut app,
            get(
                "/oauth/oidc/callback/google?error=access_denied&error_description=user%20cancelled&code=x&state=y",
            )?,
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, json!({ "error": "user cancelled" }));

        // No description → Go answers {"error":""}.
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        }));
        let (status, body) = call_json(
            &mut app,
            get("/oauth/oidc/callback/google?error=access_denied&code=x&state=y")?,
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, json!({ "error": "" }));
        Ok(())
    }

    /// Missing code and/or state → 400 "Code and state are required"
    /// (oidc.go:153-156).
    #[tokio::test]
    async fn callback_missing_code_or_state_returns_400() -> Result<(), Box<dyn StdError>> {
        for uri in [
            "/oauth/oidc/callback/google?code=abc",
            "/oauth/oidc/callback/google?state=xyz",
            "/oauth/oidc/callback/google",
        ] {
            let mut app = app_with(Arc::new(FakeOidcService {
                providers: vec![sample_provider()],
                ..FakeOidcService::default()
            }));
            let (status, body) = call_json(&mut app, get(uri)?).await?;

            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(
                body,
                json!({ "error": "Code and state are required" }),
                "{uri}"
            );
        }
        Ok(())
    }

    /// Service Callback failure: 302 →
    /// {base}/oauth/oidc/idp-callback?error=auth_failed&error_description=<QueryEscape(err)>
    /// (oidc.go:158-164). Go url.QueryEscape("state expired / bad") =
    /// "state+expired+%2F+bad".
    #[tokio::test]
    async fn callback_service_error_redirects_with_escaped_description()
    -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            callback_result: Some(Err("state expired / bad".to_string())),
            ..FakeOidcService::default()
        }));
        let (status, headers, _) = call(
            &mut app,
            get("/oauth/oidc/callback/google?code=abc&state=xyz")?,
        )
        .await?;

        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(
            location(&headers),
            "http://gateway.test/oauth/oidc/idp-callback?error=auth_failed&error_description=state+expired+%2F+bad"
        );
        Ok(())
    }

    /// Provider-less route with exactly one provider falls back to
    /// GetProviders()[0].ID (oidc.go:129-136).
    #[tokio::test]
    async fn callback_without_provider_single_provider_fallback() -> Result<(), Box<dyn StdError>> {
        let service = Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        });
        let mut app = app_with(service.clone());
        let (status, headers, _) =
            call(&mut app, get("/oauth/oidc/callback?code=abc&state=xyz")?).await?;

        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(
            location(&headers),
            "http://gateway.test/oauth/oidc/idp-callback?code=exchange-code-1"
        );
        assert_eq!(
            seen(&service.seen_callback).map(|(provider, ..)| provider),
            Some("google".to_string())
        );
        Ok(())
    }

    /// Provider-less route with zero or multiple providers → 400
    /// "Provider is required" (oidc.go:138-141).
    #[tokio::test]
    async fn callback_without_provider_rejects_zero_or_multiple() -> Result<(), Box<dyn StdError>> {
        // Zero providers (also covers the unwired-service skeleton state).
        let mut app = app_with(Arc::new(FakeOidcService::default()));
        let (status, body) =
            call_json(&mut app, get("/oauth/oidc/callback?code=abc&state=xyz")?).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, json!({ "error": "Provider is required" }));

        // Two providers: CountProviders() != 1, no fallback.
        let mut second = sample_provider();
        second.id = "github".to_string();
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider(), second],
            ..FakeOidcService::default()
        }));
        let (status, body) =
            call_json(&mut app, get("/oauth/oidc/callback?code=abc&state=xyz")?).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, json!({ "error": "Provider is required" }));
        Ok(())
    }

    // ---- POST /oauth/oidc/exchange (oidc.go:191-225) ----------------------

    async fn post_exchange(
        app: &mut Router,
        payload: &str,
    ) -> Result<(StatusCode, Value), Box<dyn StdError>> {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/oauth/oidc/exchange")
            .header(header::HOST, "gateway.test")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(payload.to_string()))?;
        call_json(app, request).await
    }

    /// Happy path: 200 {"data":{"token","user"}} with the ent.User JSON
    /// passed through verbatim (oidc.go:219-224).
    #[tokio::test]
    async fn exchange_happy_path_returns_token_and_user() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        }));
        let payload = json!({"code": "good-code"}).to_string();
        let (status, body) = post_exchange(&mut app, &payload).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            json!({"data": {
                "token": "jwt-for-7",
                "user": {"id": 7, "email": "ada@example.com", "firstName": "Ada"},
            }})
        );
        Ok(())
    }

    /// Bind failures → 400 {"error": <bind text>} (oidc.go:195-198). The
    /// empty-body and required-tag messages are byte-exact captures from the
    /// real Go stack; other malformed inputs keep the shape with the serde
    /// message.
    #[tokio::test]
    async fn exchange_bind_failures_return_gin_error_text() -> Result<(), Box<dyn StdError>> {
        const REQUIRED_TAG: &str =
            "Key: 'Code' Error:Field validation for 'Code' failed on the 'required' tag";

        for (payload, want) in [
            ("", Some("EOF")),
            ("{}", Some(REQUIRED_TAG)),
            (r#"{"code":""}"#, Some(REQUIRED_TAG)),
            ("{", None),               // Go: "unexpected EOF" (decoder-internal)
            (r#"{"code":123}"#, None), // Go: "json: cannot unmarshal number ..."
        ] {
            let mut app = app_with(Arc::new(FakeOidcService {
                providers: vec![sample_provider()],
                ..FakeOidcService::default()
            }));
            let (status, body) = post_exchange(&mut app, payload).await?;

            assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
            match want {
                Some(text) => assert_eq!(body, json!({ "error": text }), "{payload}"),
                None => {
                    let message = body["error"].as_str().unwrap_or_default();
                    assert!(!message.is_empty(), "{payload}");
                }
            }
        }
        Ok(())
    }

    /// "invalid or expired" exchange errors map to 400 (oidc.go:202-206);
    /// anything else maps to 500 (oidc.go:208).
    #[tokio::test]
    async fn exchange_error_mapping_matches_go() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        }));
        let payload = json!({"code": "expired-code"}).to_string();
        let (status, body) = post_exchange(&mut app, &payload).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body,
            json!({ "error": "exchange code is invalid or expired" })
        );

        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            ..FakeOidcService::default()
        }));
        let payload = json!({"code": "boom-code"}).to_string();
        let (status, body) = post_exchange(&mut app, &payload).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, json!({ "error": "cache down" }));
        Ok(())
    }

    /// GenerateJWTToken failure → 500 "Failed to generate token: <err>"
    /// (oidc.go:213-217).
    #[tokio::test]
    async fn exchange_token_failure_returns_500() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(Arc::new(FakeOidcService {
            providers: vec![sample_provider()],
            fail_token: true,
            ..FakeOidcService::default()
        }));
        let payload = json!({"code": "good-code"}).to_string();
        let (status, body) = post_exchange(&mut app, &payload).await?;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({ "error": "Failed to generate token: no secret key" })
        );
        Ok(())
    }

    // ---- pure helpers ------------------------------------------------------

    /// NewOIDCHandlers warn predicate (oidc.go:34-36).
    #[test]
    fn warn_predicate_matches_go_condition() {
        assert!(should_warn_missing_public_url(1, ""));
        assert!(should_warn_missing_public_url(3, ""));
        assert!(!should_warn_missing_public_url(0, ""));
        assert!(!should_warn_missing_public_url(1, "https://ax.example"));
    }

    /// getBaseURL (oidc.go:176-189).
    #[test]
    fn resolve_base_url_matches_go_get_base_url() {
        // public_url wins; exactly one trailing "/" trimmed (TrimSuffix).
        assert_eq!(
            resolve_base_url("https://ax.example/", None, "ignored"),
            "https://ax.example"
        );
        assert_eq!(
            resolve_base_url("https://ax.example//", Some("https"), "ignored"),
            "https://ax.example/"
        );
        // Fallback: scheme from X-Forwarded-Proto (exact "https" match only).
        assert_eq!(
            resolve_base_url("", None, "gateway.test:8090"),
            "http://gateway.test:8090"
        );
        assert_eq!(
            resolve_base_url("", Some("https"), "gateway.test"),
            "https://gateway.test"
        );
        assert_eq!(
            resolve_base_url("", Some("HTTPS"), "gateway.test"),
            "http://gateway.test"
        );
    }

    /// Go url.QueryEscape golden values (oidc.go:161).
    #[test]
    fn query_escape_matches_go_query_escape() {
        assert_eq!(query_escape("a b"), "a+b");
        assert_eq!(query_escape("a/b"), "a%2Fb");
        assert_eq!(query_escape("A-Za-z0-9-._~"), "A-Za-z0-9-._~");
        assert_eq!(query_escape("中"), "%E4%B8%AD");
        assert_eq!(query_escape("err: no+such"), "err%3A+no%2Bsuch");
        assert_eq!(query_escape(""), "");
    }

    /// gin c.Query semantics: first value wins, percent/plus decoding, pairs
    /// with ';' or broken escapes dropped (net/url.ParseQuery).
    #[test]
    fn query_value_mirrors_gin_first_value_semantics() {
        assert_eq!(query_value("code=abc&state=xyz", "code"), "abc");
        assert_eq!(query_value("code=first&code=second", "code"), "first");
        assert_eq!(query_value("code=a%20b+c", "code"), "a b c");
        assert_eq!(query_value("code", "code"), "");
        assert_eq!(query_value("", "code"), "");
        assert_eq!(query_value("other=1", "code"), "");
        // Pair containing ';' is dropped (Go 1.17+ ParseQuery).
        assert_eq!(query_value("code=a;b", "code"), "");
        // Invalid escape drops the pair; a later valid one is still found.
        assert_eq!(query_value("code=%zz&code=ok", "code"), "ok");
    }
}
