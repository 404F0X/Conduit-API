/// Mirrors the admin OAuth route set mounted in
/// `conduit/internal/server/routes.go:106-117` (all behind the JWT-protected
/// `/admin` group). Each variant corresponds to exactly one Go route:
/// - `Start { provider }`        -> `POST /admin/{provider}/oauth/start`
/// - `Exchange { provider }`     -> `POST /admin/{provider}/oauth/exchange`
///   (Copilot has *no* exchange route)
/// - `Poll { provider }`         -> `POST /admin/copilot/oauth/poll`
/// - `DecodeAuthJson`            -> `POST /admin/codex/auth/decode`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdminOAuthRoute {
    Start { provider: OAuthProvider },
    Exchange { provider: OAuthProvider },
    Poll { provider: OAuthProvider },
    DecodeAuthJson,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OAuthProvider {
    Codex,
    ClaudeCode,
    Antigravity,
    Copilot,
}

/// Parses an admin OAuth/auth request target into [`AdminOAuthRoute`].
///
/// Mirrors `conduit/internal/server/routes.go:106-117` exactly. The path must
/// begin with `/admin/`, and provider spellings are matched verbatim
/// (`codex`, `claudecode`, `antigravity`, `copilot`). The legacy
/// `/oauth/...` shape (no `admin` prefix, hyphenated provider) is **not**
/// recognized and returns `None`.
pub fn parse_admin_oauth_route(request_target: &str) -> Option<AdminOAuthRoute> {
    let path = request_path_without_query(request_target);
    // Split into exactly four segments: "admin", "{provider}", "{group}", "{action}".
    // Anything with a different segment count (including trailing extra segments)
    // is rejected.
    let mut segments = path.trim_start_matches('/').split('/');
    let seg0 = segments.next()?;
    let seg1 = segments.next()?;
    let seg2 = segments.next()?;
    let seg3 = segments.next()?;
    if seg0 != "admin" || segments.next().is_some() {
        return None;
    }
    let provider = parse_provider(seg1)?;
    match (seg2, seg3, provider) {
        // /admin/codex/auth/decode  -- special case, not under oauth/
        ("auth", "decode", OAuthProvider::Codex) => Some(AdminOAuthRoute::DecodeAuthJson),
        // /admin/copilot/oauth/poll -- Copilot-only poll route
        ("oauth", "poll", OAuthProvider::Copilot) => Some(AdminOAuthRoute::Poll {
            provider: OAuthProvider::Copilot,
        }),
        // /admin/{provider}/oauth/start  -- all four providers
        ("oauth", "start", provider) => Some(AdminOAuthRoute::Start { provider }),
        // /admin/{provider}/oauth/exchange  -- everyone except Copilot
        ("oauth", "exchange", provider) if provider != OAuthProvider::Copilot => {
            Some(AdminOAuthRoute::Exchange { provider })
        }
        _ => None,
    }
}

fn parse_provider(provider: &str) -> Option<OAuthProvider> {
    match provider {
        "codex" => Some(OAuthProvider::Codex),
        "claudecode" => Some(OAuthProvider::ClaudeCode),
        "antigravity" => Some(OAuthProvider::Antigravity),
        "copilot" => Some(OAuthProvider::Copilot),
        _ => None,
    }
}

fn request_path_without_query(request_target: &str) -> &str {
    request_target
        .split_once('?')
        .map_or(request_target, |(path, _)| path)
}

// ---------------------------------------------------------------------------
// Handler bodies — ports the gin handler bodies of
// `conduit/internal/server/api/codex.go` + `claudecode.go`
// (RUST-P11-003 S08). The route parser above (Hegel-the-2nd) dispatches
// to these bodies. antigravity.go + copilot.go handlers are listed as
// gaps: routes are mounted for parity but the handler bodies return 501
// NotImplemented until ported.
// ---------------------------------------------------------------------------

use async_trait::async_trait;
use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::api_error::json_error;
use crate::app_state::AppState;

/// `httpclient.ProxyConfig` subset — Go tags verbatim. Only the two fields
/// the `req.Proxy.Type == ProxyTypeURL && req.Proxy.URL != ""` guard reads
/// (codex.go:238 / claudecode.go:207) are ported; the rest of the Go
/// struct (Timeout/Username/Password) is omitted since these handlers
/// never read it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Go `Type string \`json:"type"\`` — value `"url"` selects URL-mode
    /// proxying. Other values fall back to the default client.
    #[serde(default)]
    pub r#type: String,
    /// Go `URL string \`json:"url"\`` — proxy URL, used only when
    /// `type == "url"` and non-empty.
    #[serde(default)]
    pub url: String,
}

/// Mirrors Go `StartCodexOAuthRequest` / `StartClaudeCodeOAuthRequest`
/// (codex.go:43 / claudecode.go:43) — empty struct, accepted as `{}`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StartOAuthRequest {}

/// Mirrors Go `StartCodexOAuthResponse` / `StartClaudeCodeOAuthResponse`
/// (codex.go:45-48 / claudecode.go:45-48).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartOAuthResponse {
    pub session_id: String,
    pub auth_url: String,
}

/// Mirrors Go `ExchangeCodexOAuthRequest` / `ExchangeClaudeCodeOAuthRequest`
/// (codex.go:129-133 / claudecode.go:127-131) — `session_id` and
/// `callback_url` are `binding:"required"`; `proxy,omitempty`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ExchangeOAuthRequest {
    pub session_id: String,
    pub callback_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
}

/// Mirrors Go `ExchangeCodexOAuthResponse` / `ExchangeClaudeCodeOAuthResponse`
/// (codex.go:135-137 / claudecode.go:133-135).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExchangeOAuthResponse {
    pub credentials: String,
}

/// Mirrors Go `DecodeCodexAuthJSONRequest` (codex.go:139-141).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct DecodeAuthJsonRequest {
    pub auth_json: String,
}

/// Mirrors Go `DecodeCodexAuthJSONResponse` (codex.go:143-145).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecodeAuthJsonResponse {
    pub credentials: String,
}

/// Mirrors Go `StartAntigravityOAuthRequest` (antigravity.go:45-47) —
/// `ProjectID` is **not** `binding:"required"`; Go zero-fills it when absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct StartAntigravityOAuthRequest {
    /// Go `ProjectID string \`json:"project_id"\``. camelCase is preserved
    /// by `#[serde(rename_all = "camelCase")]` on the wrapping handler
    /// dispatch; we use the explicit tag here to be unambiguous.
    #[serde(default, rename = "project_id")]
    pub project_id: String,
}

/// Mirrors Go `StartAntigravityOAuthResponse` (antigravity.go:49-52). Wire
/// shape is identical to [`StartOAuthResponse`] but the Go type is distinct,
/// so we mirror it as a distinct Rust type for traceability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartAntigravityOAuthResponse {
    pub session_id: String,
    pub auth_url: String,
}

/// Mirrors Go `StartCopilotOAuthRequest` (copilot.go:140-142). Empty body
/// is allowed (copilot.go:159-166 — `EOF` is treated as the zero value).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct StartCopilotOAuthRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
}

/// Mirrors Go `StartCopilotOAuthResponse` (copilot.go:145-151).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartCopilotOAuthResponse {
    pub session_id: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Go `ExpiresIn int` — i64 to mirror the JSON number width the host
    /// serializes for `time.Duration`-derived counts.
    pub expires_in: i64,
    /// Go `Interval int` — polling interval in seconds.
    pub interval: i64,
}

/// Mirrors Go `PollCopilotOAuthRequest` (copilot.go:253-257). `SessionID` is
/// `binding:"required"`; `Proxy` is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PollCopilotOAuthRequest {
    #[serde(default, rename = "session_id")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyConfig>,
}

/// Mirrors Go `PollCopilotOAuthResponse` (copilot.go:260-266). All fields
/// except `status` are `omitempty` in Go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PollCopilotOAuthResponse {
    #[serde(
        default,
        rename = "access_token",
        skip_serializing_if = "Option::is_none"
    )]
    pub access_token: Option<String>,
    #[serde(
        default,
        rename = "token_type",
        skip_serializing_if = "Option::is_none"
    )]
    pub token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Status variants emitted by Go's `PollOAuth` (copilot.go:308-355) — captured
/// here so the host service can return a strongly-typed result that the
/// handler renders into [`PollCopilotOAuthResponse`] without owning the
/// `authorization_pending` / `slow_down` / `expired_token` / `access_denied`
/// error-string mapping itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopilotPollStatus {
    /// `authorization_pending` (copilot.go:310-315) — 200 `{"status":"pending"}`.
    Pending,
    /// `slow_down` (copilot.go:316-321) — 200 `{"status":"slow_down"}`.
    SlowDown,
    /// Success (copilot.go:337-352) — 200 `{"status":"complete",...}`.
    Complete {
        access_token: String,
        token_type: String,
        scope: String,
    },
}

/// Service-layer error category — captures the per-call-site status code the
/// Go handlers hand to `JSONError` (api/error.go:12-20 → wire shape
/// `{"error":{"type":"<StatusText>","message":"<msg>"}}`):
///
/// | variant      | status | call sites (codex.go / claudecode.go) |
/// |--------------|--------|---------------------------------------|
/// | `BadRequest` | 400    | invalid/expired session (217/186), callback URL parse (227/196), state mismatch (232/201), decode auth json (184) |
/// | `Internal`   | 500    | state/verifier/cache failures (95/95, 101/101, 109/109), credentials encode (190/229, 259) |
/// | `BadGateway` | 502    | token exchange failure (253/223) |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OAuthServerError {
    BadRequest(String),
    Internal(String),
    BadGateway(String),
}

impl OAuthServerError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::BadGateway(_) => StatusCode::BAD_GATEWAY,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::BadRequest(m) | Self::Internal(m) | Self::BadGateway(m) => m.clone(),
        }
    }
}

impl std::fmt::Display for OAuthServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for OAuthServerError {}

/// Minimal OAuth-admin service consumed by the admin OAuth handlers. Stands
/// in for the two Go handler structs (`CodexHandlers` codex.go:31-41 +
/// `ClaudeCodeHandlers` claudecode.go:31-41) which own:
/// * `xcache.Cache[T]` — the PKCE/state cache (`stateCache`),
/// * `*httpclient.HttpClient` — the outbound HTTP client used for the
///   token endpoint Exchange call (`httpClient`).
///
/// The trait surface is exactly the union of the gin-handler call sites:
/// * [`start_oauth`](Self::start_oauth) — generate state + PKCE verifier,
///   store them in the TTL cache (`xcache.WithExpiration(10*time.Minute)`,
///   codex.go:108 / claudecode.go:108), and build the provider-specific
///   authorize URL.
/// * [`exchange`](Self::exchange) — fetch the cached state, parse
///   `callback_url` (`parseCodexCallbackURL` codex.go:147-171 /
///   `parseClaudeCodeCallbackURL` claudecode.go:137-169 — note Claude
///   puts state in URL fragment first), enforce `state == session_id`,
///   call `TokenProvider.Exchange`, return credentials JSON via
///   `OAuthCredentials.ToJSON`.
/// * [`decode_auth_json`](Self::decode_auth_json) — Codex-only, calls
///   `codex.DecodeAuthJSON` then `creds.ToJSON`.
///
/// **Documented wiring gaps** (HTTP-client/cache/token-URL decisions live
/// behind this trait):
/// * real TTL cache port — currently the host bridges to a concrete impl
///   (the codex.go/claudecode.go `xcache.Cache[codexOAuthState]` seam);
/// * proxy HTTP client wiring (codex.go:237-240 / claudecode.go:206-209
///   branch on `req.Proxy.Type == "url" && req.Proxy.URL != ""`);
/// * JWT-protected admin group middleware (Go `middleware.WithJWTAuth`,
///   routes.go:96) — these handlers preserve the route-table entry but
///   the Rust middleware port is a separate gap.
#[async_trait]
pub trait OAuthAdminService: Send + Sync {
    /// POST `/admin/{provider}/oauth/start`.
    async fn start_oauth(
        &self,
        provider: OAuthProvider,
    ) -> Result<StartOAuthResponse, OAuthServerError>;

    /// POST `/admin/{provider}/oauth/exchange`. Returns the credentials
    /// JSON string (already `OAuthCredentials.ToJSON`-encoded).
    async fn exchange(
        &self,
        provider: OAuthProvider,
        request: ExchangeOAuthRequest,
    ) -> Result<String, OAuthServerError>;

    /// POST `/admin/codex/auth/decode` (Codex-only). Returns the
    /// credentials JSON string.
    async fn decode_auth_json(&self, auth_json: String) -> Result<String, OAuthServerError>;

    /// POST `/admin/antigravity/oauth/start`. Mirrors Go
    /// `AntigravityHandlers.StartOAuth` (antigravity.go:89-138): mint PKCE
    /// state, cache it (10 min TTL), and build the Google authorize URL.
    /// `project_id` is carried through to the cached state so the exchange
    /// step can fall back to it (antigravity.go:232-240).
    async fn start_antigravity_oauth(
        &self,
        project_id: String,
    ) -> Result<StartAntigravityOAuthResponse, OAuthServerError>;

    /// POST `/admin/antigravity/oauth/exchange`. Mirrors Go
    /// `AntigravityHandlers.Exchange` (antigravity.go:178-247): fetch cached
    /// state, parse callback URL (query-only — `code` + `state`), call
    /// Google's token endpoint, then either reuse the cached `project_id` or
    /// resolve one via `resolveProjectID` + `onboardUser`
    /// (antigravity.go:249-405). Returns the raw credentials string in the
    /// `refreshToken|projectId` format (antigravity.go:243-246).
    async fn exchange_antigravity(
        &self,
        request: ExchangeOAuthRequest,
    ) -> Result<String, OAuthServerError>;

    /// POST `/admin/copilot/oauth/start`. Mirrors Go
    /// `CopilotHandlers.StartOAuth` (copilot.go:155-215): mint session ID,
    /// call GitHub's device-code endpoint, cache the device-flow state, and
    /// return the user_code/verification_uri/expires_in/interval quad. Empty
    /// body is allowed (copilot.go:159-166 — `EOF` is treated as the zero
    /// request).
    async fn start_copilot_oauth(
        &self,
        proxy: Option<ProxyConfig>,
    ) -> Result<StartCopilotOAuthResponse, OAuthServerError>;

    /// POST `/admin/copilot/oauth/poll`. Mirrors Go
    /// `CopilotHandlers.PollOAuth` (copilot.go:270-356): look up cached
    /// device-flow state, enforce expiry (copilot.go:288-292), poll GitHub's
    /// access-token endpoint, and surface the status enum
    /// ([`CopilotPollStatus`]). Cache cleanup on terminal states is owned by
    /// the implementation (mirrors copilot.go:324/328/340).
    async fn poll_copilot_oauth(
        &self,
        request: PollCopilotOAuthRequest,
    ) -> Result<CopilotPollStatus, OAuthServerError>;
}

/// POST `/admin/{provider}/oauth/start` — dispatcher for the four Go
/// provider-specific StartOAuth handlers:
/// * `CodexHandlers.StartOAuth`        (codex.go:84-127)
/// * `ClaudeCodeHandlers.StartOAuth`   (claudecode.go:84-125)
/// * `AntigravityHandlers.StartOAuth`  (antigravity.go:89-138)
/// * `CopilotHandlers.StartOAuth`      (copilot.go:155-215)
///
/// Provider-specific bind + service-call rules:
/// | provider    | body shape                                  | empty body? | service call |
/// |-------------|---------------------------------------------|-------------|--------------|
/// | codex       | `{}` empty object                           | 400 (EOF)   | [`OAuthAdminService::start_oauth`] |
/// | claudecode  | `{}` empty object                           | 400 (EOF)   | [`OAuthAdminService::start_oauth`] |
/// | antigravity | `{"project_id":"..."}` (project_id optional)| 400 (EOF)   | [`OAuthAdminService::start_antigravity_oauth`] |
/// | copilot     | `{}` or `{"proxy":{...}}`                   | 200 (Go treats EOF as zero value, copilot.go:159-166) | [`OAuthAdminService::start_copilot_oauth`] |
pub async fn start_oauth(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let Some(provider) = parse_provider(&provider) else {
        return json_error(StatusCode::NOT_FOUND, "unknown oauth provider");
    };

    let Some(service) = state.services().oauth_admin_service() else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "oauth service is not configured",
        );
    };

    let bytes = match body {
        Ok(bytes) => bytes,
        Err(_) => {
            // BytesRejection maps to a malformed body. For codex/claudecode/
            // antigravity this is a 400 "invalid request format". For copilot,
            // Go treats empty body (EOF) as the zero value
            // (copilot.go:159-166) — but a BytesRejection is a different
            // failure mode than EOF on the JSON decoder, so we still map it
            // to 400 for copilot too. The copilot EOF escape hatch is in the
            // JSON-bind branch below.
            if provider == OAuthProvider::Copilot {
                // Empty-body allowed: dispatch with no proxy.
                return match service.start_copilot_oauth(None).await {
                    Ok(response) => (StatusCode::OK, Json(response)).into_response(),
                    Err(err) => json_error(err.status(), err.message()),
                };
            }
            return json_error(StatusCode::BAD_REQUEST, "invalid request format");
        }
    };

    match provider {
        OAuthProvider::Codex | OAuthProvider::ClaudeCode => {
            // gin ShouldBindJSON on empty body → EOF → 400 (codex.go:88-91).
            if bind_empty_object(&bytes).is_err() {
                return json_error(StatusCode::BAD_REQUEST, "invalid request format");
            }
            match service.start_oauth(provider).await {
                Ok(response) => (StatusCode::OK, Json(response)).into_response(),
                Err(err) => json_error(err.status(), err.message()),
            }
        }
        OAuthProvider::Antigravity => {
            // antigravity.go:92-96 — ShouldBindJSON; EOF and decode errors
            // both yield 400 "invalid request format". `project_id` is NOT
            // binding:required (Go zero-fills it).
            let request: StartAntigravityOAuthRequest = match serde_json::from_slice(&bytes) {
                Ok(request) => request,
                Err(_) => {
                    return json_error(StatusCode::BAD_REQUEST, "invalid request format");
                }
            };
            match service.start_antigravity_oauth(request.project_id).await {
                Ok(response) => (StatusCode::OK, Json(response)).into_response(),
                Err(err) => json_error(err.status(), err.message()),
            }
        }
        OAuthProvider::Copilot => {
            // copilot.go:159-166 — ShouldBindJSON; *only* the exact EOF
            // error short-circuits to the zero value. Any other decode
            // failure → 400 "invalid request format".
            if bytes.is_empty() {
                return match service.start_copilot_oauth(None).await {
                    Ok(response) => (StatusCode::OK, Json(response)).into_response(),
                    Err(err) => json_error(err.status(), err.message()),
                };
            }
            let request: StartCopilotOAuthRequest = match serde_json::from_slice(&bytes) {
                Ok(request) => request,
                Err(_) => {
                    return json_error(StatusCode::BAD_REQUEST, "invalid request format");
                }
            };
            match service.start_copilot_oauth(request.proxy).await {
                Ok(response) => (StatusCode::OK, Json(response)).into_response(),
                Err(err) => json_error(err.status(), err.message()),
            }
        }
    }
}

/// POST `/admin/{provider}/oauth/exchange` — Go `CodexHandlers.Exchange`
/// (codex.go:199-264) + `ClaudeCodeHandlers.Exchange`
/// (claudecode.go:173-234).
///
/// | condition                          | status | body message |
/// |------------------------------------|--------|--------------|
/// | malformed JSON / missing required  | 400    | "invalid request format" (codex.go:203-206 / claudecode.go:177-180) |
/// | empty session/callback             | 400    | "session_id and callback_url are required" (codex.go:208-211) |
/// | cache miss                          | 400    | "invalid or expired oauth session" (codex.go:217 / claudecode.go:186) |
/// | callback URL parse failure          | 400    | err.Error() verbatim (codex.go:227 / claudecode.go:196) |
/// | state mismatch                       | 400    | "oauth state mismatch" (codex.go:232 / claudecode.go:201) |
/// | token endpoint failure              | 502    | "token exchange failed: <err>" (codex.go:253 / claudecode.go:223) |
/// | credentials encode failure           | 500    | "failed to encode credentials: <err>" (codex.go:259 / claudecode.go:229) |
/// | success                              | 200    | `{"credentials":"<json>"}` |
pub async fn exchange(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let bytes = match body {
        Ok(bytes) => bytes,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid request format"),
    };

    let request: ExchangeOAuthRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid request format"),
    };

    // codex.go:208-211 — explicit empty guard. claudecode.go relies on the
    // gin `binding:"required"` tag which yields the same 400 outcome (the
    // shared handler treats both paths identically). antigravity.go also
    // uses `binding:"required"` (antigravity.go:141-144).
    if request.session_id.is_empty() || request.callback_url.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "session_id and callback_url are required",
        );
    }

    let Some(provider) = parse_provider(&provider) else {
        return json_error(StatusCode::NOT_FOUND, "unknown oauth provider");
    };

    let Some(service) = state.services().oauth_admin_service() else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "oauth service is not configured",
        );
    };

    // Antigravity has a distinct service method (`exchange_antigravity`) — its
    // response format is `refreshToken|projectId` rather than OAuthCredentials
    // JSON (antigravity.go:243-246), and it owns project-ID resolution.
    let credentials = if provider == OAuthProvider::Antigravity {
        match service.exchange_antigravity(request).await {
            Ok(credentials) => credentials,
            Err(err) => return json_error(err.status(), err.message()),
        }
    } else {
        match service.exchange(provider, request).await {
            Ok(credentials) => credentials,
            Err(err) => return json_error(err.status(), err.message()),
        }
    };

    (StatusCode::OK, Json(ExchangeOAuthResponse { credentials })).into_response()
}

/// POST `/admin/codex/auth/decode` — Go `CodexHandlers.DecodeAuthJSON`
/// (codex.go:175-195). Claude Code has no equivalent route.
///
/// | condition                       | status | body message |
/// |---------------------------------|--------|--------------|
/// | malformed JSON / missing field  | 400    | "invalid request format" (codex.go:177-180) |
/// | `codex.DecodeAuthJSON` failure  | 400    | "failed to decode auth json: <err>" (codex.go:184) |
/// | `creds.ToJSON` failure          | 500    | "failed to encode credentials: <err>" (codex.go:190) |
/// | success                         | 200    | `{"credentials":"<json>"}` |
pub async fn decode_auth_json(
    State(state): State<AppState>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let bytes = match body {
        Ok(bytes) => bytes,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid request format"),
    };

    let request: DecodeAuthJsonRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid request format"),
    };

    // gin `binding:"required"` rejects an empty `AuthJSON` (validator
    // `required` tag fires on the zero string). JSONError → 400 "invalid
    // request format" (codex.go:177-180).
    if request.auth_json.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "invalid request format");
    }

    let Some(service) = state.services().oauth_admin_service() else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "oauth service is not configured",
        );
    };

    match service.decode_auth_json(request.auth_json).await {
        Ok(credentials) => {
            (StatusCode::OK, Json(DecodeAuthJsonResponse { credentials })).into_response()
        }
        Err(err) => json_error(err.status(), err.message()),
    }
}

/// POST `/admin/copilot/oauth/poll` — Go `CopilotHandlers.PollOAuth`
/// (copilot.go:270-356). Polls GitHub's device-flow access-token endpoint
/// for a session started by [`start_oauth`] and renders the polling
/// status enum emitted by the service layer.
///
/// | condition                                  | status | body shape |
/// |--------------------------------------------|--------|------------|
/// | malformed JSON / missing session_id        | 400    | `{"error":{"type":"Bad Request","message":"invalid request format"}}` (copilot.go:273-277) |
/// | cache miss / unknown session               | 400    | `... "invalid or expired session"` (copilot.go:281-285) |
/// | device code expired                        | 400    | `... "device code expired"` (copilot.go:288-292, 323-326) |
/// | access denied by user                      | 400    | `... "access denied by user"` (copilot.go:327-330) |
/// | token-poll transport/HTTP failure          | 502    | `... "token poll failed: <err>"` (copilot.go:301-305) |
/// | other OAuth error from GitHub              | 502    | `... "OAuth error: <err> - <desc>"` (copilot.go:331-334) |
/// | unexpected empty token                     | 500    | `... "unexpected response from GitHub"` (copilot.go:354-355) |
/// | `authorization_pending`                    | 200    | `{"status":"pending","message":"Authorization pending..."}` (copilot.go:310-315) |
/// | `slow_down`                                | 200    | `{"status":"slow_down","message":"Polling too fast..."}` (copilot.go:316-321) |
/// | success                                    | 200    | `{"access_token":"...","token_type":"...","scope":"...","status":"complete","message":"..."}` (copilot.go:337-352) |
pub async fn poll_oauth(
    State(state): State<AppState>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    let bytes = match body {
        Ok(bytes) => bytes,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid request format"),
    };

    // copilot.go:273-277 — ShouldBindJSON; any error (incl. EOF) → 400.
    let request: PollCopilotOAuthRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid request format"),
    };

    // copilot.go:254-256 — gin `binding:"required"` rejects empty SessionID.
    if request.session_id.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "invalid request format");
    }

    let Some(service) = state.services().oauth_admin_service() else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "oauth service is not configured",
        );
    };

    let outcome = match service.poll_copilot_oauth(request).await {
        Ok(status) => status,
        Err(err) => return json_error(err.status(), err.message()),
    };

    let response = match outcome {
        CopilotPollStatus::Pending => PollCopilotOAuthResponse {
            access_token: None,
            token_type: None,
            scope: None,
            status: "pending".to_string(),
            message: Some(
                "Authorization pending. User has not yet authorized the device.".to_string(),
            ),
        },
        CopilotPollStatus::SlowDown => PollCopilotOAuthResponse {
            access_token: None,
            token_type: None,
            scope: None,
            status: "slow_down".to_string(),
            message: Some("Polling too fast. Please slow down.".to_string()),
        },
        CopilotPollStatus::Complete {
            access_token,
            token_type,
            scope,
        } => PollCopilotOAuthResponse {
            access_token: Some(access_token),
            token_type: Some(token_type),
            scope: Some(scope),
            status: "complete".to_string(),
            message: Some("Authorization complete. Access token received.".to_string()),
        },
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// gin `ShouldBindJSON` for the empty `StartOAuthRequest` struct
/// (codex.go:87-91 / claudecode.go:87-91). gin's JSON decoder (Go
/// `encoding/json`) on an empty body returns `EOF`; on a non-JSON body it
/// returns a decoder error; both map to JSONError 400 "invalid request
/// format" at the call site. Unknown fields are silently ignored (gin's
/// default), so `{"anything":1}` binds successfully.
fn bind_empty_object(bytes: &[u8]) -> Result<(), ()> {
    if bytes.is_empty() {
        return Err(());
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    if !value.is_object() {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_start_routes_for_all_providers() {
        for (path, provider) in [
            ("/admin/codex/oauth/start", OAuthProvider::Codex),
            ("/admin/claudecode/oauth/start", OAuthProvider::ClaudeCode),
            ("/admin/antigravity/oauth/start", OAuthProvider::Antigravity),
            ("/admin/copilot/oauth/start", OAuthProvider::Copilot),
        ] {
            assert_eq!(
                parse_admin_oauth_route(path),
                Some(AdminOAuthRoute::Start { provider }),
                "{path}"
            );
        }
    }

    #[test]
    fn parses_exchange_routes_except_copilot() {
        for (path, provider) in [
            ("/admin/codex/oauth/exchange", OAuthProvider::Codex),
            (
                "/admin/claudecode/oauth/exchange",
                OAuthProvider::ClaudeCode,
            ),
            (
                "/admin/antigravity/oauth/exchange",
                OAuthProvider::Antigravity,
            ),
        ] {
            assert_eq!(
                parse_admin_oauth_route(path),
                Some(AdminOAuthRoute::Exchange { provider }),
                "{path}"
            );
        }
    }

    #[test]
    fn parses_copilot_poll_route() {
        assert_eq!(
            parse_admin_oauth_route("/admin/copilot/oauth/poll"),
            Some(AdminOAuthRoute::Poll {
                provider: OAuthProvider::Copilot
            })
        );
    }

    #[test]
    fn parses_codex_auth_decode_route() {
        assert_eq!(
            parse_admin_oauth_route("/admin/codex/auth/decode"),
            Some(AdminOAuthRoute::DecodeAuthJson)
        );
    }

    #[test]
    fn ignores_query_string() {
        assert_eq!(
            parse_admin_oauth_route("/admin/copilot/oauth/poll?state=abc"),
            Some(AdminOAuthRoute::Poll {
                provider: OAuthProvider::Copilot
            })
        );
    }

    #[test]
    fn rejects_legacy_oauth_shape() {
        // The old (incorrect) shape must not be recognized.
        for path in [
            "/oauth/codex/start",
            "/oauth/claude-code/start",
            "/oauth/copilot/poll",
            "/oauth/codex/decode-auth-json",
            "/admin/oauth/codex/start",
        ] {
            assert_eq!(parse_admin_oauth_route(path), None, "{path}");
        }
    }

    #[test]
    fn rejects_missing_admin_prefix() {
        for path in [
            "codex/oauth/start",
            "claudecode/oauth/exchange",
            "/codex/oauth/start",
            "copilot/oauth/poll",
        ] {
            assert_eq!(parse_admin_oauth_route(path), None, "{path}");
        }
    }

    #[test]
    fn rejects_unknown_provider() {
        for path in [
            "/admin/unknown/oauth/start",
            "/admin/codex-code/oauth/start",
            "/admin/Codex/oauth/start",
            "/admin/claude-code/oauth/start", // wrong spelling (hyphenated)
        ] {
            assert_eq!(parse_admin_oauth_route(path), None, "{path}");
        }
    }

    #[test]
    fn rejects_provider_action_mismatch() {
        // Copilot has no exchange route.
        assert_eq!(
            parse_admin_oauth_route("/admin/copilot/oauth/exchange"),
            None
        );
        // Codex/ClaudeCode/Antigravity have no poll route.
        assert_eq!(parse_admin_oauth_route("/admin/codex/oauth/poll"), None);
        assert_eq!(
            parse_admin_oauth_route("/admin/claudecode/oauth/poll"),
            None
        );
        assert_eq!(
            parse_admin_oauth_route("/admin/antigravity/oauth/poll"),
            None
        );
    }

    #[test]
    fn rejects_decode_route_for_non_codex_providers() {
        for path in [
            "/admin/claudecode/auth/decode",
            "/admin/antigravity/auth/decode",
            "/admin/copilot/auth/decode",
        ] {
            assert_eq!(parse_admin_oauth_route(path), None, "{path}");
        }
    }

    #[test]
    fn rejects_trailing_extra_segments() {
        for path in [
            "/admin/codex/oauth/start/extra",
            "/admin/codex/auth/decode/extra",
            "/admin/codex/oauth/start/",
        ] {
            assert_eq!(parse_admin_oauth_route(path), None, "{path}");
        }
    }

    #[test]
    fn rejects_unknown_action_under_oauth() {
        for path in [
            "/admin/codex/oauth/refresh",
            "/admin/copilot/oauth/decode",
            "/admin/codex/oauth/decode",
        ] {
            assert_eq!(parse_admin_oauth_route(path), None, "{path}");
        }
    }

    #[test]
    fn rejects_wrong_group_segment() {
        for path in [
            "/admin/codex/authz/decode", // group != "auth"
            "/admin/codex/oauth-auth/decode",
            "/admin/copilot/oauth/polls", // typo of poll
        ] {
            assert_eq!(parse_admin_oauth_route(path), None, "{path}");
        }
    }

    #[test]
    fn rejects_garbage_and_empty() {
        for path in ["", "/", "/admin", "/admin/codex", "/admin/codex/oauth"] {
            assert_eq!(parse_admin_oauth_route(path), None, "{path}");
        }
    }

    // ---- Handler-body tests (RUST-P11-003 S08) ----------------------------
    //
    // The InMemory service stands in for the host wiring (real TTL cache +
    // httpclient live behind the trait). Tests focus on the HTTP-layer
    // decisions the handler bodies own: bind-error mapping, status-code
    // mapping per Go call site, and the JSON response shape. Provider
    // constants (CLIENT_ID, AUTHORIZE_URL, etc.) are mirrored verbatim
    // from `llm/transformer/openai/codex/constants.go` and
    // `llm/transformer/anthropic/claudecode/constants.go`.

    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, header};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tower::Service;

    use crate::app_state::AppServices;
    use crate::router::build_router;

    const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
    const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
    const CODEX_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
    const CODEX_SCOPES: &str = "openid profile email offline_access";

    const CLAUDE_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
    const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
    const CLAUDE_REDIRECT_URI: &str = "http://localhost:54545/callback";
    const CLAUDE_SCOPES: &str = "org:create_api_key user:profile user:inference";

    /// `base64.URLEncoding.WithPadding(base64.NoPadding)` — the exact
    /// encoding Go uses for state + code_verifier + code_challenge
    /// (codex.go:55-76 / claudecode.go:55-76).
    fn b64url_nopad(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            let triple = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
            out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
            if chunk.len() >= 2 {
                out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
            }
            if chunk.len() >= 3 {
                out.push(ALPHABET[(triple & 0x3F) as usize] as char);
            }
        }
        out
    }

    /// `url.Values.Encode()` — keys sorted alphabetically, key/value
    /// query-escaped, joined by `&`. Reuses the verbatim `query_escape`
    /// from `oidc_handlers` for byte parity with Go.
    fn encode_query(pairs: &[(&str, &str)]) -> String {
        let mut sorted: Vec<&(&str, &str)> = pairs.iter().collect();
        sorted.sort_by_key(|(k, _)| *k);
        sorted
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    crate::oidc_handlers::query_escape(k),
                    crate::oidc_handlers::query_escape(v)
                )
            })
            .collect::<Vec<_>>()
            .join("&")
    }

    /// Codex authorize URL — codex.go:113-124 (note the two Codex-only
    /// extras `id_token_add_organizations=true` +
    /// `codex_cli_simplified_flow=true`).
    fn codex_authorize_url(state: &str, challenge: &str) -> String {
        let pairs: &[(&str, &str)] = &[
            ("response_type", "code"),
            ("client_id", CODEX_CLIENT_ID),
            ("redirect_uri", CODEX_REDIRECT_URI),
            ("scope", CODEX_SCOPES),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
        ];
        format!("{CODEX_AUTHORIZE_URL}?{}", encode_query(pairs))
    }

    /// Claude Code authorize URL — claudecode.go:113-122 (no Codex-only
    /// extras).
    fn claude_authorize_url(state: &str, challenge: &str) -> String {
        let pairs: &[(&str, &str)] = &[
            ("response_type", "code"),
            ("client_id", CLAUDE_CLIENT_ID),
            ("redirect_uri", CLAUDE_REDIRECT_URI),
            ("scope", CLAUDE_SCOPES),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
        ];
        format!("{CLAUDE_AUTHORIZE_URL}?{}", encode_query(pairs))
    }

    /// Antigravity authorize URL constants — verbatim from
    /// `llm/transformer/antigravity/constants.go:8,13,15,34,54`.
    const ANTIGRAVITY_CLIENT_ID: &str =
        "REMOVED_GOOGLE_OAUTH_CLIENT_ID";
    const ANTIGRAVITY_REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
    const ANTIGRAVITY_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
    const ANTIGRAVITY_SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
    /// Go `antigravity.DefaultProjectID` (constants.go:34) — fallback when
    /// neither the request body nor `resolveProjectID` produces a project.
    const ANTIGRAVITY_DEFAULT_PROJECT_ID: &str = "rising-fact-p41fc";

    /// Antigravity authorize URL — antigravity.go:124-135. Note the three
    /// Antigravity-only params (`access_type=offline`, `prompt=consent`, plus
    /// the standard PKCE set).
    fn antigravity_authorize_url(state: &str, challenge: &str) -> String {
        let pairs: &[(&str, &str)] = &[
            ("response_type", "code"),
            ("client_id", ANTIGRAVITY_CLIENT_ID),
            ("redirect_uri", ANTIGRAVITY_REDIRECT_URI),
            ("scope", ANTIGRAVITY_SCOPES),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("access_type", "offline"),
            ("prompt", "consent"),
        ];
        format!("{ANTIGRAVITY_AUTHORIZE_URL}?{}", encode_query(pairs))
    }

    /// Minimal in-memory OAuthAdminService. Mirrors the state-management
    /// shape of Go `CodexHandlers`/`ClaudeCodeHandlers` (stateCache +
    /// deterministic verifier) without performing real HTTP token
    /// exchange. The exchange returns a canned credentials JSON shape
    /// (Go `OAuthCredentials.ToJSON` minimal subset).
    #[derive(Default)]
    struct InMemoryOAuthService {
        counter: Mutex<u64>,
        sessions: Mutex<HashMap<String, String>>, // session_id -> verifier
        fail_exchange_upstream: bool,
        fail_decode: bool,
        fail_encode: bool,
        seen_exchange: Mutex<Option<ExchangeOAuthRequest>>,
        seen_decode: Mutex<Option<String>>,
        // Antigravity + Copilot stub extensions.
        /// session_id -> project_id (antigravity StartOAuth input).
        antigravity_sessions: Mutex<HashMap<String, String>>,
        /// session_id -> () sentinel for copilot device-flow state.
        copilot_sessions: Mutex<HashMap<String, ()>>,
        /// Status the copilot poll handler should return.
        copilot_poll_status: Option<CopilotPollStatus>,
        /// When true, start_copilot_oauth returns 502 (mirrors Go
        /// device-code endpoint failure).
        fail_copilot_device_code: bool,
        /// When true, start_antigravity_oauth returns 500 (mirrors Go
        /// state/verifier/cache failure).
        fail_antigravity_start: bool,
        /// Records the most recent antigravity exchange request.
        seen_antigravity_exchange: Mutex<Option<ExchangeOAuthRequest>>,
    }

    impl InMemoryOAuthService {
        fn next(&self, prefix: &str) -> String {
            let mut guard = match self.counter.lock() {
                Ok(g) => g,
                // Test-only path; the host never poisons the mutex.
                Err(_) => return String::new(),
            };
            *guard += 1;
            format!("{prefix}-{}", *guard)
        }
    }

    #[async_trait::async_trait]
    impl OAuthAdminService for InMemoryOAuthService {
        async fn start_oauth(
            &self,
            provider: OAuthProvider,
        ) -> Result<StartOAuthResponse, OAuthServerError> {
            let (prefix, builder): (&str, fn(&str, &str) -> String) = match provider {
                OAuthProvider::Codex => ("codex-state", codex_authorize_url),
                OAuthProvider::ClaudeCode => ("claude-state", claude_authorize_url),
                OAuthProvider::Antigravity | OAuthProvider::Copilot => {
                    return Err(OAuthServerError::Internal(
                        "provider not implemented in test stub".to_string(),
                    ));
                }
            };
            let state = self.next(prefix);
            let verifier = self.next("verifier");
            let challenge = b64url_nopad(&Sha256::digest(verifier.as_bytes()));
            if let Ok(mut sessions) = self.sessions.lock() {
                sessions.insert(state.clone(), verifier);
            }
            Ok(StartOAuthResponse {
                session_id: state.clone(),
                auth_url: builder(&state, &challenge),
            })
        }

        async fn exchange(
            &self,
            provider: OAuthProvider,
            request: ExchangeOAuthRequest,
        ) -> Result<String, OAuthServerError> {
            if let Ok(mut seen) = self.seen_exchange.lock() {
                *seen = Some(request.clone());
            }
            // codex.go:213-219 — session must exist (cache.Get error → 400).
            let verifier = match self.sessions.lock() {
                Ok(mut sessions) => sessions.remove(&request.session_id),
                Err(_) => None,
            };
            let verifier = verifier.ok_or_else(|| {
                OAuthServerError::BadRequest("invalid or expired oauth session".to_string())
            })?;

            // codex.go:225-234 — parse callback URL + state-match guard.
            let (_code, callback_state) = parse_callback(&request.callback_url)
                .map_err(|err| OAuthServerError::BadRequest(err.to_string()))?;
            if callback_state != request.session_id {
                return Err(OAuthServerError::BadRequest("oauth state mismatch".into()));
            }

            if self.fail_exchange_upstream {
                return Err(OAuthServerError::BadGateway(
                    "token exchange failed: 502 from provider".to_string(),
                ));
            }

            let client_id = match provider {
                OAuthProvider::Codex => CODEX_CLIENT_ID,
                OAuthProvider::ClaudeCode => CLAUDE_CLIENT_ID,
                _ => "",
            };

            if self.fail_encode {
                return Err(OAuthServerError::Internal(
                    "failed to encode credentials: mock".to_string(),
                ));
            }

            // Mirrors the wire shape of OAuthCredentials.ToJSON — minimal
            // subset sufficient for parity asserts.
            Ok(format!(
                r#"{{"client_id":"{client_id}","access_token":"mock-token-{verifier}","refresh_token":"","expires_at":"0001-01-01T00:00:00Z","scopes":["openid"]}}"#
            ))
        }

        async fn decode_auth_json(&self, auth_json: String) -> Result<String, OAuthServerError> {
            if let Ok(mut seen) = self.seen_decode.lock() {
                *seen = Some(auth_json.clone());
            }
            if self.fail_decode {
                return Err(OAuthServerError::BadRequest(
                    "failed to decode auth json: mock".to_string(),
                ));
            }
            if self.fail_encode {
                return Err(OAuthServerError::Internal(
                    "failed to encode credentials: mock".to_string(),
                ));
            }
            // Echo a minimal credentials JSON acknowledging the input.
            Ok(format!(
                r#"{{"client_id":"{CODEX_CLIENT_ID}","access_token":"decoded","refresh_token":""}}"#
            ))
        }

        async fn start_antigravity_oauth(
            &self,
            project_id: String,
        ) -> Result<StartAntigravityOAuthResponse, OAuthServerError> {
            if self.fail_antigravity_start {
                return Err(OAuthServerError::Internal(
                    "failed to save oauth state: mock".to_string(),
                ));
            }
            let state = self.next("antigravity-state");
            if let Ok(mut sessions) = self.antigravity_sessions.lock() {
                sessions.insert(state.clone(), project_id.clone());
            }
            Ok(StartAntigravityOAuthResponse {
                session_id: state.clone(),
                auth_url: antigravity_authorize_url(&state, "mock-challenge"),
            })
        }

        async fn exchange_antigravity(
            &self,
            request: ExchangeOAuthRequest,
        ) -> Result<String, OAuthServerError> {
            if let Ok(mut seen) = self.seen_antigravity_exchange.lock() {
                *seen = Some(request.clone());
            }
            // antigravity.go:189-193 — cache miss → 400.
            let project_id = match self.antigravity_sessions.lock() {
                Ok(mut sessions) => sessions.remove(&request.session_id),
                Err(_) => None,
            };
            let project_id = project_id.ok_or_else(|| {
                OAuthServerError::BadRequest("invalid or expired oauth session".to_string())
            })?;

            // antigravity.go:195-204 — callback URL parse + state match.
            let (_code, callback_state) = parse_callback(&request.callback_url)
                .map_err(|err| OAuthServerError::BadRequest(err.to_string()))?;
            if callback_state != request.session_id {
                return Err(OAuthServerError::BadRequest("oauth state mismatch".into()));
            }

            if self.fail_exchange_upstream {
                return Err(OAuthServerError::BadGateway(
                    "token exchange failed: mock".to_string(),
                ));
            }

            // antigravity.go:243-246 — `refreshToken|projectId` format.
            // If the cached project_id is empty, fall back to the antigravity
            // default (mirrors antigravity.DefaultProjectID).
            let resolved_project = if project_id.is_empty() {
                ANTIGRAVITY_DEFAULT_PROJECT_ID
            } else {
                project_id.as_str()
            };
            Ok(format!("mock-refresh-token|{resolved_project}"))
        }

        async fn start_copilot_oauth(
            &self,
            _proxy: Option<ProxyConfig>,
        ) -> Result<StartCopilotOAuthResponse, OAuthServerError> {
            if self.fail_copilot_device_code {
                return Err(OAuthServerError::BadGateway(
                    "failed to request device code: mock".to_string(),
                ));
            }
            let session_id = self.next("copilot-session");
            if let Ok(mut sessions) = self.copilot_sessions.lock() {
                sessions.insert(session_id.clone(), ());
            }
            Ok(StartCopilotOAuthResponse {
                session_id,
                user_code: "MOCK-CODE".to_string(),
                verification_uri: "https://github.com/login/device".to_string(),
                expires_in: 900,
                interval: 5,
            })
        }

        async fn poll_copilot_oauth(
            &self,
            request: PollCopilotOAuthRequest,
        ) -> Result<CopilotPollStatus, OAuthServerError> {
            // copilot.go:281-285 — cache miss → 400.
            let exists = match self.copilot_sessions.lock() {
                Ok(sessions) => sessions.contains_key(&request.session_id),
                Err(_) => false,
            };
            if !exists {
                return Err(OAuthServerError::BadRequest(
                    "invalid or expired session".to_string(),
                ));
            }
            // copilot.go:288-292 — expiry check (mocked as non-expired).
            // copilot.go:301-305 — token-poll transport failure.
            if self.fail_exchange_upstream {
                return Err(OAuthServerError::BadGateway(
                    "token poll failed: mock".to_string(),
                ));
            }
            let status = self
                .copilot_poll_status
                .clone()
                .unwrap_or(CopilotPollStatus::Pending);
            // Mirror cache cleanup on terminal states (copilot.go:324/328/340).
            if matches!(status, CopilotPollStatus::Complete { .. })
                && let Ok(mut sessions) = self.copilot_sessions.lock()
            {
                sessions.remove(&request.session_id);
            }
            Ok(status)
        }
    }

    /// Subset of Go `parseCodexCallbackURL` / `parseClaudeCodeCallbackURL`
    /// sufficient for the InMemory stub: extracts `(code, state)` from
    /// either the query or the fragment (Claude style).
    fn parse_callback(callback_url: &str) -> Result<(String, String), String> {
        let trimmed = callback_url.trim();
        if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
            return Err("callback_url must be a full URL".to_string());
        }
        let after_q = trimmed.split_once('?').map(|(_, q)| q).unwrap_or("");
        let fragment = trimmed.split_once('#').map(|(_, f)| f).unwrap_or("");
        let mut code = String::new();
        let mut state = String::new();
        for segment in after_q.split('&') {
            if let Some((k, v)) = segment.split_once('=') {
                if k == "code" {
                    code = v.to_string();
                } else if k == "state" {
                    state = v.to_string();
                }
            }
        }
        // Claude-style: state lives in the fragment.
        if state.is_empty() && !fragment.is_empty() {
            state = fragment.to_string();
        }
        if code.is_empty() {
            return Err("code parameter not found in callback_url".to_string());
        }
        if state.is_empty() {
            return Err("state parameter not found in callback_url".to_string());
        }
        Ok((code, state))
    }

    /// Shared HS256 secret for the admin-group JWT guard in these tests.
    ///
    /// The admin OAuth routes live under Go's `adminGroup` which is wrapped in
    /// `middleware.WithJWTAuth(...)` (routes.go:96); the Rust router mirrors
    /// this by placing them behind the `jwt_admin_auth` middleware. The guard
    /// resolves its signing secret from `config.api_auth.jwt_secret` (the
    /// config fallback for tests that wire no live SystemService), so these
    /// fixtures set the same secret used by [`mint_admin_jwt`] to mint a valid
    /// bearer token.
    const TEST_JWT_SECRET: &str = "oauth-admin-test-secret";

    /// Mint a valid HS256 bearer token accepted by the admin JWT guard, signed
    /// with [`TEST_JWT_SECRET`] (mirrors the pattern in
    /// `middleware/jwt_auth.rs` tests). On the (unreachable in practice)
    /// encode error, returns an empty string so the middleware yields its
    /// 401 path rather than panicking.
    fn mint_admin_jwt() -> String {
        use conduit_auth::jwt::{Claims, encode_hs256};
        encode_hs256(&Claims::new(42, "user:42".to_string()), TEST_JWT_SECRET).unwrap_or_default()
    }

    fn app_with_oauth(service: Arc<InMemoryOAuthService>) -> Router {
        let mut config = conduit_config::AppConfig::default();
        // Wire the JWT secret the admin guard reads (routes.go:96 adminGroup).
        config.api_auth.jwt_secret = Some(TEST_JWT_SECRET.to_string());
        let services = AppServices::new().with_oauth_admin_service(service);
        build_router(crate::app_state::AppState::new(
            std::sync::Arc::new(config),
            std::sync::Arc::new(services),
        ))
    }

    async fn call_json(
        app: &mut Router,
        request: Request<Body>,
    ) -> Result<(axum::http::StatusCode, Value), Box<dyn std::error::Error>> {
        let response = app.call(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    fn post(uri: &str, body: &str) -> Result<Request<Body>, Box<dyn std::error::Error>> {
        // All these routes sit under Go's `adminGroup` JWT guard
        // (routes.go:96 — `middleware.WithJWTAuth`); the Rust router mounts
        // them behind `jwt_admin_auth`. Attach a valid bearer token minted
        // with the same secret `app_with_oauth` wires, so the request reaches
        // the OAuth handler instead of short-circuiting at the 401 guard.
        Ok(Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::HOST, "gateway.test")
            .header(header::CONTENT_TYPE, "application/json")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", mint_admin_jwt()),
            )
            .body(Body::from(body.to_string()))?)
    }

    /// Codex StartOAuth happy path (codex.go:84-127): 200 with the
    /// authorize URL carrying the two Codex-only extras + sorted query
    /// params (Go `url.Values.Encode()`).
    #[tokio::test]
    async fn codex_start_oauth_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) = call_json(&mut app, post("/admin/codex/oauth/start", "{}")?).await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        let session_id = body["session_id"].as_str().unwrap_or_default();
        let auth_url = body["auth_url"].as_str().unwrap_or_default();
        assert!(session_id.starts_with("codex-state-"), "{session_id}");
        assert!(
            auth_url.starts_with("https://auth.openai.com/oauth/authorize?"),
            "{auth_url}"
        );
        // Codex-only extras present.
        assert!(
            auth_url.contains("codex_cli_simplified_flow=true"),
            "{auth_url}"
        );
        assert!(
            auth_url.contains("id_token_add_organizations=true"),
            "{auth_url}"
        );
        // Sorted query: client_id precedes code_challenge.
        let cid_pos = auth_url.find("client_id=").unwrap_or(0);
        let cc_pos = auth_url.find("code_challenge=").unwrap_or(0);
        assert!(
            cid_pos < cc_pos,
            "Go url.Values.Encode sorts keys: {auth_url}"
        );
        // session_id appears in state param.
        assert!(
            auth_url.contains(&format!("state={session_id}")),
            "{auth_url}"
        );
        Ok(())
    }

    /// Claude Code StartOAuth happy path (claudecode.go:84-125): 200
    /// without the Codex-only extras.
    #[tokio::test]
    async fn claude_start_oauth_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) =
            call_json(&mut app, post("/admin/claudecode/oauth/start", "{}")?).await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        let auth_url = body["auth_url"].as_str().unwrap_or_default();
        assert!(
            auth_url.starts_with("https://claude.ai/oauth/authorize?"),
            "{auth_url}"
        );
        assert!(
            !auth_url.contains("codex_cli_simplified_flow"),
            "{auth_url}"
        );
        assert!(
            !auth_url.contains("id_token_add_organizations"),
            "{auth_url}"
        );
        assert!(auth_url.contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"));
        Ok(())
    }

    /// Empty body → gin EOF → 400 "invalid request format" (codex.go:88-91).
    #[tokio::test]
    async fn start_oauth_empty_body_returns_400_invalid_request_format()
    -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) = call_json(&mut app, post("/admin/codex/oauth/start", "")?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "Bad Request");
        assert_eq!(body["error"]["message"], "invalid request format");
        Ok(())
    }

    /// Unknown provider → 404 (router accepts the path, handler rejects).
    #[tokio::test]
    async fn start_oauth_unknown_provider_returns_404() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) = call_json(&mut app, post("/admin/unknown/oauth/start", "{}")?).await?;

        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "unknown oauth provider");
        Ok(())
    }

    /// Unwired service → 500 (skeleton-state degradation).
    #[tokio::test]
    async fn start_oauth_no_service_returns_500() -> Result<(), Box<dyn std::error::Error>> {
        // Wire the JWT secret so the request clears the admin guard
        // (routes.go:96 adminGroup), but wire NO OAuthAdminService — the
        // handler must then hit its own skeleton-state 500 branch. Using a
        // bare `AppState::default()` here would instead fail at the JWT guard
        // (no secret → 500 "Failed to validate token"), masking the branch
        // this test pins.
        let mut config = conduit_config::AppConfig::default();
        config.api_auth.jwt_secret = Some(TEST_JWT_SECRET.to_string());
        let mut app = build_router(crate::app_state::AppState::new(
            std::sync::Arc::new(config),
            std::sync::Arc::new(AppServices::new()),
        ));
        let (status, body) = call_json(&mut app, post("/admin/codex/oauth/start", "{}")?).await?;

        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["message"], "oauth service is not configured");
        Ok(())
    }

    /// Exchange happy path (codex.go:199-264): cached state + matching
    /// callback state → 200 `{"credentials":"<json>"}`.
    #[tokio::test]
    async fn codex_exchange_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        // Start a session first.
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) = call_json(&mut app, post("/admin/codex/oauth/start", "{}")?).await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let callback = format!("http://localhost:1455/auth/callback?code=abc&state={session_id}");
        let payload = json!({
            "session_id": session_id,
            "callback_url": callback,
        })
        .to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/codex/oauth/exchange", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        let creds = body["credentials"].as_str().unwrap_or_default();
        assert!(creds.contains("app_EMoamEEZ73f0CkXaXp7hrann"), "{creds}");
        assert!(creds.contains("mock-token-"), "{creds}");
        Ok(())
    }

    /// Expired/invalid session → 400 "invalid or expired oauth session"
    /// (codex.go:217 / claudecode.go:186).
    #[tokio::test]
    async fn exchange_unknown_session_returns_400() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let payload = json!({
            "session_id": "never-issued",
            "callback_url": "http://localhost:1455/auth/callback?code=abc&state=never-issued",
        })
        .to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/codex/oauth/exchange", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "invalid or expired oauth session");
        Ok(())
    }

    /// Empty session_id/callback_url → 400 "session_id and callback_url are
    /// required" (codex.go:208-211).
    #[tokio::test]
    async fn exchange_missing_fields_returns_400_required() -> Result<(), Box<dyn std::error::Error>>
    {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) = call_json(
            &mut app,
            post(
                "/admin/codex/oauth/exchange",
                &json!({"session_id": "", "callback_url": ""}).to_string(),
            )?,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"]["message"],
            "session_id and callback_url are required"
        );
        Ok(())
    }

    /// Malformed JSON body → 400 "invalid request format" (codex.go:203-206).
    #[tokio::test]
    async fn exchange_malformed_body_returns_400() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) =
            call_json(&mut app, post("/admin/codex/oauth/exchange", "{not json")?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "invalid request format");
        Ok(())
    }

    /// State mismatch → 400 "oauth state mismatch" (codex.go:232).
    #[tokio::test]
    async fn exchange_state_mismatch_returns_400() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) = call_json(&mut app, post("/admin/codex/oauth/start", "{}")?).await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        // Different state in callback.
        let payload = json!({
            "session_id": session_id,
            "callback_url": "http://localhost:1455/auth/callback?code=abc&state=other",
        })
        .to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/codex/oauth/exchange", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "oauth state mismatch");
        Ok(())
    }

    /// Token-endpoint failure → 502 "token exchange failed: <err>"
    /// (codex.go:253 / claudecode.go:223).
    #[tokio::test]
    async fn exchange_token_failure_returns_502() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService {
            fail_exchange_upstream: true,
            ..InMemoryOAuthService::default()
        });
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) = call_json(&mut app, post("/admin/codex/oauth/start", "{}")?).await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let payload = json!({
            "session_id": session_id,
            "callback_url": format!("http://localhost:1455/auth/callback?code=abc&state={session_id}"),
        })
        .to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/codex/oauth/exchange", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_GATEWAY);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .starts_with("token exchange failed:"),
            "{}",
            body
        );
        Ok(())
    }

    /// Callback URL missing the `code` query param → 400 with err.Error()
    /// verbatim (codex.go:227).
    #[tokio::test]
    async fn exchange_callback_missing_code_returns_400() -> Result<(), Box<dyn std::error::Error>>
    {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) = call_json(&mut app, post("/admin/codex/oauth/start", "{}")?).await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let payload = json!({
            "session_id": session_id,
            "callback_url": format!("http://localhost:1455/auth/callback?state={session_id}"),
        })
        .to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/codex/oauth/exchange", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"]["message"],
            "code parameter not found in callback_url"
        );
        Ok(())
    }

    /// ClaudeCode exchange also flows through the same handler.
    #[tokio::test]
    async fn claude_exchange_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) =
            call_json(&mut app, post("/admin/claudecode/oauth/start", "{}")?).await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let payload = json!({
            "session_id": session_id,
            "callback_url": format!("http://localhost:54545/callback?code=abc&state={session_id}"),
        })
        .to_string();
        let (status, body) = call_json(
            &mut app,
            post("/admin/claudecode/oauth/exchange", &payload)?,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        let creds = body["credentials"].as_str().unwrap_or_default();
        assert!(
            creds.contains("9d1c250a-e61b-44d9-88ed-5944d1962f5e"),
            "{creds}"
        );
        Ok(())
    }

    /// DecodeAuthJSON happy path (codex.go:175-195).
    #[tokio::test]
    async fn codex_decode_auth_json_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service.clone());
        let payload = json!({"auth_json": r#"{"tokens":{"access_token":"abc"}}"#}).to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/codex/auth/decode", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        let creds = body["credentials"].as_str().unwrap_or_default();
        assert!(creds.contains("app_EMoamEEZ73f0CkXaXp7hrann"), "{creds}");
        // Saw the auth_json verbatim.
        let seen = service.seen_decode.lock().ok().and_then(|g| g.clone());
        assert_eq!(
            seen.as_deref(),
            Some(r#"{"tokens":{"access_token":"abc"}}"#)
        );
        Ok(())
    }

    /// Missing auth_json → 400 "invalid request format" (codex.go:177-180).
    #[tokio::test]
    async fn decode_auth_json_missing_returns_400() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) = call_json(&mut app, post("/admin/codex/auth/decode", "{}")?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "invalid request format");
        Ok(())
    }

    /// `codex.DecodeAuthJSON` failure → 400 "failed to decode auth json"
    /// (codex.go:184).
    #[tokio::test]
    async fn decode_auth_json_decode_failure_returns_400() -> Result<(), Box<dyn std::error::Error>>
    {
        let service = Arc::new(InMemoryOAuthService {
            fail_decode: true,
            ..InMemoryOAuthService::default()
        });
        let mut app = app_with_oauth(service);
        let payload = json!({"auth_json": "garbage"}).to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/codex/auth/decode", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .starts_with("failed to decode auth json:"),
            "{}",
            body
        );
        Ok(())
    }

    /// creds.ToJSON failure → 500 "failed to encode credentials"
    /// (codex.go:190).
    #[tokio::test]
    async fn decode_auth_json_encode_failure_returns_500() -> Result<(), Box<dyn std::error::Error>>
    {
        let service = Arc::new(InMemoryOAuthService {
            fail_encode: true,
            ..InMemoryOAuthService::default()
        });
        let mut app = app_with_oauth(service);
        let payload = json!({"auth_json": "ok"}).to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/codex/auth/decode", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .starts_with("failed to encode credentials:"),
            "{}",
            body
        );
        Ok(())
    }

    /// Copilot poll with empty body → 400 "invalid request format"
    /// (copilot.go:273-277 — ShouldBindJSON on EOF yields 400, *not* the
    /// StartOAuth-style escape hatch).
    #[tokio::test]
    async fn copilot_poll_empty_body_returns_400() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) = call_json(&mut app, post("/admin/copilot/oauth/poll", "")?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "invalid request format");
        Ok(())
    }

    /// Copilot poll with missing session_id → 400 (gin `binding:"required"`
    /// on SessionID — copilot.go:254-257).
    #[tokio::test]
    async fn copilot_poll_missing_session_id_returns_400() -> Result<(), Box<dyn std::error::Error>>
    {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) = call_json(&mut app, post("/admin/copilot/oauth/poll", "{}")?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "invalid request format");
        Ok(())
    }

    /// Copilot poll happy path: pending status (copilot.go:310-315).
    #[tokio::test]
    async fn copilot_poll_pending_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) = call_json(&mut app, post("/admin/copilot/oauth/start", "")?).await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let payload = json!({"session_id": session_id}).to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/copilot/oauth/poll", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["status"], "pending");
        assert_eq!(
            body["message"],
            "Authorization pending. User has not yet authorized the device."
        );
        Ok(())
    }

    /// Copilot poll happy path: complete status (copilot.go:337-352).
    #[tokio::test]
    async fn copilot_poll_complete_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService {
            copilot_poll_status: Some(CopilotPollStatus::Complete {
                access_token: "gho_mocktoken".to_string(),
                token_type: "bearer".to_string(),
                scope: "read:user".to_string(),
            }),
            ..InMemoryOAuthService::default()
        });
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) =
            call_json(&mut app, post("/admin/copilot/oauth/start", "{}")?).await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let payload = json!({"session_id": session_id}).to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/copilot/oauth/poll", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["status"], "complete");
        assert_eq!(body["access_token"], "gho_mocktoken");
        assert_eq!(body["token_type"], "bearer");
        assert_eq!(body["scope"], "read:user");
        // Cache cleanup on terminal state.
        let sessions = service.copilot_sessions.lock().ok();
        assert!(
            sessions.is_some_and(|s| s.is_empty()),
            "expected cache cleared after complete"
        );
        Ok(())
    }

    /// Copilot poll: slow_down status (copilot.go:316-321).
    #[tokio::test]
    async fn copilot_poll_slow_down_status() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService {
            copilot_poll_status: Some(CopilotPollStatus::SlowDown),
            ..InMemoryOAuthService::default()
        });
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) =
            call_json(&mut app, post("/admin/copilot/oauth/start", "{}")?).await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let payload = json!({"session_id": session_id}).to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/copilot/oauth/poll", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["status"], "slow_down");
        assert_eq!(body["message"], "Polling too fast. Please slow down.");
        Ok(())
    }

    /// Copilot poll: unknown session → 400 "invalid or expired session"
    /// (copilot.go:281-285).
    #[tokio::test]
    async fn copilot_poll_unknown_session_returns_400() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let payload = json!({"session_id": "never-issued"}).to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/copilot/oauth/poll", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "invalid or expired session");
        Ok(())
    }

    /// Copilot poll: token-poll transport failure → 502 (copilot.go:301-305).
    #[tokio::test]
    async fn copilot_poll_token_failure_returns_502() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService {
            fail_exchange_upstream: true,
            ..InMemoryOAuthService::default()
        });
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) =
            call_json(&mut app, post("/admin/copilot/oauth/start", "{}")?).await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let payload = json!({"session_id": session_id}).to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/copilot/oauth/poll", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_GATEWAY);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .starts_with("token poll failed:"),
            "{}",
            body
        );
        Ok(())
    }

    /// Copilot StartOAuth happy path: empty body allowed (copilot.go:159-166).
    #[tokio::test]
    async fn copilot_start_oauth_empty_body_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) = call_json(&mut app, post("/admin/copilot/oauth/start", "")?).await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        let session_id = body["session_id"].as_str().unwrap_or_default();
        assert!(session_id.starts_with("copilot-session-"), "{session_id}");
        assert_eq!(body["user_code"], "MOCK-CODE");
        assert_eq!(body["verification_uri"], "https://github.com/login/device");
        assert_eq!(body["expires_in"], 900);
        assert_eq!(body["interval"], 5);
        Ok(())
    }

    /// Copilot StartOAuth happy path: `{}` JSON body.
    #[tokio::test]
    async fn copilot_start_oauth_json_object_happy_path() -> Result<(), Box<dyn std::error::Error>>
    {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) = call_json(&mut app, post("/admin/copilot/oauth/start", "{}")?).await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(
            body["session_id"]
                .as_str()
                .unwrap_or_default()
                .starts_with("copilot-session-")
        );
        Ok(())
    }

    /// Copilot StartOAuth device-code failure → 502 (copilot.go:182-185).
    #[tokio::test]
    async fn copilot_start_oauth_device_code_failure_returns_502()
    -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService {
            fail_copilot_device_code: true,
            ..InMemoryOAuthService::default()
        });
        let mut app = app_with_oauth(service);
        let (status, body) = call_json(&mut app, post("/admin/copilot/oauth/start", "{}")?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_GATEWAY);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .starts_with("failed to request device code:"),
            "{}",
            body
        );
        Ok(())
    }

    /// Antigravity StartOAuth happy path (antigravity.go:89-138).
    #[tokio::test]
    async fn antigravity_start_oauth_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let payload = json!({"project_id": "my-project"}).to_string();
        let (status, body) =
            call_json(&mut app, post("/admin/antigravity/oauth/start", &payload)?).await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        let session_id = body["session_id"].as_str().unwrap_or_default();
        assert!(session_id.starts_with("antigravity-state-"), "{session_id}");
        let auth_url = body["auth_url"].as_str().unwrap_or_default();
        assert!(
            auth_url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
            "{auth_url}"
        );
        // Antigravity-only extras.
        assert!(auth_url.contains("access_type=offline"), "{auth_url}");
        assert!(auth_url.contains("prompt=consent"), "{auth_url}");
        assert!(
            auth_url.contains(
                "client_id=REMOVED_GOOGLE_OAUTH_CLIENT_ID"
            ),
            "{auth_url}"
        );
        // Sorted query: access_type precedes client_id.
        let at_pos = auth_url.find("access_type=").unwrap_or(0);
        let cid_pos = auth_url.find("client_id=").unwrap_or(0);
        assert!(at_pos < cid_pos, "Go url.Values.Encode sorts: {auth_url}");
        Ok(())
    }

    /// Antigravity StartOAuth: empty body → 400 (antigravity.go:92-96 — no
    /// EOF escape hatch for this handler).
    #[tokio::test]
    async fn antigravity_start_oauth_empty_body_returns_400()
    -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, body) =
            call_json(&mut app, post("/admin/antigravity/oauth/start", "")?).await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "invalid request format");
        Ok(())
    }

    /// Antigravity StartOAuth: project_id omitted is allowed (Go zero-fills).
    #[tokio::test]
    async fn antigravity_start_oauth_omits_project_id() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let (status, _body) =
            call_json(&mut app, post("/admin/antigravity/oauth/start", "{}")?).await?;
        assert_eq!(status, axum::http::StatusCode::OK);
        Ok(())
    }

    /// Antigravity exchange happy path — verifies the
    /// `refreshToken|projectId` wire format (antigravity.go:243-246).
    #[tokio::test]
    async fn antigravity_exchange_happy_path() -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service.clone());
        // Start with explicit project_id.
        let (_, start_body) = call_json(
            &mut app,
            post(
                "/admin/antigravity/oauth/start",
                &json!({"project_id": "proj-123"}).to_string(),
            )?,
        )
        .await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let payload = json!({
            "session_id": session_id,
            "callback_url": format!(
                "http://localhost:51121/oauth-callback?code=abc&state={session_id}"
            ),
        })
        .to_string();
        let (status, body) = call_json(
            &mut app,
            post("/admin/antigravity/oauth/exchange", &payload)?,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["credentials"], "mock-refresh-token|proj-123");
        Ok(())
    }

    /// Antigravity exchange: empty project_id in start → falls back to
    /// `antigravity.DefaultProjectID` (constants.go:34).
    #[tokio::test]
    async fn antigravity_exchange_falls_back_to_default_project_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) = call_json(
            &mut app,
            post("/admin/antigravity/oauth/start", &json!({}).to_string())?,
        )
        .await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let payload = json!({
            "session_id": session_id,
            "callback_url": format!(
                "http://localhost:51121/oauth-callback?code=abc&state={session_id}"
            ),
        })
        .to_string();
        let (status, body) = call_json(
            &mut app,
            post("/admin/antigravity/oauth/exchange", &payload)?,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body["credentials"], "mock-refresh-token|rising-fact-p41fc");
        Ok(())
    }

    /// Antigravity exchange: unknown session → 400 (antigravity.go:189-193).
    #[tokio::test]
    async fn antigravity_exchange_unknown_session_returns_400()
    -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service);
        let payload = json!({
            "session_id": "never-issued",
            "callback_url": "http://localhost:51121/oauth-callback?code=abc&state=never-issued",
        })
        .to_string();
        let (status, body) = call_json(
            &mut app,
            post("/admin/antigravity/oauth/exchange", &payload)?,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "invalid or expired oauth session");
        Ok(())
    }

    /// Antigravity exchange: state mismatch → 400 (antigravity.go:201-204).
    #[tokio::test]
    async fn antigravity_exchange_state_mismatch_returns_400()
    -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService::default());
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) = call_json(
            &mut app,
            post("/admin/antigravity/oauth/start", &json!({}).to_string())?,
        )
        .await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let payload = json!({
            "session_id": session_id,
            "callback_url": "http://localhost:51121/oauth-callback?code=abc&state=other",
        })
        .to_string();
        let (status, body) = call_json(
            &mut app,
            post("/admin/antigravity/oauth/exchange", &payload)?,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "oauth state mismatch");
        Ok(())
    }

    /// Antigravity exchange: token endpoint failure → 502
    /// (antigravity.go:227-229).
    #[tokio::test]
    async fn antigravity_exchange_token_failure_returns_502()
    -> Result<(), Box<dyn std::error::Error>> {
        let service = Arc::new(InMemoryOAuthService {
            fail_exchange_upstream: true,
            ..InMemoryOAuthService::default()
        });
        let mut app = app_with_oauth(service.clone());
        let (_, start_body) = call_json(
            &mut app,
            post("/admin/antigravity/oauth/start", &json!({}).to_string())?,
        )
        .await?;
        let session_id = start_body["session_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let payload = json!({
            "session_id": session_id,
            "callback_url": format!(
                "http://localhost:51121/oauth-callback?code=abc&state={session_id}"
            ),
        })
        .to_string();
        let (status, body) = call_json(
            &mut app,
            post("/admin/antigravity/oauth/exchange", &payload)?,
        )
        .await?;

        assert_eq!(status, axum::http::StatusCode::BAD_GATEWAY);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .starts_with("token exchange failed:"),
            "{}",
            body
        );
        Ok(())
    }

    /// bind_empty_object: empty body → Err; `{}` → Ok; non-object JSON →
    /// Err; unknown-field object → Ok (gin default).
    #[test]
    fn bind_empty_object_matches_gin_semantics() {
        assert!(bind_empty_object(b"").is_err());
        assert!(bind_empty_object(b"{}").is_ok());
        assert!(bind_empty_object(b"{\"anything\":1}").is_ok());
        assert!(bind_empty_object(b"123").is_err()); // non-object JSON
        assert!(bind_empty_object(b"not json").is_err());
    }
}
