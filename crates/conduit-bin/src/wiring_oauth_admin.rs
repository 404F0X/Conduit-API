//! ADPT-OAUTH-ADMIN — host adapter implementing
//! [`conduit_http::oauth_handlers::OAuthAdminService`] (the currently-None
//! `oauth_admin` AppServices slot).
//!
//! Scope: this is the **channel OAuth** admin flow — obtaining provider
//! credentials for OAuth-based channel types (Codex / Claude Code /
//! Antigravity / GitHub Copilot). It is NOT user SSO (that is
//! `oidc_handlers` / `OidcService`).
//!
//! ## Go parity anchors (read from `conduit/`, never guessed)
//! - `api.CodexHandlers` — `conduit/internal/server/api/codex.go`
//!   (StartOAuth :84-127, DecodeAuthJSON :175-195, Exchange :199-264,
//!   parseCodexCallbackURL :147-171).
//! - `api.ClaudeCodeHandlers` — `claudecode.go` (StartOAuth :84-125,
//!   Exchange :173-234, parseClaudeCodeCallbackURL :137-169 — state comes
//!   from the URL **fragment** first).
//! - `api.AntigravityHandlers` — `antigravity.go` (StartOAuth :89-138,
//!   Exchange :178-247, resolveProjectID :249-346 / onboardUser :348-405).
//! - `api.CopilotHandlers` — `copilot.go` (constants :24-44, StartOAuth
//!   :155-215, requestDeviceCode :218-251, PollOAuth :270-356,
//!   pollAccessToken :359-407).
//!
//! ## Storage decision (with Go refs)
//! Go persists **nothing to the database** in this flow:
//! - Pending PKCE / device-flow state lives in an `xcache.Cache[T]` TTL cache
//!   (codex.go:32+108, claudecode.go:32+108, antigravity.go:34+115,
//!   copilot.go:79+203) — 10-minute TTL for the PKCE providers,
//!   `min(expires_in, 15min)` for the Copilot device flow.
//! - The exchanged credentials are **returned to the caller** as a string
//!   (`OAuthCredentials.ToJSON()` / the Antigravity `refreshToken|projectId`
//!   format); the frontend later stores them into the channel row's
//!   `credentials.oauth` JSON via the channel create/update GraphQL mutation.
//!   No channel-repo write happens inside the OAuth handlers, so this adapter
//!   deliberately has NO channel-repo dependency (adding one would invent a
//!   flow the Go source does not have).
//!
//! This host mirrors the Go cache with an in-process `Mutex<HashMap>` TTL
//! store (the Go default `xcache` config is the in-memory backend; a Redis
//! port is a separate wiring gap).
//!
//! ## Random-token encoding deviation (documented)
//! Go encodes the random `state` (32 bytes) and `code_verifier` (64 bytes)
//! with `base64.URLEncoding.WithPadding(NoPadding)`. The bin crate has no
//! `rand`/`base64` dependency, so this host reuses
//! `conduit_auth::generate_secret_key()` (32 CSPRNG bytes, hex-encoded):
//! one call for the state (64 URL-safe chars, 256-bit entropy — same
//! entropy as Go) and two concatenated calls for the verifier (128 chars,
//! the RFC 7636 maximum; hex is a valid PKCE-verifier alphabet). These are
//! opaque one-shot random tokens, not contract values — only URL-safety and
//! entropy matter, both preserved.
//!
//! ## Leader wiring (do NOT self-wire)
//! 1. `crates/conduit-bin/src/main.rs`: `mod wiring_oauth_admin;`
//! 2. In `wiring.rs::build_services` (or equivalent):
//!    ```ignore
//!    let oauth_admin: std::sync::Arc<dyn conduit_http::oauth_handlers::OAuthAdminService> =
//!        std::sync::Arc::new(crate::wiring_oauth_admin::OAuthAdminAdapter::new());
//!    let services = services.with_oauth_admin_service(oauth_admin);
//!    ```
//!    (`AppServices::with_oauth_admin_service` — app_state.rs:146.)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use conduit_auth::generate_secret_key;
use conduit_http::oauth_handlers::{
    CopilotPollStatus, ExchangeOAuthRequest, OAuthAdminService, OAuthProvider, OAuthServerError,
    PollCopilotOAuthRequest, ProxyConfig, StartAntigravityOAuthResponse, StartCopilotOAuthResponse,
    StartOAuthResponse,
};
use conduit_http::{pkce_challenge, query_escape};
use conduit_llm::model::{HeaderMap, HttpRequest};
use conduit_transformers::antigravity as anti_tf;
use conduit_transformers::claudecode as claude_tf;
use conduit_transformers::claudecode::{ExchangeParams, OAuthCredentials};
use conduit_transformers::codex as codex_tf;

// ---------------------------------------------------------------------------
// GitHub Copilot device-flow constants — Go copilot.go:24-44 verbatim.
// ---------------------------------------------------------------------------

/// Go `githubDeviceCodeURL` (copilot.go:26).
const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
/// Go `githubAccessTokenURL` (copilot.go:27).
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
/// Go `defaultGithubCopilotClientID` (copilot.go:31) — the VS Code public
/// client ID, used as the fallback when `GITHUB_COPILOT_CLIENT_ID` is unset.
const DEFAULT_GITHUB_COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
/// Go `githubCopilotScope` (copilot.go:34).
const GITHUB_COPILOT_SCOPE: &str = "read:user";
/// Go `deviceGrantType` (copilot.go:37).
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Go `deviceFlowCacheExpiration` (copilot.go:43) — 15 minutes, in seconds.
const DEVICE_FLOW_CACHE_EXPIRATION_SECS: i64 = 15 * 60;

/// Go `xcache.WithExpiration(10*time.Minute)` — the PKCE-state TTL shared by
/// the Codex / Claude Code / Antigravity StartOAuth handlers (codex.go:108,
/// claudecode.go:108, antigravity.go:119).
const PKCE_STATE_TTL_SECS: i64 = 10 * 60;

/// Go `getGithubCopilotClientID` (copilot.go:49-54): env override first,
/// then the VS Code default. Read at call time, like Go.
fn github_copilot_client_id() -> String {
    match std::env::var("GITHUB_COPILOT_CLIENT_ID") {
        Ok(client_id) if !client_id.is_empty() => client_id,
        _ => DEFAULT_GITHUB_COPILOT_CLIENT_ID.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Pending-state TTL store — stands in for the Go `xcache.Cache[T]` seam.
// ---------------------------------------------------------------------------

/// One pending OAuth session. Mirrors the three Go cache value types; the
/// PKCE variants drop Go's `CreatedAt` field because Go never reads it (the
/// cache TTL owns expiry), while the Copilot variant keeps
/// `created_at`/`expires_in` because `PollOAuth` re-checks them explicitly
/// (copilot.go:288-292).
#[derive(Debug, Clone)]
enum PendingState {
    /// Go `codexOAuthState` (codex.go:50-53) / `claudeCodeOAuthState`
    /// (claudecode.go:50-53).
    Pkce { code_verifier: String },
    /// Go `antigravityOAuthState` (antigravity.go:54-58) — carries the
    /// optional `project_id` from the start request through to the exchange.
    AntigravityPkce {
        code_verifier: String,
        project_id: String,
    },
    /// Go `copilotDeviceFlowState` (copilot.go:85-92). `user_code` /
    /// `verification_uri` / `interval` are only echoed in the start response
    /// and never read back by PollOAuth, so they are not stored.
    CopilotDevice {
        device_code: String,
        expires_in: i64,
        created_at: i64,
    },
}

struct StoredEntry {
    state: PendingState,
    expires_at: DateTime<Utc>,
}

/// In-process TTL map mirroring Go `xcache` Set/Get/Delete semantics: an
/// expired entry behaves exactly like a missing one.
#[derive(Default)]
struct StateStore {
    entries: Mutex<HashMap<String, StoredEntry>>,
}

/// The store error every method maps a poisoned mutex to. Unreachable in
/// practice (no code path panics while holding the lock), but the workspace
/// forbids `unwrap`, so the failure mode is surfaced as the Go cache-failure
/// 500 rather than a panic.
const STORE_POISONED: &str = "oauth state store poisoned";

impl StateStore {
    fn set(&self, key: String, state: PendingState, ttl_secs: i64) -> Result<(), OAuthServerError> {
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| OAuthServerError::Internal(STORE_POISONED.to_string()))?;
        let ttl = chrono::Duration::try_seconds(ttl_secs).unwrap_or_else(chrono::Duration::zero);
        guard.insert(
            key,
            StoredEntry {
                state,
                expires_at: Utc::now() + ttl,
            },
        );
        Ok(())
    }

    /// Non-consuming lookup (Go `cache.Get`). Expired entries are evicted and
    /// reported as missing.
    fn get(&self, key: &str) -> Result<Option<PendingState>, OAuthServerError> {
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| OAuthServerError::Internal(STORE_POISONED.to_string()))?;
        let now = Utc::now();
        let expired = guard.get(key).map(|entry| entry.expires_at <= now);
        match expired {
            None => Ok(None),
            Some(true) => {
                guard.remove(key);
                Ok(None)
            }
            Some(false) => Ok(guard.get(key).map(|entry| entry.state.clone())),
        }
    }

    /// Consuming lookup (Go `cache.Get` immediately followed by
    /// `cache.Delete`, the codex.go:215-223 pattern).
    fn remove(&self, key: &str) -> Result<Option<PendingState>, OAuthServerError> {
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| OAuthServerError::Internal(STORE_POISONED.to_string()))?;
        match guard.remove(key) {
            Some(entry) if entry.expires_at > Utc::now() => Ok(Some(entry.state)),
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Outbound token HTTP — the `*httpclient.HttpClient` seam.
// ---------------------------------------------------------------------------

/// Minimal response the token flows need (status + content type + body).
#[derive(Debug, Clone)]
pub struct OAuthHttpResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// Executes one token-endpoint HTTP request. Stands in for the Go
/// `httpclient.HttpClient.Do` call sites; `proxy` mirrors the per-request
/// `h.httpClient.WithProxy(req.Proxy)` branch (codex.go:237-240,
/// claudecode.go:206-209, antigravity.go:212-215, copilot.go:175-178 /
/// 294-298: taken only when `type == "url"` and the URL is non-empty).
#[async_trait]
pub trait OAuthTokenHttp: Send + Sync {
    async fn execute(
        &self,
        request: HttpRequest,
        proxy: Option<&ProxyConfig>,
    ) -> Result<OAuthHttpResponse, String>;
}

/// Production executor backed by `reqwest` (the same client family the rest
/// of the host wiring uses).
#[derive(Default)]
pub struct ReqwestTokenHttp {
    client: reqwest::Client,
}

impl ReqwestTokenHttp {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl OAuthTokenHttp for ReqwestTokenHttp {
    async fn execute(
        &self,
        request: HttpRequest,
        proxy: Option<&ProxyConfig>,
    ) -> Result<OAuthHttpResponse, String> {
        let url = request
            .url
            .clone()
            .ok_or_else(|| "token request url is empty".to_string())?;

        // Go `httpclient.ProxyTypeURL` is the literal "url" (proxy.go:8).
        let client = match proxy {
            Some(config) if config.r#type == "url" && !config.url.is_empty() => {
                let proxy = reqwest::Proxy::all(&config.url)
                    .map_err(|err| format!("invalid proxy url: {err}"))?;
                reqwest::Client::builder()
                    .proxy(proxy)
                    .build()
                    .map_err(|err| format!("failed to build proxied client: {err}"))?
            }
            _ => self.client.clone(),
        };

        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|err| format!("invalid method: {err}"))?;
        let mut builder = client.request(method, &url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(content_type) = &request.content_type {
            builder = builder.header("Content-Type", content_type);
        }
        if let Some(body) = request.body.clone() {
            builder = builder.body(body);
        }

        let response = builder.send().await.map_err(|err| err.to_string())?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = response
            .bytes()
            .await
            .map_err(|err| err.to_string())?
            .to_vec();
        Ok(OAuthHttpResponse {
            status,
            content_type,
            body,
        })
    }
}

// ---------------------------------------------------------------------------
// Pure helpers — URL building + callback parsing (Go parity).
// ---------------------------------------------------------------------------

/// Go `url.Values.Encode()`: keys sorted alphabetically, both key and value
/// `url.QueryEscape`d, pairs joined with `&`. `query_escape` is the verbatim
/// Go `url.QueryEscape` port from `conduit_http::oidc_handlers`.
fn encode_query(pairs: &[(&str, &str)]) -> String {
    let mut sorted: Vec<&(&str, &str)> = pairs.iter().collect();
    sorted.sort_by_key(|(key, _)| *key);
    sorted
        .iter()
        .map(|(key, value)| format!("{}={}", query_escape(key), query_escape(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Codex authorize URL (codex.go:113-124) — includes the two Codex-only
/// params `id_token_add_organizations=true` + `codex_cli_simplified_flow=true`.
fn codex_authorize_url(state: &str, challenge: &str) -> String {
    let pairs: &[(&str, &str)] = &[
        ("response_type", "code"),
        ("client_id", codex_tf::CLIENT_ID),
        ("redirect_uri", codex_tf::REDIRECT_URI),
        ("scope", codex_tf::SCOPES),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
    ];
    format!("{}?{}", codex_tf::AUTHORIZE_URL, encode_query(pairs))
}

/// Claude Code authorize URL (claudecode.go:113-122).
fn claude_authorize_url(state: &str, challenge: &str) -> String {
    let pairs: &[(&str, &str)] = &[
        ("response_type", "code"),
        ("client_id", claude_tf::CLIENT_ID),
        ("redirect_uri", claude_tf::REDIRECT_URI),
        ("scope", claude_tf::SCOPES),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    format!("{}?{}", claude_tf::AUTHORIZE_URL, encode_query(pairs))
}

/// Antigravity (Google) authorize URL (antigravity.go:124-135) — adds
/// `access_type=offline` + `prompt=consent`.
fn antigravity_authorize_url(state: &str, challenge: &str) -> String {
    let scopes = anti_tf::scopes_string();
    let pairs: &[(&str, &str)] = &[
        ("response_type", "code"),
        ("client_id", anti_tf::CLIENT_ID),
        ("redirect_uri", anti_tf::REDIRECT_URI),
        ("scope", scopes.as_str()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("access_type", "offline"),
        ("prompt", "consent"),
    ];
    format!("{}?{}", anti_tf::AUTHORIZE_URL, encode_query(pairs))
}

/// Percent-decode one query/fragment component. `plus_as_space` mirrors the
/// query-component rules (Go `url.ParseQuery`); the fragment keeps `+`
/// literal (Go `u.Fragment`). Malformed `%` escapes are kept literally
/// (lenient; Go's `url.Parse` would reject the whole URL as
/// `"invalid callback_url: …"` — that error arm is unreachable here).
fn percent_decode(input: &str, plus_as_space: bool) -> String {
    fn hex_val(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push((hi << 4) | lo);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a raw query string into decoded `(key, value)` pairs.
fn parse_query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|segment| !segment.is_empty())
        .map(|segment| match segment.split_once('=') {
            Some((key, value)) => (percent_decode(key, true), percent_decode(value, true)),
            None => (percent_decode(segment, true), String::new()),
        })
        .collect()
}

/// Ports `parseCodexCallbackURL` (codex.go:147-171) /
/// `parseAntigravityCallbackURL` (antigravity.go:150-174) and, with
/// `fragment_state_first`, `parseClaudeCodeCallbackURL`
/// (claudecode.go:137-169 — Claude puts the state in the URL fragment,
/// `?code=xxx#state`, with the query param as fallback). Returns
/// `(code, state)`; error strings are the Go ones verbatim (surfaced to the
/// client as 400 bodies — codex.go:227).
fn parse_callback_url(
    callback_url: &str,
    fragment_state_first: bool,
) -> Result<(String, String), String> {
    let trimmed = callback_url.trim();
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err("callback_url must be a full URL".to_string());
    }

    let (without_fragment, fragment) = match trimmed.split_once('#') {
        Some((head, fragment)) => (head, fragment),
        None => (trimmed, ""),
    };
    let query = without_fragment
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");

    // Go `q.Get(...)` returns the FIRST value for a key.
    let mut code = String::new();
    let mut query_state = String::new();
    for (key, value) in parse_query_pairs(query) {
        if key == "code" && code.is_empty() {
            code = value;
        } else if key == "state" && query_state.is_empty() {
            query_state = value;
        }
    }

    if code.is_empty() {
        return Err("code parameter not found in callback_url".to_string());
    }

    let state = if fragment_state_first {
        let fragment = percent_decode(fragment, false);
        if fragment.is_empty() {
            query_state
        } else {
            fragment
        }
    } else {
        query_state
    };

    if state.is_empty() {
        return Err(if fragment_state_first {
            // claudecode.go:165
            "state parameter not found in callback_url (should be after # or in query)".to_string()
        } else {
            // codex.go:167 / antigravity.go:170
            "state parameter not found in callback_url".to_string()
        });
    }

    Ok((code, state))
}

/// Ensures a service-layer failure surfaces with the Go handler's
/// `"token exchange failed: %w"` wrap (codex.go:253 / claudecode.go:223 /
/// antigravity.go:228) without double-prefixing errors that
/// `parse_exchange_response` already rewrote.
fn wrap_token_exchange_message(message: &str) -> String {
    if message.starts_with("token exchange failed:") {
        message.to_string()
    } else {
        format!("token exchange failed: {message}")
    }
}

/// Headers of the GitHub device-flow form posts (copilot.go:223-228 /
/// 365-370: `Accept: application/json` + form content type).
fn github_form_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );
    headers
}

/// Go `deviceCodeResponse` (copilot.go:95-101).
#[derive(Debug, Clone, Default, Deserialize)]
struct DeviceCodeResponse {
    #[serde(default)]
    device_code: String,
    #[serde(default)]
    user_code: String,
    #[serde(default)]
    verification_uri: String,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    interval: i64,
}

/// Go `accessTokenResponse` (copilot.go:104-110).
#[derive(Debug, Clone, Default, Deserialize)]
struct AccessTokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    token_type: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

/// GitHub returns JSON or form-encoded depending on the Accept negotiation;
/// Go branches on the response Content-Type (copilot.go:383-404). The
/// form-encoded arm is lenient where Go's `url.ParseQuery` could error.
fn parse_access_token_response(
    response: &OAuthHttpResponse,
) -> Result<AccessTokenResponse, String> {
    if response.content_type.contains("application/json") {
        serde_json::from_slice(&response.body)
            .map_err(|err| format!("failed to parse access token JSON response: {err}"))
    } else {
        let body = String::from_utf8_lossy(&response.body);
        let mut out = AccessTokenResponse::default();
        for (key, value) in parse_query_pairs(&body) {
            match key.as_str() {
                "access_token" if out.access_token.is_empty() => out.access_token = value,
                "token_type" if out.token_type.is_empty() => out.token_type = value,
                "scope" if out.scope.is_empty() => out.scope = value,
                "error" if out.error.is_empty() => out.error = value,
                "error_description" if out.error_description.is_empty() => {
                    out.error_description = value;
                }
                _ => {}
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Host-side [`OAuthAdminService`] implementation: in-process TTL state store
/// + reqwest-backed token-endpoint execution. One instance owns all four
/// providers' pending sessions (Go uses four separate handler structs, but
/// the cache keys are already provider-prefixed — `codex:oauth:…`,
/// `claudecode:oauth:…`, `antigravity:oauth:…`, `copilot:oauth:…` — so a
/// single map cannot collide).
pub struct OAuthAdminAdapter {
    http: Arc<dyn OAuthTokenHttp>,
    store: StateStore,
}

impl OAuthAdminAdapter {
    /// Production constructor (reqwest-backed token HTTP).
    pub fn new() -> Self {
        Self::with_http(Arc::new(ReqwestTokenHttp::new()))
    }

    /// Constructor with an injectable token-HTTP executor (tests).
    pub fn with_http(http: Arc<dyn OAuthTokenHttp>) -> Self {
        Self {
            http,
            store: StateStore::default(),
        }
    }

    /// Generate the PKCE trio. State: Go generates 32 random bytes
    /// (codex.go:69-76); verifier: 64 random bytes (codex.go:55-62);
    /// challenge: `base64url_nopad(sha256(verifier))` (codex.go:64-67,
    /// matched exactly by [`pkce_challenge`]). See the module doc for the
    /// hex-vs-base64url encoding note on the two random tokens.
    fn generate_pkce() -> (String, String, String) {
        let state = generate_secret_key();
        let verifier = format!("{}{}", generate_secret_key(), generate_secret_key());
        let challenge = pkce_challenge(&verifier);
        (state, verifier, challenge)
    }

    /// Execute a token-endpoint request and decode the OAuth credentials.
    /// Shared by the Codex / Claude Code / Antigravity exchanges: transport
    /// errors, non-2xx statuses, and `{error, error_description}` bodies all
    /// surface as 502 `"token exchange failed: …"` (Go handler wrap —
    /// codex.go:253 / claudecode.go:223 / antigravity.go:228).
    async fn execute_token_exchange(
        &self,
        request: HttpRequest,
        proxy: Option<&ProxyConfig>,
        client_id: &str,
    ) -> Result<OAuthCredentials, OAuthServerError> {
        let response = self
            .http
            .execute(request, proxy)
            .await
            .map_err(|err| OAuthServerError::BadGateway(wrap_token_exchange_message(&err)))?;

        // Go's httpclient.Do fails requests with status >= 400, carrying the
        // body in the error (llm/httpclient/client.go:227); TokenProvider
        // propagates that as the exchange error.
        if !(200..300).contains(&response.status) {
            return Err(OAuthServerError::BadGateway(format!(
                "token exchange failed: status {}: {}",
                response.status,
                String::from_utf8_lossy(&response.body)
            )));
        }

        claude_tf::parse_exchange_response(&response.body, client_id, Utc::now())
            .map_err(|err| OAuthServerError::BadGateway(wrap_token_exchange_message(&err.message)))
    }
}

impl Default for OAuthAdminAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OAuthAdminService for OAuthAdminAdapter {
    /// Codex `StartOAuth` (codex.go:84-127) / Claude Code `StartOAuth`
    /// (claudecode.go:84-125): mint state + PKCE verifier, cache them for 10
    /// minutes, and return the provider authorize URL.
    async fn start_oauth(
        &self,
        provider: OAuthProvider,
    ) -> Result<StartOAuthResponse, OAuthServerError> {
        let (cache_prefix, build_url): (&str, fn(&str, &str) -> String) = match provider {
            OAuthProvider::Codex => ("codex:oauth", codex_authorize_url),
            OAuthProvider::ClaudeCode => ("claudecode:oauth", claude_authorize_url),
            // The HTTP dispatcher routes Antigravity/Copilot to their
            // dedicated trait methods; reaching here is a routing bug.
            OAuthProvider::Antigravity | OAuthProvider::Copilot => {
                return Err(OAuthServerError::Internal(
                    "provider is not routed through start_oauth".to_string(),
                ));
            }
        };

        let (state, code_verifier, challenge) = Self::generate_pkce();
        // codex.go:107-111 / claudecode.go:107-111 — cache Set failure → 500.
        self.store.set(
            format!("{cache_prefix}:{state}"),
            PendingState::Pkce { code_verifier },
            PKCE_STATE_TTL_SECS,
        )?;

        Ok(StartOAuthResponse {
            auth_url: build_url(&state, &challenge),
            session_id: state,
        })
    }

    /// Codex `Exchange` (codex.go:199-264) / Claude Code `Exchange`
    /// (claudecode.go:173-234): consume the cached PKCE state, validate the
    /// callback, perform the live authorization-code exchange against the
    /// provider token endpoint, and return `OAuthCredentials.ToJSON()`.
    async fn exchange(
        &self,
        provider: OAuthProvider,
        request: ExchangeOAuthRequest,
    ) -> Result<String, OAuthServerError> {
        let (cache_prefix, client_id, redirect_uri, fragment_state_first) = match provider {
            OAuthProvider::Codex => (
                "codex:oauth",
                codex_tf::CLIENT_ID,
                codex_tf::REDIRECT_URI,
                false,
            ),
            OAuthProvider::ClaudeCode => (
                "claudecode:oauth",
                claude_tf::CLIENT_ID,
                claude_tf::REDIRECT_URI,
                true,
            ),
            // Antigravity is routed to exchange_antigravity; Copilot has no
            // exchange route at all (routes.go:106-117).
            OAuthProvider::Antigravity | OAuthProvider::Copilot => {
                return Err(OAuthServerError::Internal(
                    "provider is not routed through exchange".to_string(),
                ));
            }
        };

        // codex.go:213-223 — cache miss → 400; the entry is deleted right
        // after a successful Get (before any callback validation).
        let key = format!("{cache_prefix}:{}", request.session_id);
        let Some(PendingState::Pkce { code_verifier }) = self.store.remove(&key)? else {
            return Err(OAuthServerError::BadRequest(
                "invalid or expired oauth session".to_string(),
            ));
        };

        // codex.go:225-229 — parse error surfaced verbatim as the 400 body.
        let (code, callback_state) =
            parse_callback_url(&request.callback_url, fragment_state_first)
                .map_err(OAuthServerError::BadRequest)?;

        // codex.go:231-234 / claudecode.go:200-203.
        if callback_state != request.session_id {
            return Err(OAuthServerError::BadRequest(
                "oauth state mismatch".to_string(),
            ));
        }

        // codex.go:242-251 (form-encoded strategy, no state) /
        // claudecode.go:211-221 (JSON strategy — Claude requires the state in
        // the token exchange, exchange_strategy.go:99-102).
        let params = ExchangeParams {
            code,
            code_verifier,
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
            state: if fragment_state_first {
                callback_state
            } else {
                String::new()
            },
        };
        let http_request = match provider {
            OAuthProvider::Codex => codex_tf::build_exchange_request(&params, codex_tf::TOKEN_URL),
            _ => claude_tf::build_exchange_request(&params),
        }
        .map_err(|err| OAuthServerError::BadGateway(wrap_token_exchange_message(&err.message)))?;

        let creds = self
            .execute_token_exchange(http_request, request.proxy.as_ref(), client_id)
            .await?;

        // codex.go:257-261 / claudecode.go:227-231.
        creds.to_json().map_err(|err| {
            OAuthServerError::Internal(format!("failed to encode credentials: {}", err.message))
        })
    }

    /// Codex `DecodeAuthJSON` (codex.go:175-195): decode a Codex CLI
    /// `auth.json` into normalized OAuth credentials JSON. Pure — no HTTP.
    async fn decode_auth_json(&self, auth_json: String) -> Result<String, OAuthServerError> {
        // codex.go:182-186 — decode failure → 400 with the wrapped cause.
        let creds = codex_tf::decode_auth_json(&auth_json, Utc::now()).map_err(|err| {
            OAuthServerError::BadRequest(format!("failed to decode auth json: {}", err.message))
        })?;

        // codex.go:188-192 — encode failure → 500.
        creds.to_json().map_err(|err| {
            OAuthServerError::Internal(format!("failed to encode credentials: {}", err.message))
        })
    }

    /// Antigravity `StartOAuth` (antigravity.go:89-138): PKCE mint + cache
    /// (the `project_id` rides along in the cached state so the exchange can
    /// fall back to it, antigravity.go:115-119 / 232) + Google authorize URL.
    async fn start_antigravity_oauth(
        &self,
        project_id: String,
    ) -> Result<StartAntigravityOAuthResponse, OAuthServerError> {
        let (state, code_verifier, challenge) = Self::generate_pkce();
        // antigravity.go:114-122 — cache Set failure → 500 "failed to save
        // oauth state".
        self.store.set(
            format!("antigravity:oauth:{state}"),
            PendingState::AntigravityPkce {
                code_verifier,
                project_id,
            },
            PKCE_STATE_TTL_SECS,
        )?;

        Ok(StartAntigravityOAuthResponse {
            auth_url: antigravity_authorize_url(&state, &challenge),
            session_id: state,
        })
    }

    /// Antigravity `Exchange` (antigravity.go:178-247): validate the cached
    /// session (deleted only AFTER validation succeeds, antigravity.go:
    /// 206-209 — unlike Codex), perform the live Google token exchange, and
    /// return the `refreshToken|projectId` credentials format
    /// (antigravity.go:243-246).
    async fn exchange_antigravity(
        &self,
        request: ExchangeOAuthRequest,
    ) -> Result<String, OAuthServerError> {
        let key = format!("antigravity:oauth:{}", request.session_id);
        // antigravity.go:189-193 — cache miss → 400 (non-consuming Get).
        let Some(PendingState::AntigravityPkce {
            code_verifier,
            project_id,
        }) = self.store.get(&key)?
        else {
            return Err(OAuthServerError::BadRequest(
                "invalid or expired oauth session".to_string(),
            ));
        };

        // antigravity.go:195-199 — query-only callback (`code` + `state`).
        let (code, callback_state) = parse_callback_url(&request.callback_url, false)
            .map_err(OAuthServerError::BadRequest)?;

        // antigravity.go:201-204.
        if callback_state != request.session_id {
            return Err(OAuthServerError::BadRequest(
                "oauth state mismatch".to_string(),
            ));
        }

        // antigravity.go:206-209 — delete the state after validation.
        self.store.remove(&key)?;

        // antigravity.go:217-229 — form-encoded exchange against Google's
        // token endpoint (includes the public client_secret,
        // token_provider.go:40-63).
        let http_request = anti_tf::build_exchange_request(
            anti_tf::CLIENT_ID,
            &code,
            anti_tf::REDIRECT_URI,
            &code_verifier,
            None,
        )
        .map_err(|err| OAuthServerError::BadGateway(wrap_token_exchange_message(&err.message)))?;

        let creds = self
            .execute_token_exchange(http_request, request.proxy.as_ref(), anti_tf::CLIENT_ID)
            .await?;

        // antigravity.go:232-241 — cached project_id wins; otherwise Go runs
        // resolveProjectID (loadCodeAssist across LoadEndpoints) + onboardUser
        // with retry/sleep loops (antigravity.go:249-405).
        //
        // DEFER: the resolveProjectID/onboardUser fallback is a multi-endpoint
        // Google-internal probe with retry loops that is not wired into the
        // host yet. Its failure path in Go is exactly this 502
        // (antigravity.go:235-240), so an empty project_id surfaces the same
        // status + message prefix instead of fabricating the probe.
        if project_id.is_empty() {
            return Err(OAuthServerError::BadGateway(
                "failed to resolve project id and none provided: project resolution is not wired \
                 in the Rust host (Go antigravity.go:249-405)"
                    .to_string(),
            ));
        }

        // antigravity.go:243-246 — `refreshToken|projectId`.
        Ok(format!("{}|{project_id}", creds.refresh_token))
    }

    /// Copilot `StartOAuth` (copilot.go:155-215): mint a session ID, POST
    /// GitHub's device-code endpoint (live HTTP), cache the device-flow
    /// state with `min(expires_in, 15min)` TTL, and return the
    /// user_code/verification_uri/expires_in/interval quad.
    async fn start_copilot_oauth(
        &self,
        proxy: Option<ProxyConfig>,
    ) -> Result<StartCopilotOAuthResponse, OAuthServerError> {
        // copilot.go:168-171 — session-ID generation failure → 500. The
        // platform CSPRNG behind generate_secret_key is infallible, so that
        // arm has no Rust equivalent.
        let session_id = generate_secret_key();

        // copilot.go:218-229 — form post `client_id` + `scope`.
        let client_id = github_copilot_client_id();
        let form = encode_query(&[
            ("client_id", client_id.as_str()),
            ("scope", GITHUB_COPILOT_SCOPE),
        ]);
        let http_request = HttpRequest {
            method: "POST".to_string(),
            url: Some(GITHUB_DEVICE_CODE_URL.to_string()),
            headers: github_form_headers(),
            body: Some(form.into_bytes()),
            ..HttpRequest::default()
        };

        // copilot.go:181-185 — any requestDeviceCode failure → 502 "failed
        // to request device code: …" (transport :231-234, non-2xx :237-239,
        // decode :241-244, empty device code :246-248).
        let response = self
            .http
            .execute(http_request, proxy.as_ref())
            .await
            .map_err(|err| {
                OAuthServerError::BadGateway(format!(
                    "failed to request device code: device code request failed: {err}"
                ))
            })?;
        if !(200..300).contains(&response.status) {
            return Err(OAuthServerError::BadGateway(format!(
                "failed to request device code: device code request failed with status {}: {}",
                response.status,
                String::from_utf8_lossy(&response.body)
            )));
        }
        let device: DeviceCodeResponse = serde_json::from_slice(&response.body).map_err(|err| {
            OAuthServerError::BadGateway(format!(
                "failed to request device code: failed to parse device code response: {err}"
            ))
        })?;
        if device.device_code.is_empty() {
            return Err(OAuthServerError::BadGateway(
                "failed to request device code: device code not received from GitHub".to_string(),
            ));
        }

        // copilot.go:187-206 — cache the device-flow state; TTL is the
        // GitHub-provided expiry capped at 15 minutes.
        let ttl = device.expires_in.min(DEVICE_FLOW_CACHE_EXPIRATION_SECS);
        self.store.set(
            format!("copilot:oauth:{session_id}"),
            PendingState::CopilotDevice {
                device_code: device.device_code,
                expires_in: device.expires_in,
                created_at: Utc::now().timestamp(),
            },
            ttl,
        )?;

        Ok(StartCopilotOAuthResponse {
            session_id,
            user_code: device.user_code,
            verification_uri: device.verification_uri,
            expires_in: device.expires_in,
            interval: device.interval,
        })
    }

    /// Copilot `PollOAuth` (copilot.go:270-356): look up the cached
    /// device-flow state, enforce expiry, poll GitHub's access-token endpoint
    /// (live HTTP), and map GitHub's OAuth error strings to the status enum.
    /// Terminal states clean the cache entry up (copilot.go:324/328/340).
    async fn poll_copilot_oauth(
        &self,
        request: PollCopilotOAuthRequest,
    ) -> Result<CopilotPollStatus, OAuthServerError> {
        let key = format!("copilot:oauth:{}", request.session_id);
        // copilot.go:281-285 — cache miss → 400.
        let Some(PendingState::CopilotDevice {
            device_code,
            expires_in,
            created_at,
        }) = self.store.get(&key)?
        else {
            return Err(OAuthServerError::BadRequest(
                "invalid or expired session".to_string(),
            ));
        };

        // copilot.go:288-292 — device-code expiry check independent of the
        // cache TTL.
        if Utc::now().timestamp() > created_at + expires_in {
            self.store.remove(&key)?;
            return Err(OAuthServerError::BadRequest(
                "device code expired".to_string(),
            ));
        }

        // copilot.go:359-371 — form post client_id + device_code + grant_type.
        let client_id = github_copilot_client_id();
        let form = encode_query(&[
            ("client_id", client_id.as_str()),
            ("device_code", device_code.as_str()),
            ("grant_type", DEVICE_GRANT_TYPE),
        ]);
        let http_request = HttpRequest {
            method: "POST".to_string(),
            url: Some(GITHUB_ACCESS_TOKEN_URL.to_string()),
            headers: github_form_headers(),
            body: Some(form.into_bytes()),
            ..HttpRequest::default()
        };

        // copilot.go:301-305 — transport / non-2xx / parse failures → 502
        // "token poll failed: …" (:373-376, :379-381, :388-397).
        let response = self
            .http
            .execute(http_request, request.proxy.as_ref())
            .await
            .map_err(|err| {
                OAuthServerError::BadGateway(format!(
                    "token poll failed: access token request failed: {err}"
                ))
            })?;
        if !(200..300).contains(&response.status) {
            return Err(OAuthServerError::BadGateway(format!(
                "token poll failed: access token request failed with status {}",
                response.status
            )));
        }
        let token = parse_access_token_response(&response)
            .map_err(|err| OAuthServerError::BadGateway(format!("token poll failed: {err}")))?;

        // copilot.go:308-335 — GitHub OAuth error-string mapping.
        if !token.error.is_empty() {
            return match token.error.as_str() {
                "authorization_pending" => Ok(CopilotPollStatus::Pending),
                "slow_down" => Ok(CopilotPollStatus::SlowDown),
                "expired_token" => {
                    self.store.remove(&key)?;
                    Err(OAuthServerError::BadRequest(
                        "device code expired".to_string(),
                    ))
                }
                "access_denied" => {
                    self.store.remove(&key)?;
                    Err(OAuthServerError::BadRequest(
                        "access denied by user".to_string(),
                    ))
                }
                _ => Err(OAuthServerError::BadGateway(format!(
                    "OAuth error: {} - {}",
                    token.error, token.error_description
                ))),
            };
        }

        // copilot.go:338-352 — success: clean up + complete.
        if !token.access_token.is_empty() {
            self.store.remove(&key)?;
            return Ok(CopilotPollStatus::Complete {
                access_token: token.access_token,
                token_type: token.token_type,
                scope: token.scope,
            });
        }

        // copilot.go:354-355.
        Err(OAuthServerError::Internal(
            "unexpected response from GitHub".to_string(),
        ))
    }
}

// ===========================================================================
// Tests — golden cases mirroring the http crate's InMemory mock semantics and
// the Go handler behaviors, exercised against the REAL adapter with a mock
// token-HTTP executor (the only live-network seam).
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use serde_json::Value;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Mock token-HTTP executor: pops canned responses in order and records
    /// every outbound request + proxy for assertions.
    #[derive(Default)]
    struct MockTokenHttp {
        responses: Mutex<VecDeque<Result<OAuthHttpResponse, String>>>,
        seen: Mutex<Vec<(HttpRequest, Option<ProxyConfig>)>>,
    }

    #[async_trait]
    impl OAuthTokenHttp for MockTokenHttp {
        async fn execute(
            &self,
            request: HttpRequest,
            proxy: Option<&ProxyConfig>,
        ) -> Result<OAuthHttpResponse, String> {
            if let Ok(mut seen) = self.seen.lock() {
                seen.push((request, proxy.cloned()));
            }
            let mut guard = self
                .responses
                .lock()
                .map_err(|_| "mock lock poisoned".to_string())?;
            guard
                .pop_front()
                .unwrap_or_else(|| Err("no mock response queued".to_string()))
        }
    }

    fn adapter_with(
        responses: Vec<Result<OAuthHttpResponse, String>>,
    ) -> (Arc<MockTokenHttp>, OAuthAdminAdapter) {
        let http = Arc::new(MockTokenHttp {
            responses: Mutex::new(responses.into_iter().collect()),
            seen: Mutex::new(Vec::new()),
        });
        let adapter = OAuthAdminAdapter::with_http(Arc::clone(&http) as Arc<dyn OAuthTokenHttp>);
        (http, adapter)
    }

    fn ok_json(body: &str) -> Result<OAuthHttpResponse, String> {
        Ok(OAuthHttpResponse {
            status: 200,
            content_type: "application/json".to_string(),
            body: body.as_bytes().to_vec(),
        })
    }

    /// Read the stored PKCE verifier for a session (test-only store access).
    fn stored_verifier(adapter: &OAuthAdminAdapter, key: &str) -> Result<String, String> {
        let guard = adapter
            .store
            .entries
            .lock()
            .map_err(|_| "store poisoned".to_string())?;
        match guard.get(key).map(|entry| &entry.state) {
            Some(PendingState::Pkce { code_verifier })
            | Some(PendingState::AntigravityPkce { code_verifier, .. }) => {
                Ok(code_verifier.clone())
            }
            _ => Err(format!("no pkce state under {key}")),
        }
    }

    fn seen_requests(http: &MockTokenHttp) -> Vec<(HttpRequest, Option<ProxyConfig>)> {
        http.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }

    fn body_string(request: &HttpRequest) -> String {
        request
            .body
            .as_ref()
            .map(|body| String::from_utf8_lossy(body).into_owned())
            .unwrap_or_default()
    }

    // A canned provider token-endpoint success body (oauth.TokenResponse
    // wire shape, credentials.go:20-27).
    const TOKEN_OK: &str = r#"{"access_token":"mock-access","refresh_token":"mock-refresh","expires_in":3600,"token_type":"Bearer","scope":"openid profile"}"#;

    // ---- start_oauth -------------------------------------------------------

    /// codex.go:84-127 — session persisted, authorize URL carries the sorted
    /// query, PKCE challenge, and the two Codex-only extras.
    #[tokio::test]
    async fn codex_start_oauth_builds_authorize_url_and_persists_state() -> TestResult {
        let (_http, adapter) = adapter_with(Vec::new());
        let response = adapter
            .start_oauth(OAuthProvider::Codex)
            .await
            .map_err(|err| err.message())?;

        // The verifier is persisted under the Go cache key and the URL's
        // code_challenge is exactly pkce_challenge(verifier).
        let key = format!("codex:oauth:{}", response.session_id);
        let verifier = stored_verifier(&adapter, &key)?;
        let expected = codex_authorize_url(&response.session_id, &pkce_challenge(&verifier));
        assert_eq!(response.auth_url, expected);

        assert!(
            response
                .auth_url
                .starts_with("https://auth.openai.com/oauth/authorize?"),
            "{}",
            response.auth_url
        );
        assert!(response.auth_url.contains("codex_cli_simplified_flow=true"));
        assert!(
            response
                .auth_url
                .contains("id_token_add_organizations=true")
        );
        assert!(
            response
                .auth_url
                .contains(&format!("state={}", response.session_id))
        );
        // Go url.Values.Encode sorts keys: client_id precedes code_challenge.
        let cid = response.auth_url.find("client_id=").unwrap_or(usize::MAX);
        let cc = response.auth_url.find("code_challenge=").unwrap_or(0);
        assert!(cid < cc, "{}", response.auth_url);
        Ok(())
    }

    /// claudecode.go:84-125 — Claude URL has no Codex-only extras.
    #[tokio::test]
    async fn claude_start_oauth_builds_authorize_url() -> TestResult {
        let (_http, adapter) = adapter_with(Vec::new());
        let response = adapter
            .start_oauth(OAuthProvider::ClaudeCode)
            .await
            .map_err(|err| err.message())?;

        assert!(
            response
                .auth_url
                .starts_with("https://claude.ai/oauth/authorize?"),
            "{}",
            response.auth_url
        );
        assert!(!response.auth_url.contains("codex_cli_simplified_flow"));
        assert!(
            response
                .auth_url
                .contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e")
        );
        let key = format!("claudecode:oauth:{}", response.session_id);
        assert!(stored_verifier(&adapter, &key).is_ok());
        Ok(())
    }

    /// antigravity.go:89-138 — Google URL with access_type/prompt extras;
    /// project_id rides along in the cached state.
    #[tokio::test]
    async fn antigravity_start_oauth_builds_url_and_caches_project_id() -> TestResult {
        let (_http, adapter) = adapter_with(Vec::new());
        let response = adapter
            .start_antigravity_oauth("proj-123".to_string())
            .await
            .map_err(|err| err.message())?;

        assert!(
            response
                .auth_url
                .starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
            "{}",
            response.auth_url
        );
        assert!(response.auth_url.contains("access_type=offline"));
        assert!(response.auth_url.contains("prompt=consent"));

        let key = format!("antigravity:oauth:{}", response.session_id);
        let guard = adapter
            .store
            .entries
            .lock()
            .map_err(|_| "store poisoned".to_string())?;
        match guard.get(&key).map(|entry| &entry.state) {
            Some(PendingState::AntigravityPkce { project_id, .. }) => {
                assert_eq!(project_id, "proj-123");
            }
            other => return Err(format!("unexpected state: {other:?}").into()),
        }
        Ok(())
    }

    // ---- exchange ----------------------------------------------------------

    /// codex.go:199-264 happy path: state consumed once, token endpoint hit
    /// with the form body, credentials JSON returned.
    #[tokio::test]
    async fn codex_exchange_happy_path_consumes_state_and_returns_credentials() -> TestResult {
        let (http, adapter) = adapter_with(vec![ok_json(TOKEN_OK)]);
        let start = adapter
            .start_oauth(OAuthProvider::Codex)
            .await
            .map_err(|err| err.message())?;
        let session_id = start.session_id.clone();

        let credentials = adapter
            .exchange(
                OAuthProvider::Codex,
                ExchangeOAuthRequest {
                    session_id: session_id.clone(),
                    callback_url: format!(
                        "http://localhost:1455/auth/callback?code=abc&state={session_id}"
                    ),
                    proxy: None,
                },
            )
            .await
            .map_err(|err| err.message())?;

        // OAuthCredentials.ToJSON parity — decode and check fields.
        let parsed: Value = serde_json::from_str(&credentials)?;
        assert_eq!(parsed["client_id"], codex_tf::CLIENT_ID);
        assert_eq!(parsed["access_token"], "mock-access");
        assert_eq!(parsed["refresh_token"], "mock-refresh");

        // Outbound request went to the Codex token endpoint with the
        // form-encoded PKCE exchange body (no state — form strategy).
        let seen = seen_requests(&http);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0.url.as_deref(), Some(codex_tf::TOKEN_URL));
        let body = body_string(&seen[0].0);
        assert!(body.contains("grant_type=authorization_code"), "{body}");
        assert!(body.contains("code=abc"), "{body}");
        assert!(body.contains("code_verifier="), "{body}");
        assert!(!body.contains("state="), "{body}");

        // State is consumed — a replay is an invalid session (codex.go:221).
        let replay = adapter
            .exchange(
                OAuthProvider::Codex,
                ExchangeOAuthRequest {
                    session_id: session_id.clone(),
                    callback_url: format!(
                        "http://localhost:1455/auth/callback?code=abc&state={session_id}"
                    ),
                    proxy: None,
                },
            )
            .await;
        match replay {
            Err(OAuthServerError::BadRequest(message)) => {
                assert_eq!(message, "invalid or expired oauth session");
            }
            other => return Err(format!("expected invalid session, got {other:?}").into()),
        }
        Ok(())
    }

    /// claudecode.go:173-234 — Claude reads the state from the URL fragment
    /// and the JSON token request carries the state field.
    #[tokio::test]
    async fn claude_exchange_accepts_fragment_state_and_sends_state_in_body() -> TestResult {
        let (http, adapter) = adapter_with(vec![ok_json(TOKEN_OK)]);
        let start = adapter
            .start_oauth(OAuthProvider::ClaudeCode)
            .await
            .map_err(|err| err.message())?;
        let session_id = start.session_id.clone();

        let credentials = adapter
            .exchange(
                OAuthProvider::ClaudeCode,
                ExchangeOAuthRequest {
                    session_id: session_id.clone(),
                    // Claude callback shape: ?code=xxx#state (claudecode.go:155-157).
                    callback_url: format!("http://localhost:54545/callback?code=abc#{session_id}"),
                    proxy: None,
                },
            )
            .await
            .map_err(|err| err.message())?;

        let parsed: Value = serde_json::from_str(&credentials)?;
        assert_eq!(parsed["client_id"], claude_tf::CLIENT_ID);

        let seen = seen_requests(&http);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0.url.as_deref(), Some(claude_tf::TOKEN_URL));
        // JSON strategy body includes the state (exchange_strategy.go:99-102).
        let body: Value = serde_json::from_str(&body_string(&seen[0].0))?;
        assert_eq!(body["state"], session_id.as_str());
        assert_eq!(body["grant_type"], "authorization_code");
        Ok(())
    }

    /// codex.go:217 — unknown session → 400 without touching the network.
    #[tokio::test]
    async fn exchange_unknown_session_is_bad_request() -> TestResult {
        let (http, adapter) = adapter_with(Vec::new());
        let result = adapter
            .exchange(
                OAuthProvider::Codex,
                ExchangeOAuthRequest {
                    session_id: "never-issued".to_string(),
                    callback_url: "http://localhost:1455/auth/callback?code=abc&state=never-issued"
                        .to_string(),
                    proxy: None,
                },
            )
            .await;
        match result {
            Err(OAuthServerError::BadRequest(message)) => {
                assert_eq!(message, "invalid or expired oauth session");
            }
            other => return Err(format!("expected 400, got {other:?}").into()),
        }
        assert!(seen_requests(&http).is_empty());
        Ok(())
    }

    /// codex.go:231-234 — callback state != session → 400 "oauth state
    /// mismatch"; codex.go:225-229 — missing code → the parser error verbatim.
    #[tokio::test]
    async fn exchange_validates_callback_state_and_code() -> TestResult {
        let (_http, adapter) = adapter_with(Vec::new());
        let start = adapter
            .start_oauth(OAuthProvider::Codex)
            .await
            .map_err(|err| err.message())?;
        let session_id = start.session_id.clone();

        let mismatch = adapter
            .exchange(
                OAuthProvider::Codex,
                ExchangeOAuthRequest {
                    session_id: session_id.clone(),
                    callback_url: "http://localhost:1455/auth/callback?code=abc&state=other"
                        .to_string(),
                    proxy: None,
                },
            )
            .await;
        match mismatch {
            Err(OAuthServerError::BadRequest(message)) => {
                assert_eq!(message, "oauth state mismatch");
            }
            other => return Err(format!("expected mismatch, got {other:?}").into()),
        }

        // A fresh session (the previous one was consumed by the Get+Delete).
        let start = adapter
            .start_oauth(OAuthProvider::Codex)
            .await
            .map_err(|err| err.message())?;
        let session_id = start.session_id.clone();
        let missing_code = adapter
            .exchange(
                OAuthProvider::Codex,
                ExchangeOAuthRequest {
                    session_id: session_id.clone(),
                    callback_url: format!("http://localhost:1455/auth/callback?state={session_id}"),
                    proxy: None,
                },
            )
            .await;
        match missing_code {
            Err(OAuthServerError::BadRequest(message)) => {
                assert_eq!(message, "code parameter not found in callback_url");
            }
            other => return Err(format!("expected missing-code error, got {other:?}").into()),
        }
        Ok(())
    }

    /// codex.go:253 — token endpoint failure → 502 with the
    /// "token exchange failed:" prefix (both transport errors and non-2xx).
    #[tokio::test]
    async fn exchange_token_endpoint_failure_is_bad_gateway() -> TestResult {
        let (_http, adapter) = adapter_with(vec![Ok(OAuthHttpResponse {
            status: 500,
            content_type: "text/plain".to_string(),
            body: b"boom".to_vec(),
        })]);
        let start = adapter
            .start_oauth(OAuthProvider::Codex)
            .await
            .map_err(|err| err.message())?;
        let session_id = start.session_id.clone();

        let result = adapter
            .exchange(
                OAuthProvider::Codex,
                ExchangeOAuthRequest {
                    session_id: session_id.clone(),
                    callback_url: format!(
                        "http://localhost:1455/auth/callback?code=abc&state={session_id}"
                    ),
                    proxy: None,
                },
            )
            .await;
        match result {
            Err(OAuthServerError::BadGateway(message)) => {
                assert!(message.starts_with("token exchange failed:"), "{message}");
            }
            other => return Err(format!("expected 502, got {other:?}").into()),
        }
        Ok(())
    }

    /// codex.go:237-240 — the request proxy config reaches the HTTP seam.
    #[tokio::test]
    async fn exchange_passes_proxy_to_http_executor() -> TestResult {
        let (http, adapter) = adapter_with(vec![ok_json(TOKEN_OK)]);
        let start = adapter
            .start_oauth(OAuthProvider::Codex)
            .await
            .map_err(|err| err.message())?;
        let session_id = start.session_id.clone();

        adapter
            .exchange(
                OAuthProvider::Codex,
                ExchangeOAuthRequest {
                    session_id: session_id.clone(),
                    callback_url: format!(
                        "http://localhost:1455/auth/callback?code=abc&state={session_id}"
                    ),
                    proxy: Some(ProxyConfig {
                        r#type: "url".to_string(),
                        url: "http://proxy.local:8080".to_string(),
                    }),
                },
            )
            .await
            .map_err(|err| err.message())?;

        let seen = seen_requests(&http);
        assert_eq!(seen.len(), 1);
        let proxy = seen[0].1.clone().ok_or("proxy not forwarded")?;
        assert_eq!(proxy.r#type, "url");
        assert_eq!(proxy.url, "http://proxy.local:8080");
        Ok(())
    }

    // ---- decode_auth_json --------------------------------------------------

    /// codex.go:175-195 happy path — mirrors the Go codex/token_test.go
    /// golden shape: tokens + last_refresh → credentials JSON with the Codex
    /// client_id, bearer token type, and last_refresh+1h expiry.
    #[tokio::test]
    async fn decode_auth_json_happy_path() -> TestResult {
        let (_http, adapter) = adapter_with(Vec::new());
        let credentials = adapter
            .decode_auth_json(
                r#"{"tokens":{"access_token":"at","refresh_token":"rt","id_token":"idt"},"last_refresh":"2024-01-01T00:00:00Z"}"#
                    .to_string(),
            )
            .await
            .map_err(|err| err.message())?;

        let parsed: Value = serde_json::from_str(&credentials)?;
        assert_eq!(parsed["client_id"], codex_tf::CLIENT_ID);
        assert_eq!(parsed["access_token"], "at");
        assert_eq!(parsed["refresh_token"], "rt");
        assert_eq!(parsed["id_token"], "idt");
        assert_eq!(parsed["token_type"], "bearer");
        // last_refresh + 1h (token.go:59-64).
        assert_eq!(parsed["expires_at"], "2024-01-01T01:00:00Z");
        Ok(())
    }

    /// codex.go:182-186 — decode failure → 400 "failed to decode auth json:".
    #[tokio::test]
    async fn decode_auth_json_failure_is_bad_request() -> TestResult {
        let (_http, adapter) = adapter_with(Vec::new());
        let result = adapter.decode_auth_json("not json".to_string()).await;
        match result {
            Err(OAuthServerError::BadRequest(message)) => {
                assert!(
                    message.starts_with("failed to decode auth json:"),
                    "{message}"
                );
            }
            other => return Err(format!("expected 400, got {other:?}").into()),
        }
        Ok(())
    }

    // ---- exchange_antigravity ----------------------------------------------

    /// antigravity.go:178-247 happy path — `refreshToken|projectId` format,
    /// Google token endpoint hit with the client_secret form body.
    #[tokio::test]
    async fn antigravity_exchange_returns_refresh_token_pipe_project_id() -> TestResult {
        let (http, adapter) = adapter_with(vec![ok_json(TOKEN_OK)]);
        let start = adapter
            .start_antigravity_oauth("proj-123".to_string())
            .await
            .map_err(|err| err.message())?;
        let session_id = start.session_id.clone();

        let credentials = adapter
            .exchange_antigravity(ExchangeOAuthRequest {
                session_id: session_id.clone(),
                callback_url: format!(
                    "http://localhost:51121/oauth-callback?code=abc&state={session_id}"
                ),
                proxy: None,
            })
            .await
            .map_err(|err| err.message())?;
        assert_eq!(credentials, "mock-refresh|proj-123");

        let seen = seen_requests(&http);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0.url.as_deref(), Some(anti_tf::TOKEN_URL));
        let body = body_string(&seen[0].0);
        assert!(body.contains("client_secret="), "{body}");
        assert!(body.contains("grant_type=authorization_code"), "{body}");

        // State was deleted after validation — replay is invalid.
        let replay = adapter
            .exchange_antigravity(ExchangeOAuthRequest {
                session_id: session_id.clone(),
                callback_url: format!(
                    "http://localhost:51121/oauth-callback?code=abc&state={session_id}"
                ),
                proxy: None,
            })
            .await;
        assert!(matches!(replay, Err(OAuthServerError::BadRequest(_))));
        Ok(())
    }

    /// DEFER pin — empty project_id surfaces Go's resolution-failure 502
    /// (antigravity.go:235-240) because resolveProjectID/onboardUser is not
    /// wired in the host.
    #[tokio::test]
    async fn antigravity_exchange_empty_project_id_defers_resolution() -> TestResult {
        let (_http, adapter) = adapter_with(vec![ok_json(TOKEN_OK)]);
        let start = adapter
            .start_antigravity_oauth(String::new())
            .await
            .map_err(|err| err.message())?;
        let session_id = start.session_id.clone();

        let result = adapter
            .exchange_antigravity(ExchangeOAuthRequest {
                session_id: session_id.clone(),
                callback_url: format!(
                    "http://localhost:51121/oauth-callback?code=abc&state={session_id}"
                ),
                proxy: None,
            })
            .await;
        match result {
            Err(OAuthServerError::BadGateway(message)) => {
                assert!(
                    message.starts_with("failed to resolve project id and none provided:"),
                    "{message}"
                );
            }
            other => return Err(format!("expected DEFER 502, got {other:?}").into()),
        }
        Ok(())
    }

    /// antigravity.go:201-204 — state mismatch does NOT consume the cached
    /// session (delete happens only after validation succeeds).
    #[tokio::test]
    async fn antigravity_state_mismatch_keeps_session_alive() -> TestResult {
        let (_http, adapter) = adapter_with(vec![ok_json(TOKEN_OK)]);
        let start = adapter
            .start_antigravity_oauth("proj-1".to_string())
            .await
            .map_err(|err| err.message())?;
        let session_id = start.session_id.clone();

        let mismatch = adapter
            .exchange_antigravity(ExchangeOAuthRequest {
                session_id: session_id.clone(),
                callback_url: "http://localhost:51121/oauth-callback?code=abc&state=other"
                    .to_string(),
                proxy: None,
            })
            .await;
        match mismatch {
            Err(OAuthServerError::BadRequest(message)) => {
                assert_eq!(message, "oauth state mismatch");
            }
            other => return Err(format!("expected mismatch, got {other:?}").into()),
        }

        // The session survives the failed attempt and the retry succeeds.
        let credentials = adapter
            .exchange_antigravity(ExchangeOAuthRequest {
                session_id: session_id.clone(),
                callback_url: format!(
                    "http://localhost:51121/oauth-callback?code=abc&state={session_id}"
                ),
                proxy: None,
            })
            .await
            .map_err(|err| err.message())?;
        assert_eq!(credentials, "mock-refresh|proj-1");
        Ok(())
    }

    // ---- copilot -----------------------------------------------------------

    const DEVICE_OK: &str = r#"{"device_code":"dc-1","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#;

    /// copilot.go:155-215 happy path — device-code endpoint hit, state cached,
    /// response mirrors GitHub's quad.
    #[tokio::test]
    async fn copilot_start_oauth_requests_device_code_and_caches_state() -> TestResult {
        let (http, adapter) = adapter_with(vec![ok_json(DEVICE_OK)]);
        let response = adapter
            .start_copilot_oauth(None)
            .await
            .map_err(|err| err.message())?;

        assert_eq!(response.user_code, "ABCD-1234");
        assert_eq!(response.verification_uri, "https://github.com/login/device");
        assert_eq!(response.expires_in, 900);
        assert_eq!(response.interval, 5);

        let seen = seen_requests(&http);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0.url.as_deref(), Some(GITHUB_DEVICE_CODE_URL));
        let body = body_string(&seen[0].0);
        assert!(body.contains("client_id="), "{body}");
        // Go url.QueryEscape escapes ':' → read%3Auser.
        assert!(body.contains("scope=read%3Auser"), "{body}");

        // Device state persisted under the Go cache key.
        let key = format!("copilot:oauth:{}", response.session_id);
        let guard = adapter
            .store
            .entries
            .lock()
            .map_err(|_| "store poisoned".to_string())?;
        match guard.get(&key).map(|entry| &entry.state) {
            Some(PendingState::CopilotDevice {
                device_code,
                expires_in,
                ..
            }) => {
                assert_eq!(device_code, "dc-1");
                assert_eq!(*expires_in, 900);
            }
            other => return Err(format!("unexpected state: {other:?}").into()),
        }
        Ok(())
    }

    /// copilot.go:181-185 — device-code endpoint failure → 502 with the
    /// "failed to request device code:" prefix.
    #[tokio::test]
    async fn copilot_start_oauth_device_code_failure_is_bad_gateway() -> TestResult {
        let (_http, adapter) = adapter_with(vec![Err("connection refused".to_string())]);
        let result = adapter.start_copilot_oauth(None).await;
        match result {
            Err(OAuthServerError::BadGateway(message)) => {
                assert!(
                    message.starts_with("failed to request device code:"),
                    "{message}"
                );
            }
            other => return Err(format!("expected 502, got {other:?}").into()),
        }
        Ok(())
    }

    /// copilot.go:281-285 — unknown session → 400 without polling GitHub.
    #[tokio::test]
    async fn copilot_poll_unknown_session_is_bad_request() -> TestResult {
        let (http, adapter) = adapter_with(Vec::new());
        let result = adapter
            .poll_copilot_oauth(PollCopilotOAuthRequest {
                session_id: "never-issued".to_string(),
                proxy: None,
            })
            .await;
        match result {
            Err(OAuthServerError::BadRequest(message)) => {
                assert_eq!(message, "invalid or expired session");
            }
            other => return Err(format!("expected 400, got {other:?}").into()),
        }
        assert!(seen_requests(&http).is_empty());
        Ok(())
    }

    /// copilot.go:308-321 — authorization_pending / slow_down map to the
    /// 200-status enum variants (no cache cleanup).
    #[tokio::test]
    async fn copilot_poll_pending_and_slow_down_statuses() -> TestResult {
        let (_http, adapter) = adapter_with(vec![
            ok_json(DEVICE_OK),
            ok_json(r#"{"error":"authorization_pending"}"#),
            ok_json(r#"{"error":"slow_down"}"#),
        ]);
        let start = adapter
            .start_copilot_oauth(None)
            .await
            .map_err(|err| err.message())?;

        let pending = adapter
            .poll_copilot_oauth(PollCopilotOAuthRequest {
                session_id: start.session_id.clone(),
                proxy: None,
            })
            .await
            .map_err(|err| err.message())?;
        assert_eq!(pending, CopilotPollStatus::Pending);

        let slow_down = adapter
            .poll_copilot_oauth(PollCopilotOAuthRequest {
                session_id: start.session_id.clone(),
                proxy: None,
            })
            .await
            .map_err(|err| err.message())?;
        assert_eq!(slow_down, CopilotPollStatus::SlowDown);
        Ok(())
    }

    /// copilot.go:337-352 — success completes with the token triple, cleans
    /// the cache (replay → 400), and the poll body carries the device grant.
    #[tokio::test]
    async fn copilot_poll_complete_cleans_cache() -> TestResult {
        let (http, adapter) = adapter_with(vec![
            ok_json(DEVICE_OK),
            ok_json(r#"{"access_token":"gho_mock","token_type":"bearer","scope":"read:user"}"#),
        ]);
        let start = adapter
            .start_copilot_oauth(None)
            .await
            .map_err(|err| err.message())?;

        let complete = adapter
            .poll_copilot_oauth(PollCopilotOAuthRequest {
                session_id: start.session_id.clone(),
                proxy: None,
            })
            .await
            .map_err(|err| err.message())?;
        assert_eq!(
            complete,
            CopilotPollStatus::Complete {
                access_token: "gho_mock".to_string(),
                token_type: "bearer".to_string(),
                scope: "read:user".to_string(),
            }
        );

        // The poll request carried the device_code + RFC 8628 grant type.
        let seen = seen_requests(&http);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].0.url.as_deref(), Some(GITHUB_ACCESS_TOKEN_URL));
        let body = body_string(&seen[1].0);
        assert!(body.contains("device_code=dc-1"), "{body}");
        assert!(
            body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"),
            "{body}"
        );

        // Terminal state cleanup (copilot.go:340).
        let replay = adapter
            .poll_copilot_oauth(PollCopilotOAuthRequest {
                session_id: start.session_id.clone(),
                proxy: None,
            })
            .await;
        assert!(matches!(replay, Err(OAuthServerError::BadRequest(_))));
        Ok(())
    }

    /// copilot.go:323-334 — expired_token / access_denied are 400 with cache
    /// cleanup; unknown OAuth errors are 502.
    #[tokio::test]
    async fn copilot_poll_error_string_mapping() -> TestResult {
        let (_http, adapter) = adapter_with(vec![
            ok_json(DEVICE_OK),
            ok_json(r#"{"error":"access_denied"}"#),
            // Second session for the unknown-error case.
            ok_json(DEVICE_OK),
            ok_json(r#"{"error":"incorrect_device_code","error_description":"bad code"}"#),
        ]);

        let start = adapter
            .start_copilot_oauth(None)
            .await
            .map_err(|err| err.message())?;
        let denied = adapter
            .poll_copilot_oauth(PollCopilotOAuthRequest {
                session_id: start.session_id.clone(),
                proxy: None,
            })
            .await;
        match denied {
            Err(OAuthServerError::BadRequest(message)) => {
                assert_eq!(message, "access denied by user");
            }
            other => return Err(format!("expected access denied, got {other:?}").into()),
        }
        // Terminal state cleanup (copilot.go:328).
        let replay = adapter
            .poll_copilot_oauth(PollCopilotOAuthRequest {
                session_id: start.session_id.clone(),
                proxy: None,
            })
            .await;
        assert!(matches!(replay, Err(OAuthServerError::BadRequest(_))));

        let start2 = adapter
            .start_copilot_oauth(None)
            .await
            .map_err(|err| err.message())?;
        let unknown = adapter
            .poll_copilot_oauth(PollCopilotOAuthRequest {
                session_id: start2.session_id.clone(),
                proxy: None,
            })
            .await;
        match unknown {
            Err(OAuthServerError::BadGateway(message)) => {
                assert_eq!(message, "OAuth error: incorrect_device_code - bad code");
            }
            other => return Err(format!("expected 502, got {other:?}").into()),
        }
        Ok(())
    }

    /// copilot.go:383-404 — a form-encoded GitHub response is parsed when the
    /// Content-Type is not JSON.
    #[tokio::test]
    async fn copilot_poll_parses_form_encoded_response() -> TestResult {
        let (_http, adapter) = adapter_with(vec![
            ok_json(DEVICE_OK),
            Ok(OAuthHttpResponse {
                status: 200,
                content_type: "application/x-www-form-urlencoded".to_string(),
                body: b"access_token=gho_form&token_type=bearer&scope=read%3Auser".to_vec(),
            }),
        ]);
        let start = adapter
            .start_copilot_oauth(None)
            .await
            .map_err(|err| err.message())?;

        let complete = adapter
            .poll_copilot_oauth(PollCopilotOAuthRequest {
                session_id: start.session_id.clone(),
                proxy: None,
            })
            .await
            .map_err(|err| err.message())?;
        assert_eq!(
            complete,
            CopilotPollStatus::Complete {
                access_token: "gho_form".to_string(),
                token_type: "bearer".to_string(),
                scope: "read:user".to_string(),
            }
        );
        Ok(())
    }

    /// copilot.go:288-292 — a device code past created_at + expires_in is a
    /// 400 "device code expired" and evicts the session.
    #[tokio::test]
    async fn copilot_poll_expired_device_code_is_bad_request() -> TestResult {
        let (http, adapter) = adapter_with(Vec::new());
        // Seed an already-expired device state directly (the Go Clock seam).
        adapter.store.set(
            "copilot:oauth:expired-session".to_string(),
            PendingState::CopilotDevice {
                device_code: "dc-old".to_string(),
                expires_in: 10,
                created_at: Utc::now().timestamp() - 60,
            },
            DEVICE_FLOW_CACHE_EXPIRATION_SECS,
        )?;

        let result = adapter
            .poll_copilot_oauth(PollCopilotOAuthRequest {
                session_id: "expired-session".to_string(),
                proxy: None,
            })
            .await;
        match result {
            Err(OAuthServerError::BadRequest(message)) => {
                assert_eq!(message, "device code expired");
            }
            other => return Err(format!("expected expired, got {other:?}").into()),
        }
        assert!(seen_requests(&http).is_empty());
        Ok(())
    }

    // ---- pure helpers ------------------------------------------------------

    /// Callback parsing golden cases mirroring parseCodexCallbackURL /
    /// parseClaudeCodeCallbackURL error strings.
    #[test]
    fn parse_callback_url_golden_cases() {
        // Not a full URL.
        assert_eq!(
            parse_callback_url("localhost/callback?code=a&state=b", false),
            Err("callback_url must be a full URL".to_string())
        );
        // Missing code.
        assert_eq!(
            parse_callback_url("http://localhost/cb?state=b", false),
            Err("code parameter not found in callback_url".to_string())
        );
        // Missing state — codex wording vs claude wording.
        assert_eq!(
            parse_callback_url("http://localhost/cb?code=a", false),
            Err("state parameter not found in callback_url".to_string())
        );
        assert_eq!(
            parse_callback_url("http://localhost/cb?code=a", true),
            Err(
                "state parameter not found in callback_url (should be after # or in query)"
                    .to_string()
            )
        );
        // Query state (codex/antigravity).
        assert_eq!(
            parse_callback_url("http://localhost/cb?code=a&state=b", false),
            Ok(("a".to_string(), "b".to_string()))
        );
        // Fragment state wins for Claude; query state is the fallback.
        assert_eq!(
            parse_callback_url("http://localhost/cb?code=a&state=q#frag", true),
            Ok(("a".to_string(), "frag".to_string()))
        );
        assert_eq!(
            parse_callback_url("http://localhost/cb?code=a&state=q", true),
            Ok(("a".to_string(), "q".to_string()))
        );
        // Percent-decoded code (Go q.Get decodes).
        assert_eq!(
            parse_callback_url("http://localhost/cb?code=a%2Fb&state=s", false),
            Ok(("a/b".to_string(), "s".to_string()))
        );
    }

    /// encode_query mirrors Go url.Values.Encode (sorted keys + QueryEscape).
    #[test]
    fn encode_query_sorts_and_escapes_like_go() {
        assert_eq!(
            encode_query(&[("b", "2"), ("a", "1 x"), ("c", "read:user")]),
            "a=1+x&b=2&c=read%3Auser"
        );
    }

    /// The generated PKCE material is URL-safe and RFC 7636-sized.
    #[test]
    fn generated_pkce_material_shape() {
        let (state, verifier, challenge) = OAuthAdminAdapter::generate_pkce();
        assert_eq!(state.len(), 64);
        assert_eq!(verifier.len(), 128); // RFC 7636 maximum verifier length.
        assert!(verifier.bytes().all(|b| b.is_ascii_hexdigit()));
        // base64url(sha256) is always 43 chars, no padding.
        assert_eq!(challenge.len(), 43);
        assert_eq!(challenge, pkce_challenge(&verifier));
    }
}
