//! Claude Code OAuth outbound transformer (RUST-P7-003 S08).
//!
//! Port of Go `conduit/llm/transformer/anthropic/claudecode/`:
//!   * `constants.go`      — OAuth endpoints, client id, header values, models.
//!   * `token_provider.go` — Claude Code OAuth token provider defaults (JSON
//!     exchange strategy + production endpoints). The pure pieces of the shared
//!     Go `llm/oauth` package it builds on (`credentials.go`,
//!     `exchange_strategy.go` JSONStrategy, `token_provider.go` refresh
//!     decisions) are ported here as well, scoped to this module until a full
//!     shared oauth port lands.
//!   * `userid.go`         — Claude Code `user_id` parse/build/generate.
//!   * `utils.go`          — structured request mutations (system message
//!     injection, thinking disable, tool prefix, billing cch) + response strips.
//!   * `outbound.go`       — the outbound wrapper: structured phase
//!     (`prepare_claude_code_request`), HTTP decoration phase
//!     (`decorate_claude_code_http_request`), and the combined
//!     `build_claude_code_http_request` mirroring
//!     `ClaudeCodeTransformer.TransformRequest`.
//!
//! Style follows the crate's pure-decision-function convention (see
//! `anthropic.rs`): no I/O here — the real HTTP refresh/exchange calls are
//! executed by the async wiring layer using the [`HttpRequest`] values these
//! builders produce, and live tokens are injected via the minimal
//! [`TokenGetter`] trait (mirroring Go `oauth.TokenGetter`).

use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use conduit_core::ConduitError;
use conduit_llm::model::{ExtensionMap, HeaderMap};
use conduit_llm::{
    ApiFormat, ChatMessage, ChatRequest, HttpAuth, HttpRequest, LlmRequest, LlmRequestPayload,
    LlmResponse, MessageContent, StreamEvent,
};
use rand::RngCore;
use rand::rngs::OsRng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::TransformerResult;
use crate::anthropic::{
    PlatformRequestParams, PlatformType, build_anthropic_outbound_body,
    resolve_anthropic_platform_request,
};

// ---------------------------------------------------------------------------
// Constants — Go `claudecode/constants.go:1-39`.
// ---------------------------------------------------------------------------

/// Static list of Claude Code-capable model IDs. Mirrors Go `DefaultModels()`
/// (constants.go:4-14) verbatim.
pub const DEFAULT_MODELS: [&str; 7] = [
    "claude-haiku-4-5-20251001",
    "claude-sonnet-4-5-20250929",
    "claude-opus-4-5-20251101",
    "claude-opus-4-6",
    "claude-sonnet-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
];

/// Go `DefaultModels()` (constants.go:4) — returns a fresh owned list.
pub fn default_models() -> Vec<String> {
    DEFAULT_MODELS.iter().map(|m| (*m).to_string()).collect()
}

/// Go `AuthorizeURL` (constants.go:17).
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
/// Go `TokenURL` (constants.go:19).
pub const TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
/// Go `ClientID` (constants.go:20).
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Go `RedirectURI` (constants.go:22).
pub const REDIRECT_URI: &str = "http://localhost:54545/callback";
/// Go `Scopes` (constants.go:23).
pub const SCOPES: &str = "org:create_api_key user:profile user:inference";
/// Go `UserAgent` (constants.go:25) — keep consistent with Claude CLI.
pub const USER_AGENT: &str = "claude-cli/2.1.170 (external, cli)";

/// Go `ClaudeCodeBetaHeader` (constants.go:28) — beta feature identifiers for
/// the Claude Code API.
pub const CLAUDE_CODE_BETA_HEADER: &str = "claude-code-20250219,context-1m-2025-08-07,interleaved-thinking-2025-05-14,redact-thinking-2026-02-12,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,effort-2025-11-24";

/// Go `ClaudeCodeVersionHeader` (constants.go:31).
pub const CLAUDE_CODE_VERSION_HEADER: &str = "2023-06-01";
/// Go `ClaudeCodeBrowserAccessHeader` (constants.go:33).
pub const CLAUDE_CODE_BROWSER_ACCESS_HEADER: &str = "true";
/// Go `ClaudeCodeAppHeader` (constants.go:35).
pub const CLAUDE_CODE_APP_HEADER: &str = "cli";
/// Go `ClaudeCodeQuotaCheckModel` (constants.go:37).
pub const CLAUDE_CODE_QUOTA_CHECK_MODEL: &str = "claude-haiku-4-5";
/// Go `ClaudeCodeQuotaCheckHeader` (constants.go:38).
pub const CLAUDE_CODE_QUOTA_CHECK_HEADER: &str = "claude-code-20250219,interleaved-thinking-2025-05-14,redact-thinking-2026-02-12,context-management-2025-06-27,prompt-caching-scope-2026-01-05,mid-conversation-system-2026-04-07,effort-2025-11-24";

/// Go `claudeCodeSystemMessage` (outbound.go:22).
pub const CLAUDE_CODE_SYSTEM_MESSAGE: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";
/// Go `toolPrefix` (outbound.go:23).
pub const TOOL_PREFIX: &str = "proxy_";

/// The API format the Claude Code transformer speaks. Mirrors Go
/// `ClaudeCodeTransformer.APIFormat()` returning `llm.APIFormatAnthropicMessage`
/// (via the wrapped anthropic outbound, outbound_test.go:473-478).
pub const CLAUDE_CODE_API_FORMAT: ApiFormat = ApiFormat::AnthropicMessages;

// ---------------------------------------------------------------------------
// OAuth credentials — Go `llm/oauth/credentials.go:10-86`.
// ---------------------------------------------------------------------------

/// Go `time.Time{}` zero instant (`0001-01-01T00:00:00Z`). Go serializes the
/// zero `time.Time` as this RFC3339 string; `IsZero()` compares against it.
fn go_zero_time() -> DateTime<Utc> {
    chrono::NaiveDate::from_ymd_opt(1, 1, 1)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Panic-free `chrono::Duration` from seconds (out-of-range → zero).
fn duration_secs(secs: i64) -> chrono::Duration {
    chrono::Duration::try_seconds(secs).unwrap_or_else(chrono::Duration::zero)
}

/// Mirrors Go `oauth.OAuthCredentials` (credentials.go:10-18). JSON tags are
/// copied verbatim from the Go struct — snake_case, with `omitempty` mapped to
/// `skip_serializing_if` and non-`omitempty` string/time fields always
/// serialized (Go emits `""` / `"0001-01-01T00:00:00Z"` for their zero values).
/// Field order matches the Go struct so `to_json` output is byte-compatible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredentials {
    /// Go `ClientID string \`json:"client_id,omitempty"\``.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_id: String,
    /// Go `AccessToken string \`json:"access_token"\`` (no omitempty).
    #[serde(default)]
    pub access_token: String,
    /// Go `RefreshToken string \`json:"refresh_token"\`` (no omitempty).
    #[serde(default)]
    pub refresh_token: String,
    /// Go `IDToken string \`json:"id_token,omitempty"\``.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id_token: String,
    /// Go `ExpiresAt time.Time \`json:"expires_at"\`` (no omitempty — the Go
    /// zero time serializes as `0001-01-01T00:00:00Z`, mirrored by the default).
    #[serde(default = "go_zero_time")]
    pub expires_at: DateTime<Utc>,
    /// Go `TokenType string \`json:"token_type,omitempty"\``.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_type: String,
    /// Go `Scopes []string \`json:"scopes,omitempty"\``.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

impl Default for OAuthCredentials {
    /// Matches the Go zero value: empty strings/slice + zero `time.Time`.
    fn default() -> Self {
        Self {
            client_id: String::new(),
            access_token: String::new(),
            refresh_token: String::new(),
            id_token: String::new(),
            expires_at: go_zero_time(),
            token_type: String::new(),
            scopes: Vec::new(),
        }
    }
}

impl OAuthCredentials {
    /// Go `ExpiresAt.IsZero()` — true when `expires_at` is the Go zero instant.
    pub fn expires_at_is_zero(&self) -> bool {
        self.expires_at == go_zero_time()
    }

    /// Mirrors Go `(*OAuthCredentials).IsExpired(now)` (credentials.go:66-77).
    /// Zero expiry → expired; otherwise the token is considered expired **3
    /// minutes early** (`now.Add(3 * time.Minute).After(c.ExpiresAt)`, strict).
    /// The Go nil-receiver → `true` arm has no Rust equivalent (no nulls).
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        if self.expires_at_is_zero() {
            return true;
        }
        let skewed = now.checked_add_signed(duration_secs(3 * 60)).unwrap_or(now);
        skewed > self.expires_at
    }

    /// Mirrors Go `(*OAuthCredentials).ToJSON()` (credentials.go:79-86).
    pub fn to_json(&self) -> TransformerResult<String> {
        serde_json::to_string(self)
            .map_err(|err| ConduitError::internal(format!("marshal credentials: {err}")))
    }
}

/// Mirrors Go `oauth.ParseCredentialsJSON` (credentials.go:43-64):
/// * trims, rejects empty input (`"empty credentials"`),
/// * rejects a missing/empty `access_token` (`"access_token is empty"`),
/// * when a `refresh_token` exists but `expires_at` is missing/zero, assumes
///   a 1-hour expiry from `now` (Go uses `time.Now()`; passed explicitly here
///   to keep the function pure).
pub fn parse_credentials_json(
    raw: &str,
    now: DateTime<Utc>,
) -> TransformerResult<OAuthCredentials> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ConduitError::invalid_request("empty credentials"));
    }

    let mut creds: OAuthCredentials = serde_json::from_str(trimmed)
        .map_err(|err| ConduitError::invalid_request(err.to_string()))?;

    if creds.access_token.is_empty() {
        return Err(ConduitError::invalid_request("access_token is empty"));
    }

    // Go credentials.go:58-61 — assume 1 hour when refreshable but no expiry.
    if !creds.refresh_token.is_empty() && creds.expires_at_is_zero() {
        creds.expires_at = now.checked_add_signed(duration_secs(3600)).unwrap_or(now);
    }

    Ok(creds)
}

/// Mirrors Go `oauth.TokenResponse` (credentials.go:20-27) — the OAuth token
/// endpoint's response body. Go tags verbatim (`omitempty` on `id_token`,
/// `refresh_token`, `scope`; the rest always serialized).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenResponse {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id_token: String,
    #[serde(default)]
    pub access_token: String,
    /// Go `ExpiresIn int` — seconds until expiry (0 = absent).
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh_token: String,
    #[serde(default)]
    pub token_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scope: String,
}

impl TokenResponse {
    /// Mirrors Go `(*TokenResponse).ExpiresAt()` (credentials.go:30-36):
    /// `expires_in > 0` → `now + expires_in`, else the Go zero time.
    pub fn expires_at(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        if self.expires_in > 0 {
            now.checked_add_signed(duration_secs(self.expires_in))
                .unwrap_or(now)
        } else {
            go_zero_time()
        }
    }
}

/// Mirrors Go `oauth.TokenError` (credentials.go:38-41).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenError {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub error_description: String,
}

/// Mirrors Go `oauth.ParseTokenResponse` (exchange_strategy.go:163-195).
/// Decodes the token endpoint body into [`OAuthCredentials`]:
/// * JSON decode failure → `"decode response: <err>"`.
/// * missing `access_token` + parseable `{error, error_description}` →
///   `"token request failed: <error> - <error_description>"`.
/// * missing `access_token` otherwise → `"token response missing access_token"`.
/// * `scope` split on whitespace (Go `strings.Fields`) → `scopes`.
/// * `expires_in > 0` → `expires_at = now + expires_in` (Go uses `time.Now()`
///   inside `TokenResponse.ExpiresAt`; passed explicitly to stay pure).
pub fn parse_token_response(
    body: &[u8],
    client_id: &str,
    now: DateTime<Utc>,
) -> TransformerResult<OAuthCredentials> {
    let token_resp: TokenResponse = serde_json::from_slice(body)
        .map_err(|err| ConduitError::upstream(format!("decode response: {err}")))?;

    if token_resp.access_token.is_empty() {
        if let Ok(token_err) = serde_json::from_slice::<TokenError>(body)
            && !token_err.error.is_empty()
        {
            return Err(ConduitError::upstream(format!(
                "token request failed: {} - {}",
                token_err.error, token_err.error_description
            )));
        }

        return Err(ConduitError::upstream(
            "token response missing access_token",
        ));
    }

    let mut creds = OAuthCredentials {
        access_token: token_resp.access_token.clone(),
        refresh_token: token_resp.refresh_token.clone(),
        id_token: token_resp.id_token.clone(),
        client_id: client_id.to_string(),
        token_type: token_resp.token_type.clone(),
        ..OAuthCredentials::default()
    };

    if !token_resp.scope.is_empty() {
        creds.scopes = token_resp
            .scope
            .split_whitespace()
            .map(str::to_string)
            .collect();
    }

    if token_resp.expires_in > 0 {
        creds.expires_at = token_resp.expires_at(now);
    }

    Ok(creds)
}

/// Exchange-flavored wrapper over [`parse_token_response`]. Mirrors Go
/// `TokenProvider.Exchange`'s error rewrite (token_provider.go:131-139):
/// `"token request failed: X"` → `"token exchange failed: X"`.
pub fn parse_exchange_response(
    body: &[u8],
    client_id: &str,
    now: DateTime<Utc>,
) -> TransformerResult<OAuthCredentials> {
    parse_token_response(body, client_id, now).map_err(|mut err| {
        if err.message.contains("token request failed:") {
            let rest = err
                .message
                .strip_prefix("token request failed: ")
                .unwrap_or(&err.message)
                .to_string();
            err.message = format!("token exchange failed: {rest}");
        }
        err
    })
}

/// Refresh-flavored wrapper over [`parse_token_response`]. Mirrors the tail of
/// Go `TokenProvider.refresh` (token_provider.go:487-495): parses with the
/// current credentials' `client_id` and **preserves the old refresh token**
/// when the response does not return a new one.
pub fn parse_refresh_response(
    body: &[u8],
    current: &OAuthCredentials,
    now: DateTime<Utc>,
) -> TransformerResult<OAuthCredentials> {
    let mut refreshed = parse_token_response(body, &current.client_id, now)?;
    if refreshed.refresh_token.is_empty() {
        refreshed.refresh_token = current.refresh_token.clone();
    }
    Ok(refreshed)
}

// ---------------------------------------------------------------------------
// Token provider — Go `claudecode/token_provider.go` + the pure pieces of
// `llm/oauth/token_provider.go` / `exchange_strategy.go` (JSONStrategy).
// ---------------------------------------------------------------------------

/// Mirrors Go `oauth.OAuthUrls` (token_provider.go:30-33).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthUrls {
    pub authorize_url: String,
    pub token_url: String,
}

/// Mirrors Go `claudecode.DefaultTokenURLs` (token_provider.go:22-25) — the
/// production Claude OAuth endpoints.
pub fn default_token_urls() -> OAuthUrls {
    OAuthUrls {
        authorize_url: AUTHORIZE_URL.to_string(),
        token_url: TOKEN_URL.to_string(),
    }
}

/// Mirrors Go `oauth.ExchangeParams` (token_provider.go:66-72).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExchangeParams {
    pub code: String,
    pub code_verifier: String,
    pub client_id: String,
    pub redirect_uri: String,
    /// Optional — Claude requires `state` in the token exchange
    /// (exchange_strategy.go:99-102).
    pub state: String,
}

/// Shared headers of the Claude Code JSON token requests
/// (exchange_strategy.go:109-115 / 146-152). Claude Code always sends the
/// pinned CLI User-Agent: `claudecode.NewTokenProvider` forces
/// `ExchangeStrategy = &oauth.JSONStrategy{UserAgent: UserAgent}`
/// (claudecode/token_provider.go:11).
fn json_token_request_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    headers.insert("Accept".to_string(), "application/json".to_string());
    headers.insert("User-Agent".to_string(), USER_AGENT.to_string());
    headers
}

/// Build the authorization-code exchange request against the Claude Code
/// production token endpoint. Combines Go `TokenProvider.Exchange`'s parameter
/// validation (token_provider.go:105-119, with the Go error strings) with
/// `JSONStrategy.BuildExchangeRequest` (exchange_strategy.go:90-123). The
/// HTTP-client/token-URL nil guards live with the async wiring (the token URL
/// here is the non-empty `TOKEN_URL` constant).
///
/// Go marshals a `map[string]string` — key order is alphabetical. We build a
/// `BTreeMap<String, String>` and serialize it directly so the wire bytes are
/// alphabetical regardless of the `serde_json/preserve_order` feature (which,
/// if enabled transitively anywhere in the workspace dep tree, would otherwise
/// turn `serde_json::Value::Object` into an insertion-order `IndexMap`, breaking
/// Go byte-for-byte parity in the real binary).
pub fn build_exchange_request(params: &ExchangeParams) -> TransformerResult<HttpRequest> {
    if params.code.is_empty() {
        return Err(ConduitError::invalid_request("code is empty"));
    }
    if params.code_verifier.is_empty() {
        return Err(ConduitError::invalid_request("code_verifier is empty"));
    }
    if params.client_id.is_empty() {
        return Err(ConduitError::invalid_request("client_id is empty"));
    }
    if params.redirect_uri.is_empty() {
        return Err(ConduitError::invalid_request("redirect_uri is empty"));
    }

    let mut body: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    body.insert("grant_type".to_string(), "authorization_code".to_string());
    body.insert("code".to_string(), params.code.clone());
    body.insert("client_id".to_string(), params.client_id.clone());
    body.insert("redirect_uri".to_string(), params.redirect_uri.clone());
    body.insert("code_verifier".to_string(), params.code_verifier.clone());
    // Go exchange_strategy.go:99-102 — Claude requires state when present.
    if !params.state.is_empty() {
        body.insert("state".to_string(), params.state.clone());
    }

    let body_bytes = serde_json::to_vec(&body)
        .map_err(|err| ConduitError::internal(format!("marshal exchange request: {err}")))?;

    Ok(HttpRequest {
        method: "POST".to_string(),
        url: Some(TOKEN_URL.to_string()),
        headers: json_token_request_headers(),
        body: Some(body_bytes),
        ..HttpRequest::default()
    })
}

/// Build the refresh-token request against the Claude Code production token
/// endpoint. Mirrors `JSONStrategy.BuildRefreshRequest`
/// (exchange_strategy.go:126-160) — the `refresh_token is empty` guard is the
/// Go one (both the strategy and `TokenProvider.refresh` check it); `client_id`
/// is always included in the body, even when empty (Go map literal).
pub fn build_refresh_request(creds: &OAuthCredentials) -> TransformerResult<HttpRequest> {
    if creds.refresh_token.is_empty() {
        return Err(ConduitError::invalid_request("refresh_token is empty"));
    }

    // See build_exchange_request: BTreeMap keeps wire bytes alphabetical
    // (Go map marshal) independent of the serde_json/preserve_order feature.
    let mut body: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    body.insert("grant_type".to_string(), "refresh_token".to_string());
    body.insert("client_id".to_string(), creds.client_id.clone());
    body.insert("refresh_token".to_string(), creds.refresh_token.clone());

    let body_bytes = serde_json::to_vec(&body)
        .map_err(|err| ConduitError::internal(format!("marshal refresh request: {err}")))?;

    Ok(HttpRequest {
        method: "POST".to_string(),
        url: Some(TOKEN_URL.to_string()),
        headers: json_token_request_headers(),
        body: Some(body_bytes),
        ..HttpRequest::default()
    })
}

/// Refresh decision mirroring Go `TokenProvider.EnsureFresh`
/// (token_provider.go:208-231): no refresh token → never refresh (the current
/// credentials are returned as-is); `refresh_before <= 0` defaults to 5
/// minutes; refresh when the expiry is zero/unknown or falls within the
/// `refresh_before` window (`now.Add(refreshBefore).After(ExpiresAt)`, strict).
pub fn should_refresh(
    creds: &OAuthCredentials,
    now: DateTime<Utc>,
    refresh_before: chrono::Duration,
) -> bool {
    if creds.refresh_token.is_empty() {
        return false;
    }

    let refresh_before = if refresh_before <= chrono::Duration::zero() {
        duration_secs(5 * 60)
    } else {
        refresh_before
    };

    creds.expires_at_is_zero()
        || now.checked_add_signed(refresh_before).unwrap_or(now) > creds.expires_at
}

/// Mirrors Go `TokenProvider.nextAutoRefreshDelay` (token_provider.go:393-414):
/// the sleep until the next auto-refresh attempt. `fallback_interval <= 0`
/// defaults to 1 minute; credentials without a refresh token or expiry use the
/// fallback; otherwise sleep until `expires_at - refresh_before` (clamped ≥ 0).
pub fn next_auto_refresh_delay(
    creds: Option<&OAuthCredentials>,
    now: DateTime<Utc>,
    refresh_before: chrono::Duration,
    fallback_interval: chrono::Duration,
) -> chrono::Duration {
    let fallback = if fallback_interval <= chrono::Duration::zero() {
        duration_secs(60)
    } else {
        fallback_interval
    };

    let Some(creds) = creds else {
        return fallback;
    };
    if creds.refresh_token.is_empty() || creds.expires_at_is_zero() {
        return fallback;
    }

    let target = creds
        .expires_at
        .checked_sub_signed(refresh_before)
        .unwrap_or(creds.expires_at);
    let delay = target.signed_duration_since(now);
    if delay < chrono::Duration::zero() {
        chrono::Duration::zero()
    } else {
        delay
    }
}

/// Minimal token source, mirroring Go `oauth.TokenGetter`
/// (token_provider.go:35-37). The Go interface takes a `context.Context` and
/// may perform a network refresh; the async wiring layer implements that on
/// top of [`build_refresh_request`] / [`parse_refresh_response`] and hands the
/// transformer an implementation of this trait (or pre-resolves the token into
/// a [`StaticTokenProvider`]).
pub trait TokenGetter: Send + Sync {
    fn get(&self) -> TransformerResult<OAuthCredentials>;
}

/// Mirrors Go `oauth.StaticTokenProvider` (token_provider.go:416-427) — a
/// fixed set of credentials, returned without any expiry check.
#[derive(Debug, Clone)]
pub struct StaticTokenProvider {
    creds: OAuthCredentials,
}

impl StaticTokenProvider {
    /// Go `oauth.NewStaticTokenProvider`.
    pub fn new(creds: OAuthCredentials) -> Self {
        Self { creds }
    }
}

impl TokenGetter for StaticTokenProvider {
    fn get(&self) -> TransformerResult<OAuthCredentials> {
        Ok(self.creds.clone())
    }
}

// ---------------------------------------------------------------------------
// User IDs — Go `claudecode/userid.go:1-105`.
// ---------------------------------------------------------------------------

/// Mirrors Go `claudecode.UserID` (userid.go:18-22) — parsed Claude Code
/// `user_id` fields. Go JSON tags verbatim: `device_id`, `account_uuid`,
/// `session_id` (no omitempty — all fields always serialized, declaration
/// order preserved so `build_user_id` matches Go `json.Marshal` output).
///
/// Deserialization note: Go `json.Unmarshal` matches keys case-insensitively;
/// serde is case-sensitive. Real Claude Code payloads use the exact lowercase
/// tags, so this drift is not observable on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserId {
    pub device_id: String,
    pub account_uuid: String,
    pub session_id: String,
}

/// Go `legacyPattern` (userid.go:26-28) — the old Claude Code user_id format
/// `user_<64hex>_account__session_<uuid-v4>`.
fn legacy_pattern() -> Option<&'static Regex> {
    static PATTERN: OnceLock<Option<Regex>> = OnceLock::new();
    PATTERN
        .get_or_init(|| {
            Regex::new(
                r"^user_([a-fA-F0-9]{64})_account__session_([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$",
            )
            .ok()
        })
        .as_ref()
}

/// Mirrors Go `claudecode.ParseUserID` (userid.go:36-67). Supports both the
/// legacy string format and the v2 JSON format (Claude Code >= 2.1.78);
/// returns `None` when the input matches neither (including a v2 JSON body
/// with an empty `session_id`).
pub fn parse_user_id(raw: &str) -> Option<UserId> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    // v2 JSON format first (userid.go:43-54).
    if raw.starts_with('{') {
        let uid: UserId = serde_json::from_str(raw).ok()?;
        if uid.session_id.is_empty() {
            return None;
        }
        return Some(uid);
    }

    // Legacy format (userid.go:57-66).
    let captures = legacy_pattern()?.captures(raw)?;
    Some(UserId {
        device_id: captures.get(1)?.as_str().to_string(),
        account_uuid: String::new(),
        session_id: captures.get(2)?.as_str().to_string(),
    })
}

/// Mirrors Go `claudecode.BuildUserID` (userid.go:70-73) — serializes the v2
/// JSON format. Go ignores the marshal error (`data, _ := json.Marshal`);
/// serialization of this plain struct cannot fail, so the fallback is `""`.
pub fn build_user_id(uid: &UserId) -> String {
    serde_json::to_string(uid).unwrap_or_default()
}

/// Lowercase hex encoding (Go `encoding/hex.EncodeToString`).
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0F) as usize] as char);
    }
    out
}

/// Mirrors Go `claudecode.GenerateUserID` (userid.go:79-105) — creates a
/// user_id in v2 JSON format.
///
/// * `account_identity` non-empty → deterministic identity:
///   `device_id = hex(sha256(identity))`,
///   `account_uuid = uuid.NewSHA1(uuid.NameSpaceURL, identity)` (a v5 UUID).
/// * `account_identity` empty → 32 random bytes for the device id (the Go
///   `rand.Read` error is ignored, mirrored by `try_fill_bytes`), empty
///   account UUID.
/// * `session_id` — Go reads `shared.GetSessionID(ctx)`; passed explicitly
///   here. Absent/blank → a fresh v4 UUID.
pub fn generate_user_id(session_id: Option<&str>, account_identity: &str) -> String {
    let (device_id, account_uuid) = if !account_identity.is_empty() {
        let digest = Sha256::digest(account_identity.as_bytes());
        (
            hex_lower(&digest),
            Uuid::new_v5(&Uuid::NAMESPACE_URL, account_identity.as_bytes()).to_string(),
        )
    } else {
        let mut bytes = [0u8; 32];
        // Go userid.go:91 ignores the rand.Read error; same here.
        let _ = OsRng.try_fill_bytes(&mut bytes);
        (hex_lower(&bytes), String::new())
    };

    let session_id = match session_id {
        Some(session) if !session.trim().is_empty() => session.to_string(),
        _ => Uuid::new_v4().to_string(),
    };

    build_user_id(&UserId {
        device_id,
        account_uuid,
        session_id,
    })
}

// ---------------------------------------------------------------------------
// Structured request mutations + response strips — Go `claudecode/utils.go`.
// ---------------------------------------------------------------------------

/// Go `claudeCodeBillingCCHMetadataKey` (utils.go:14). Written by the Go
/// billing-header pipeline middleware (`llm/pipeline/cc/billing_header.go:13`,
/// Rust: `conduit_orchestrator::pre_execution::BILLING_CCH_KEY`).
pub const BILLING_CCH_METADATA_KEY: &str = "claudecode_billing_cch";

/// Go `billingHeaderPrefix` (utils.go:149).
pub const BILLING_HEADER_PREFIX: &str = "x-anthropic-billing-header:";

/// Extract the stripped billing `cch` value from a transformer-metadata map.
/// Mirrors the lookup inside Go `ensureBillingSystemMessageCCH`
/// (utils.go:181-189): the value must be a string whose trimmed form is
/// non-empty. Go stores it in `llm.Request.TransformerMetadata`; the Rust
/// unified request has no typed slot, so callers pass whichever
/// `ExtensionMap` carries it (e.g. `HttpRequest::transformer_metadata`).
pub fn billing_cch_from_transformer_metadata(metadata: &ExtensionMap) -> Option<String> {
    let value = metadata.get(BILLING_CCH_METADATA_KEY)?.as_str()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// Mirrors Go `mergeBetasIntoHeader` (utils.go:119-146): merge beta features
/// into an `Anthropic-Beta` header value. Existing entries are kept in order
/// (duplicates **within the base are preserved**, matching Go); extras are
/// appended only when not already present.
pub fn merge_betas_into_header(base_betas: &str, extra_betas: &[&str]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut existing: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    let base = base_betas.trim();
    if !base.is_empty() {
        for beta in base.split(',') {
            let beta = beta.trim();
            if !beta.is_empty() {
                parts.push(beta.to_string());
                existing.insert(beta.to_string());
            }
        }
    }

    for beta in extra_betas {
        let beta = beta.trim();
        if !beta.is_empty() && !existing.contains(beta) {
            parts.push(beta.to_string());
            existing.insert(beta.to_string());
        }
    }

    parts.join(",")
}

/// Mirrors Go `removeBillingSystemMessages` (utils.go:155-174): drop system
/// messages whose (trimmed) plain-text content starts with the
/// `x-anthropic-billing-header:` pattern (case-sensitive `HasPrefix`, exactly
/// like Go). Used for non-official channels to avoid leaking client info.
pub fn remove_billing_system_messages(chat: &mut ChatRequest) {
    if chat.messages.is_empty() {
        return;
    }
    chat.messages.retain(|message| {
        !(message.role == "system"
            && matches!(
                &message.content,
                Some(MessageContent::Text(text)) if text.trim().starts_with(BILLING_HEADER_PREFIX)
            ))
    });
}

/// Mirrors Go `ensureBillingHeaderCCHInText` (utils.go:226-262). Returns the
/// (possibly rewritten) text plus a changed flag. The prefix match is
/// case-insensitive; an existing `cch=` entry short-circuits; otherwise
/// ` cch=<value>;` is appended to the **trimmed** text (Go returns the trimmed
/// form when it rewrites).
fn ensure_billing_header_cch_in_text(text: &str, cch: &str) -> (String, bool) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (text.to_string(), false);
    }

    let lower = trimmed.to_lowercase();
    if !lower.starts_with(BILLING_HEADER_PREFIX) {
        return (text.to_string(), false);
    }

    // The prefix is pure ASCII, so byte slicing the (case-insensitively
    // matched) trimmed text is safe — mirrors Go `trimmed[len(prefix):]`.
    let rest = trimmed[BILLING_HEADER_PREFIX.len()..].trim();
    if rest.is_empty() {
        return (text.to_string(), false);
    }

    for part in rest.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part.to_lowercase().starts_with("cch=") {
            return (text.to_string(), false);
        }
    }

    let mut out = trimmed.to_string();
    if !out.ends_with(';') {
        out.push(';');
    }
    out.push_str(" cch=");
    out.push_str(cch.trim());
    out.push(';');

    (out, true)
}

/// Mirrors Go `ensureBillingSystemMessageCCH` (utils.go:176-224): re-append the
/// stripped billing `cch` entry to every system message carrying the billing
/// header pattern. Go resolves `cch` from `TransformerMetadata` internally;
/// here the caller passes the extracted value (see
/// [`billing_cch_from_transformer_metadata`]). Empty/blank `cch` is a no-op.
pub fn ensure_billing_system_message_cch(chat: &mut ChatRequest, cch: &str) {
    if chat.messages.is_empty() {
        return;
    }
    let cch = cch.trim();
    if cch.is_empty() {
        return;
    }

    for message in &mut chat.messages {
        if message.role != "system" {
            continue;
        }

        match &mut message.content {
            // Go utils.go:201-206 — single string content.
            Some(MessageContent::Text(text)) => {
                let (updated, changed) = ensure_billing_header_cch_in_text(text, cch);
                if changed {
                    *text = updated;
                }
            }
            // Go utils.go:208-220 — multi-part content, text parts only.
            Some(MessageContent::Parts(parts)) => {
                for part in parts {
                    if part.part_type != "text" {
                        continue;
                    }
                    let Some(text) = part.text.as_mut() else {
                        continue;
                    };
                    let (updated, changed) = ensure_billing_header_cch_in_text(text, cch);
                    if changed {
                        *text = updated;
                    }
                }
            }
            _ => {}
        }
    }
}

/// Mirrors Go `injectFakeUserIDStructured` (utils.go:17-28): ensure
/// `metadata["user_id"]` carries a Claude Code-shaped user id — keep an
/// existing parseable value, otherwise generate one. Operates on the unified
/// request's metadata map (Go `llm.Request.Metadata map[string]string`; values
/// here are `Value::String`).
pub fn inject_fake_user_id(
    metadata: &mut ExtensionMap,
    session_id: Option<&str>,
    account_identity: &str,
) {
    let existing = metadata
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if existing.is_empty() || parse_user_id(existing).is_none() {
        metadata.insert(
            "user_id".to_string(),
            Value::String(generate_user_id(session_id, account_identity)),
        );
    }
}

/// Mirrors Go `disableThinkingIfToolChoiceForcedStructured` (utils.go:34-60):
/// the Anthropic API rejects thinking when `tool_choice` forces tool use
/// (`"any"` or a named `{type:"tool"}` choice), so clear the reasoning fields.
/// Go clears `ReasoningEffort` + `ReasoningBudget`; the unified Rust
/// `ChatRequest` types `reasoning_effort` and carries `reasoning_budget` in
/// `extra`.
pub fn disable_thinking_if_tool_choice_forced(chat: &mut ChatRequest) {
    let Some(tool_choice) = chat.tool_choice.as_ref() else {
        return;
    };

    // Go utils.go:41-49 — string form "any", or named form with type "tool".
    let forces_tool_use = match tool_choice {
        Value::String(choice) => choice == "any",
        Value::Object(named) => named.get("type").and_then(Value::as_str) == Some("tool"),
        _ => false,
    };

    let effort_set = chat
        .reasoning_effort
        .as_deref()
        .is_some_and(|effort| !effort.is_empty());

    if forces_tool_use && effort_set {
        chat.reasoning_effort = None;
        chat.extra.remove("reasoning_budget");
    }
}

/// Mirrors Go `applyClaudeToolPrefixStructured` (utils.go:63-86): prefix every
/// tool name in the request, plus the named `tool_choice` function when its
/// type is `"tool"`.
///
/// Go mutates `Tools[i].Function.Name` for every tool (the Go `Function` is a
/// value struct, so an unset name is `""`). The unified `UnifiedTool.name` is
/// `Option<String>`; `None` has no Go-representable counterpart and is left
/// untouched, while `Some` values (including empty strings) follow the Go
/// `HasPrefix`/concat rule exactly.
pub fn apply_claude_tool_prefix(chat: &mut ChatRequest, prefix: &str) {
    if prefix.is_empty() {
        return;
    }

    for tool in &mut chat.tools {
        if let Some(name) = tool.name.as_mut()
            && !name.starts_with(prefix)
        {
            *name = format!("{prefix}{name}");
        }
    }

    // Go utils.go:76-83 — prefix tool_choice.function.name when type == "tool".
    if let Some(Value::Object(choice)) = chat.tool_choice.as_mut()
        && choice.get("type").and_then(Value::as_str) == Some("tool")
        && let Some(Value::Object(function)) = choice.get_mut("function")
    {
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !name.is_empty() && !name.starts_with(prefix) {
            function.insert("name".to_string(), Value::String(format!("{prefix}{name}")));
        }
    }
}

/// Mirrors Go `stripClaudeToolPrefixFromResponse` (utils.go:89-116): remove the
/// prefix from `content[i].name` of every `tool_use` block in a non-streaming
/// Anthropic response body. Unparseable bodies and bodies without a `content`
/// array are returned unchanged.
///
/// Divergence note: Go rewrites only the matched paths in place (sjson),
/// preserving the original byte layout; this port re-serializes the parsed
/// JSON (compact, alphabetical keys) **only when a name actually changed** —
/// unchanged bodies are returned byte-identical.
pub fn strip_claude_tool_prefix_from_response(body: &[u8], prefix: &str) -> Vec<u8> {
    if prefix.is_empty() {
        return body.to_vec();
    }
    let Ok(mut root) = serde_json::from_slice::<Value>(body) else {
        return body.to_vec();
    };
    let Some(content) = root.get_mut("content").and_then(Value::as_array_mut) else {
        return body.to_vec();
    };

    let mut changed = false;
    for part in content.iter_mut() {
        if part.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(name) = part.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(stripped) = name.strip_prefix(prefix) else {
            continue;
        };
        let stripped = stripped.to_string();
        if let Some(obj) = part.as_object_mut() {
            obj.insert("name".to_string(), Value::String(stripped));
            changed = true;
        }
    }

    if !changed {
        return body.to_vec();
    }
    serde_json::to_vec(&root).unwrap_or_else(|_| body.to_vec())
}

/// Mirrors Go `injectClaudeCodeSystemMessageStructured` (utils.go:265-291):
/// prepend the Claude Code system message (with a forced `ephemeral`
/// cache_control) unless the first message is already exactly it — the skip
/// branch returns early, leaving the array-instructions flag untouched, just
/// like Go.
///
/// Go then sets `TransformOptions.ArrayInstructions = &true` when unset so the
/// serialized `system` prompt uses array form (required for cache_control).
/// The unified Rust request has no typed `TransformOptions`; the flag rides in
/// `extra["transform_options"]["array_instructions"]` using the Go JSON field
/// names (`llm/options.go:3-9`).
pub fn inject_claude_code_system_message(llm_req: &mut LlmRequest) {
    let LlmRequestPayload::Chat(chat) = &mut llm_req.payload else {
        return;
    };

    // Go utils.go:275-280 — already injected → early return.
    if let Some(first) = chat.messages.first()
        && first.role == "system"
        && matches!(
            &first.content,
            Some(MessageContent::Text(text)) if text == CLAUDE_CODE_SYSTEM_MESSAGE
        )
    {
        return;
    }

    // Go utils.go:266-273, 282 — prepend with cache_control {type: ephemeral}.
    // `CacheControl` is not a typed field on the unified `ChatMessage`; it
    // rides in the `extra` flatten (crate convention, see conduit-llm model.rs).
    let mut extra = ExtensionMap::new();
    extra.insert("cache_control".to_string(), json!({ "type": "ephemeral" }));
    chat.messages.insert(
        0,
        ChatMessage {
            role: "system".to_string(),
            name: None,
            content: Some(MessageContent::Text(CLAUDE_CODE_SYSTEM_MESSAGE.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra,
        },
    );

    // Go utils.go:284-288 — only set when unset.
    let options = llm_req
        .extra
        .entry("transform_options".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(map) = options.as_object_mut()
        && !map.contains_key("array_instructions")
    {
        map.insert("array_instructions".to_string(), Value::Bool(true));
    }
}

// ---------------------------------------------------------------------------
// Outbound wrapper — Go `claudecode/outbound.go`.
// ---------------------------------------------------------------------------

/// Go `claudeCodeHeaders` (outbound.go:28-42) — every header set on Claude
/// Code requests, as `[name, value]` pairs (order preserved).
pub const CLAUDE_CODE_HEADERS: [(&str, &str); 13] = [
    ("Anthropic-Beta", CLAUDE_CODE_BETA_HEADER),
    ("Anthropic-Version", CLAUDE_CODE_VERSION_HEADER),
    (
        "Anthropic-Dangerous-Direct-Browser-Access",
        CLAUDE_CODE_BROWSER_ACCESS_HEADER,
    ),
    ("X-App", CLAUDE_CODE_APP_HEADER),
    ("X-Stainless-Helper-Method", "stream"),
    ("X-Stainless-Retry-Count", "0"),
    ("X-Stainless-Runtime-Version", "v24.3.0"),
    ("X-Stainless-Package-Version", "0.94.0"),
    ("X-Stainless-Runtime", "node"),
    ("X-Stainless-Lang", "js"),
    ("X-Stainless-Arch", "arm64"),
    ("X-Stainless-Os", "MacOS"),
    ("X-Stainless-Timeout", "600"),
];

/// Go `PassthroughHeaders` (outbound.go:44-49) — inbound headers forwarded to
/// the upstream Anthropic API (inbound values override the defaults). Derived
/// in Go as the `claudeCodeHeaders` names plus `X-Claude-Code-Session-Id`.
pub const PASSTHROUGH_HEADERS: [&str; 14] = [
    "Anthropic-Beta",
    "Anthropic-Version",
    "Anthropic-Dangerous-Direct-Browser-Access",
    "X-App",
    "X-Stainless-Helper-Method",
    "X-Stainless-Retry-Count",
    "X-Stainless-Runtime-Version",
    "X-Stainless-Package-Version",
    "X-Stainless-Runtime",
    "X-Stainless-Lang",
    "X-Stainless-Arch",
    "X-Stainless-Os",
    "X-Stainless-Timeout",
    "X-Claude-Code-Session-Id",
];

/// Go `httpReq.Metadata["strip_tool_prefix"]` key (outbound.go:160-167).
pub const STRIP_TOOL_PREFIX_METADATA_KEY: &str = "strip_tool_prefix";

/// Default Anthropic base URL (Go outbound.go:65-68).
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Mirrors Go `isClaudeCLIUserAgent` (outbound.go:358-360).
pub fn is_claude_cli_user_agent(value: &str) -> bool {
    value.starts_with("claude-cli/")
}

/// Mirrors Go `claudecode.Params` (outbound.go:52-57) minus the
/// `TokenProvider` handle, which is passed separately as a
/// [`TokenGetter`] trait object (Go requires it non-nil at construction;
/// the Rust builder takes it as a required argument instead).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeCodeParams {
    /// Base URL for the Anthropic API (optional; Go outbound.go:65-68 defaults
    /// to `https://api.anthropic.com/v1` when empty).
    pub base_url: String,
    /// Whether the channel uses official OAuth credentials.
    pub is_official: bool,
    /// Stable channel identity for deterministic user_id (optional).
    pub account_identity: String,
}

impl ClaudeCodeParams {
    /// Go outbound.go:65-68 — empty base URL falls back to the default.
    pub fn effective_base_url(&self) -> &str {
        if self.base_url.is_empty() {
            DEFAULT_BASE_URL
        } else {
            &self.base_url
        }
    }
}

/// Result of the structured phase — the facts the HTTP decoration phase (and
/// the response path) need from Go outbound.go:106-167: whether the client is
/// the Claude CLI (keep its User-Agent), the raw inbound User-Agent, and
/// whether the `proxy_` tool prefix was applied (→ strip it from responses).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreparedClaudeCode {
    pub keep_client_ua: bool,
    pub raw_user_agent: Option<String>,
    pub strip_tool_prefix: bool,
}

/// Case-insensitive header get, mirroring Go `http.Header.Get` over the
/// crate's plain `BTreeMap<String, String>` header map.
fn get_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Case-insensitive header set, mirroring Go `http.Header.Set` (replaces any
/// case-variant of the key, stores the canonical name passed in).
fn set_header(headers: &mut HeaderMap, name: &str, value: String) {
    headers.retain(|key, _| !key.eq_ignore_ascii_case(name));
    headers.insert(name.to_string(), value);
}

/// Structured phase of Go `ClaudeCodeTransformer.TransformRequest`
/// (outbound.go:106-143): sniff the inbound User-Agent, then mutate the
/// unified request **before** the base Anthropic transformer serializes it —
/// thinking disable, system-message injection, billing cch restore (official
/// only), fake user_id, and the `proxy_` tool prefix (official + non-CLI
/// clients only).
///
/// * `inbound_headers` — Go `llmReq.RawRequest.Headers`.
/// * `session_id` — Go `shared.GetSessionID(ctx)` (transformer/shared/session.go).
/// * `billing_cch` — Go `llmReq.TransformerMetadata["claudecode_billing_cch"]`
///   (see [`billing_cch_from_transformer_metadata`]).
///
/// Go clones the request shallowly (`reqCopy := *llmReq`) but still mutates
/// the shared `Tools` slice and `Metadata` map through the copy; mutating in
/// place here matches the observable behavior.
pub fn prepare_claude_code_request(
    llm_req: &mut LlmRequest,
    inbound_headers: Option<&HeaderMap>,
    params: &ClaudeCodeParams,
    session_id: Option<&str>,
    billing_cch: Option<&str>,
) -> PreparedClaudeCode {
    // Go outbound.go:106-119 — keep the client UA only for Claude CLI clients.
    let raw_user_agent = inbound_headers
        .and_then(|headers| get_header(headers, "User-Agent"))
        .unwrap_or("")
        .to_string();
    let keep_client_ua = is_claude_cli_user_agent(&raw_user_agent);

    // Go outbound.go:133.
    if let LlmRequestPayload::Chat(chat) = &mut llm_req.payload {
        disable_thinking_if_tool_choice_forced(chat);
    }

    // Go outbound.go:135.
    inject_claude_code_system_message(llm_req);

    // Go outbound.go:136-138 — official channels restore the billing cch.
    if params.is_official
        && let Some(cch) = billing_cch
        && let LlmRequestPayload::Chat(chat) = &mut llm_req.payload
    {
        ensure_billing_system_message_cch(chat, cch);
    }

    // Go outbound.go:140.
    inject_fake_user_id(&mut llm_req.metadata, session_id, &params.account_identity);

    // Go outbound.go:141-143 + 165-167.
    let strip_tool_prefix = params.is_official && !keep_client_ua;
    if strip_tool_prefix && let LlmRequestPayload::Chat(chat) = &mut llm_req.payload {
        apply_claude_tool_prefix(chat, TOOL_PREFIX);
    }

    PreparedClaudeCode {
        keep_client_ua,
        raw_user_agent: if raw_user_agent.is_empty() {
            None
        } else {
            Some(raw_user_agent)
        },
        strip_tool_prefix,
    }
}

/// HTTP decoration phase of Go `ClaudeCodeTransformer.TransformRequest`
/// (outbound.go:150-209): applied to the [`HttpRequest`] produced by the base
/// Anthropic outbound transformer.
pub fn decorate_claude_code_http_request(
    http_req: &mut HttpRequest,
    access_token: &str,
    stream: bool,
    inbound_headers: Option<&HeaderMap>,
    prepared: &PreparedClaudeCode,
) {
    // Go outbound.go:151-158 — add `beta=true` when the query lacks it.
    let beta_missing = http_req
        .query
        .get("beta")
        .and_then(|values| values.first())
        .map(String::as_str)
        .unwrap_or("")
        .is_empty();
    if beta_missing {
        http_req
            .query
            .insert("beta".to_string(), vec!["true".to_string()]);
    }

    // Go outbound.go:160-167 — mark the response path to strip the prefix.
    if prepared.strip_tool_prefix {
        http_req.metadata.insert(
            STRIP_TOOL_PREFIX_METADATA_KEY.to_string(),
            Value::String("true".to_string()),
        );
    }

    // Go outbound.go:169-172 — add/overwrite the Claude Code headers.
    for (name, value) in CLAUDE_CODE_HEADERS {
        set_header(&mut http_req.headers, name, value.to_string());
    }

    // Go outbound.go:174-186 — passthrough inbound headers, overriding the
    // defaults. Anthropic-Beta is merged (not replaced) to keep required betas.
    if let Some(raw_headers) = inbound_headers {
        for name in PASSTHROUGH_HEADERS {
            let Some(value) = get_header(raw_headers, name).filter(|value| !value.is_empty())
            else {
                continue;
            };
            if name == "Anthropic-Beta" {
                let base = get_header(&http_req.headers, name)
                    .unwrap_or("")
                    .to_string();
                let merged = merge_betas_into_header(&base, &[value]);
                set_header(&mut http_req.headers, name, merged);
            } else {
                let value = value.to_string();
                set_header(&mut http_req.headers, name, value);
            }
        }
    }

    // Go outbound.go:188-193 — Accept header per streaming mode.
    let accept = if stream {
        "text/event-stream"
    } else {
        "application/json"
    };
    set_header(&mut http_req.headers, "Accept", accept.to_string());

    // Go outbound.go:195-199 — client UA for Claude CLI, pinned UA otherwise.
    match prepared.raw_user_agent.as_deref() {
        Some(raw_ua) if prepared.keep_client_ua && !raw_ua.is_empty() => {
            set_header(&mut http_req.headers, "User-Agent", raw_ua.to_string());
        }
        _ => {
            set_header(&mut http_req.headers, "User-Agent", USER_AGENT.to_string());
        }
    }

    // Go outbound.go:201-207 — Claude Code OAuth always uses Bearer auth
    // (`httpclient.AuthConfig{Type: AuthTypeBearer, APIKey: apiKey}`).
    http_req.auth = Some(HttpAuth {
        scheme: "bearer".to_string(),
        token: Some(access_token.to_string()),
        ..HttpAuth::default()
    });
}

/// Full request build mirroring Go `ClaudeCodeTransformer.TransformRequest`
/// (outbound.go:98-210): resolve the OAuth token, run the structured phase,
/// serialize via the base Anthropic outbound
/// ([`build_anthropic_outbound_body`] + [`resolve_anthropic_platform_request`]
/// with `PlatformType::ClaudeCode`, mirroring Go's wrapped
/// `anthropic.NewOutboundTransformerWithConfig(&Config{Type:
/// PlatformClaudeCode, BaseURL})`, outbound.go:70-77), then apply the Claude
/// Code decorations.
pub fn build_claude_code_http_request(
    llm_req: &mut LlmRequest,
    inbound_headers: Option<&HeaderMap>,
    params: &ClaudeCodeParams,
    tokens: &dyn TokenGetter,
    session_id: Option<&str>,
    billing_cch: Option<&str>,
) -> TransformerResult<HttpRequest> {
    // Go outbound.go:124-130 — token first (before the mutations).
    let creds = tokens.get().map_err(|mut err| {
        err.message = format!("failed to get oauth token: {}", err.message);
        err
    })?;
    let access_token = creds.access_token;

    // Go outbound.go:106-143 — structured phase.
    let prepared =
        prepare_claude_code_request(llm_req, inbound_headers, params, session_id, billing_cch);

    // Go outbound.go:145-148 — base transformer.
    let mut body = build_anthropic_outbound_body(llm_req)?;

    // Go anthropic/outbound_convert.go:123-124 — `metadata.user_id` is emitted
    // by the Go base transformer (`req.Metadata = &AnthropicMetadata{UserID}`).
    // The Rust base body builder (S05 minimal) does not model `metadata` yet,
    // so the Claude Code wire contract is kept here.
    if let Some(user_id) = llm_req.metadata.get("user_id").and_then(Value::as_str)
        && !user_id.is_empty()
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("metadata".to_string(), json!({ "user_id": user_id }));
    }

    let model = llm_req.model.clone().unwrap_or_default();
    let platform_params = PlatformRequestParams {
        platform: PlatformType::ClaudeCode,
        base_url: params.effective_base_url(),
        endpoint_path: None,
        model: &model,
        stream: llm_req.stream,
        project_id: None,
        region: None,
        has_native_web_search: false,
    };
    let resolved = resolve_anthropic_platform_request(&platform_params, body)?;

    let mut http_req = HttpRequest {
        method: "POST".to_string(),
        url: Some(resolved.url),
        // ClaudeCode uses the default (non-Bedrock/Vertex) `/messages` path
        // (Go buildFullRequestURL, outbound.go:291-298).
        path: "/messages".to_string(),
        headers: resolved.headers,
        json_body: Some(resolved.body),
        ..HttpRequest::default()
    };

    // Go outbound.go:150-207.
    decorate_claude_code_http_request(
        &mut http_req,
        &access_token,
        llm_req.stream,
        inbound_headers,
        &prepared,
    );

    Ok(http_req)
}

/// Response-path guard mirroring Go `ClaudeCodeTransformer.TransformResponse`
/// (outbound.go:217-221): strip only when the request recorded that the
/// prefix was applied.
pub fn should_strip_tool_prefix(http_req: &HttpRequest) -> bool {
    http_req
        .metadata
        .get(STRIP_TOOL_PREFIX_METADATA_KEY)
        .and_then(Value::as_str)
        == Some("true")
}

/// Mirrors Go `stripClaudeToolPrefixFromStreamLine` (outbound.go:331-356):
/// remove the prefix from `content_block.name` of a `tool_use`
/// `content_block_start` event payload. Any non-matching/unparseable line is
/// returned unchanged. Go re-marshals the whole `map[string]any` (compact,
/// alphabetical keys) — serde_json's default `Map` matches that byte layout.
pub fn strip_claude_tool_prefix_from_stream_line(line: &[u8], prefix: &str) -> Vec<u8> {
    if prefix.is_empty() {
        return line.to_vec();
    }
    let Ok(mut data) = serde_json::from_slice::<Value>(line) else {
        return line.to_vec();
    };
    let Some(content_block) = data.get_mut("content_block").and_then(Value::as_object_mut) else {
        return line.to_vec();
    };
    if content_block.get("type").and_then(Value::as_str) != Some("tool_use") {
        return line.to_vec();
    }
    let Some(name) = content_block.get("name").and_then(Value::as_str) else {
        return line.to_vec();
    };
    let Some(stripped) = name.strip_prefix(prefix) else {
        return line.to_vec();
    };
    let stripped = stripped.to_string();
    content_block.insert("name".to_string(), Value::String(stripped));
    serde_json::to_vec(&data).unwrap_or_else(|_| line.to_vec())
}

/// Mirrors the body of Go `toolPrefixStripperStream.Next` (outbound.go:267-294):
/// strip the prefix from every streaming delta tool-call name in a unified
/// response chunk. Blind stripping is safe for the same reasons Go documents
/// (outbound.go:249-253): the prefix is only ever added for OAuth tokens from
/// non-CLI clients, and CLI clients never send `proxy_`-prefixed tools.
pub fn strip_tool_prefix_from_stream_response(response: &mut LlmResponse, prefix: &str) {
    for choice in &mut response.choices {
        let Some(delta) = choice.delta.as_mut() else {
            continue;
        };
        for tool_call in &mut delta.tool_calls {
            let Some(function) = tool_call.function.as_object_mut() else {
                continue;
            };
            let Some(name) = function.get("name").and_then(Value::as_str) else {
                continue;
            };
            // Go: `toolCall.Function.Name != "" && strings.HasPrefix(...)`.
            if name.is_empty() {
                continue;
            }
            if let Some(stripped) = name.strip_prefix(prefix) {
                let stripped = stripped.to_string();
                function.insert("name".to_string(), Value::String(stripped));
            }
        }
    }
}

/// Mirrors the chunk pre-pass of Go
/// `ClaudeCodeTransformer.AggregateStreamChunks` (outbound.go:309-327): strip
/// the prefix from every stream event whose raw data mentions a `tool_use`
/// block, before the base aggregator folds the chunks.
pub fn strip_tool_prefix_from_stream_chunks(chunks: &mut [StreamEvent], prefix: &str) {
    for chunk in chunks.iter_mut() {
        let Some(data) = chunk.data.as_ref() else {
            continue;
        };
        if data.is_empty() || !data.contains(r#""type":"tool_use""#) {
            continue;
        }
        let stripped = strip_claude_tool_prefix_from_stream_line(data.as_bytes(), prefix);
        if let Ok(text) = String::from_utf8(stripped) {
            chunk.data = Some(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Constants pins — Go constants.go golden values.
    #[test]
    fn constants_match_go_constants_go() {
        assert_eq!(default_models().len(), 7);
        assert_eq!(DEFAULT_MODELS[0], "claude-haiku-4-5-20251001");
        assert_eq!(DEFAULT_MODELS[6], "claude-opus-4-8");
        assert_eq!(TOKEN_URL, "https://api.anthropic.com/v1/oauth/token");
        assert_eq!(CLIENT_ID, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
        assert_eq!(REDIRECT_URI, "http://localhost:54545/callback");
        assert_eq!(SCOPES, "org:create_api_key user:profile user:inference");
        assert_eq!(USER_AGENT, "claude-cli/2.1.170 (external, cli)");
        assert!(CLAUDE_CODE_BETA_HEADER.contains("claude-code-20250219"));
        assert!(CLAUDE_CODE_BETA_HEADER.contains("context-1m-2025-08-07"));
        // The quota-check header is the beta header minus the 1M-context beta.
        assert!(!CLAUDE_CODE_QUOTA_CHECK_HEADER.contains("context-1m-2025-08-07"));
        assert_eq!(CLAUDE_CODE_QUOTA_CHECK_MODEL, "claude-haiku-4-5");
        assert_eq!(CLAUDE_CODE_API_FORMAT, ApiFormat::AnthropicMessages);
    }

    use conduit_llm::{Choice, LlmMessage, RequestType, ToolCall, UnifiedTool};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn text_message(role: &str, text: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            name: None,
            content: Some(MessageContent::Text(text.to_string())),
            tool_calls: Vec::new(),
            tool_call_id: None,
            extra: ExtensionMap::new(),
        }
    }

    /// The Go outbound tests' base request (outbound_test.go:31-35 etc.):
    /// model claude-sonnet-4-5, one user "Hello" message, max_tokens 1024.
    fn base_llm_request() -> LlmRequest {
        LlmRequest {
            request_type: RequestType::Chat,
            api_format: ApiFormat::AnthropicMessages,
            model: Some("claude-sonnet-4-5".to_string()),
            stream: false,
            payload: LlmRequestPayload::Chat(ChatRequest {
                messages: vec![text_message("user", "Hello")],
                max_tokens: Some(1024),
                ..ChatRequest::default()
            }),
            extra_body: ExtensionMap::new(),
            extra_headers: HeaderMap::new(),
            metadata: ExtensionMap::new(),
            extra: ExtensionMap::new(),
        }
    }

    /// Mirrors Go `newMockTokenProvider` (outbound_test.go:521-536).
    fn mock_token_provider(token: &str) -> StaticTokenProvider {
        StaticTokenProvider::new(OAuthCredentials {
            access_token: token.to_string(),
            refresh_token: "mock-refresh-token".to_string(),
            token_type: "Bearer".to_string(),
            ..OAuthCredentials::default()
        })
    }

    fn chat_payload(llm_req: &LlmRequest) -> &ChatRequest {
        match &llm_req.payload {
            LlmRequestPayload::Chat(chat) => chat,
            other => panic!("expected chat payload, got {}", other.request_type()),
        }
    }

    // --- userid.go — mirrors Go userid_test.go -----------------------------

    // Go TestParseUserID_Legacy.
    #[test]
    fn parse_user_id_legacy() {
        let raw = "user_aabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccdd_account__session_7581b58b-1234-5678-9abc-def012345678";
        let uid = parse_user_id(raw).unwrap_or_else(|| panic!("legacy user id must parse"));
        assert_eq!(
            uid.device_id,
            "aabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccdd"
        );
        assert_eq!(uid.account_uuid, "");
        assert_eq!(uid.session_id, "7581b58b-1234-5678-9abc-def012345678");
    }

    // Go TestParseUserID_V2JSON.
    #[test]
    fn parse_user_id_v2_json() {
        let raw = r#"{"device_id":"67bad5aabbccdd1122334455667788990011223344556677889900aabbccddee","account_uuid":"acc-uuid-123","session_id":"7581b58b-1234-5678-9abc-def012345678"}"#;
        let uid = parse_user_id(raw).unwrap_or_else(|| panic!("v2 user id must parse"));
        assert_eq!(
            uid.device_id,
            "67bad5aabbccdd1122334455667788990011223344556677889900aabbccddee"
        );
        assert_eq!(uid.account_uuid, "acc-uuid-123");
        assert_eq!(uid.session_id, "7581b58b-1234-5678-9abc-def012345678");
    }

    // Go TestParseUserID_V2EmptySessionID.
    #[test]
    fn parse_user_id_v2_empty_session_id() {
        let raw = r#"{"device_id":"abc","account_uuid":"","session_id":""}"#;
        assert_eq!(parse_user_id(raw), None);
    }

    // Go TestParseUserID_InvalidInputs.
    #[test]
    fn parse_user_id_invalid_inputs() {
        assert_eq!(parse_user_id(""), None);
        assert_eq!(parse_user_id("   "), None);
        assert_eq!(parse_user_id("random-string"), None);
        assert_eq!(parse_user_id("{invalid json"), None);
        assert_eq!(
            parse_user_id("user_tooshort_account__session_bad-uuid"),
            None
        );
    }

    // Go TestBuildUserID.
    #[test]
    fn build_user_id_round_trip() {
        let uid = UserId {
            device_id: "deadbeef".to_string(),
            account_uuid: "acc-123".to_string(),
            session_id: "sess-456".to_string(),
        };
        let result = build_user_id(&uid);
        assert!(result.contains(r#""device_id":"deadbeef""#));
        assert!(result.contains(r#""session_id":"sess-456""#));

        let parsed = parse_user_id(&result).unwrap_or_else(|| panic!("round trip must parse"));
        assert_eq!(parsed, uid);
    }

    // Go TestGenerateUserID_Random.
    #[test]
    fn generate_user_id_random() {
        let raw = generate_user_id(None, "");
        let uid = parse_user_id(&raw).unwrap_or_else(|| panic!("generated id must parse"));
        assert_eq!(uid.device_id.len(), 64);
        assert!(!uid.session_id.is_empty());
        assert_eq!(uid.account_uuid, "");
    }

    // Go TestGenerateUserID_Stable.
    #[test]
    fn generate_user_id_stable() {
        let uid1 = parse_user_id(&generate_user_id(None, "42"))
            .unwrap_or_else(|| panic!("stable id 1 must parse"));
        let uid2 = parse_user_id(&generate_user_id(None, "42"))
            .unwrap_or_else(|| panic!("stable id 2 must parse"));

        assert_eq!(
            uid1.device_id, uid2.device_id,
            "DeviceID should be deterministic"
        );
        assert_eq!(
            uid1.account_uuid, uid2.account_uuid,
            "AccountUUID should be deterministic"
        );
        assert_eq!(uid1.device_id.len(), 64);
        assert!(!uid1.account_uuid.is_empty());
    }

    // Go TestGenerateUserID_DifferentIdentities.
    #[test]
    fn generate_user_id_different_identities() {
        let uid1 = parse_user_id(&generate_user_id(None, "1"))
            .unwrap_or_else(|| panic!("id 1 must parse"));
        let uid2 = parse_user_id(&generate_user_id(None, "2"))
            .unwrap_or_else(|| panic!("id 2 must parse"));

        assert_ne!(uid1.device_id, uid2.device_id);
        assert_ne!(uid1.account_uuid, uid2.account_uuid);
    }

    // Go TestGenerateUserID_UsesSharedSessionID (shared.WithSessionID → param).
    #[test]
    fn generate_user_id_uses_shared_session_id() {
        let raw = generate_user_id(Some("shared-session-id"), "");
        let uid = parse_user_id(&raw).unwrap_or_else(|| panic!("id must parse"));
        assert_eq!(uid.session_id, "shared-session-id");
    }

    // --- outbound.go — mirrors Go outbound_test.go -------------------------

    // Go "Claude Code always uses Bearer auth" (outbound_test.go:25-43).
    #[test]
    fn transform_request_always_uses_bearer_auth() -> TestResult {
        let mut llm_req = base_llm_request();
        let provider = mock_token_provider("sk-ant-oat01-oauth-token");
        let http_req = build_claude_code_http_request(
            &mut llm_req,
            None,
            &ClaudeCodeParams::default(),
            &provider,
            None,
            None,
        )?;

        let auth = http_req.auth.as_ref().ok_or("auth must be set")?;
        // Go: httpclient.AuthTypeBearer ("bearer") + APIKey carrying the token.
        assert_eq!(auth.scheme, "bearer");
        assert_eq!(auth.token.as_deref(), Some("sk-ant-oat01-oauth-token"));
        Ok(())
    }

    // Go "injects Claude Code system message with cache_control"
    // (outbound_test.go:45-67). The cache_control/array-form `system`
    // serialization is the base transformer's job (Go TransformOptions
    // ArrayInstructions); the Rust S05 base builder does not serialize
    // cache_control yet, so the golden is asserted on the structured message
    // (where Go injects it) plus the body-level system text.
    #[test]
    fn transform_request_injects_claude_code_system_message_with_cache_control() -> TestResult {
        let mut llm_req = base_llm_request();
        let provider = mock_token_provider("test-api-key");
        let http_req = build_claude_code_http_request(
            &mut llm_req,
            None,
            &ClaudeCodeParams::default(),
            &provider,
            None,
            None,
        )?;

        // Structured phase golden — Go's injected message shape (utils.go:266-273).
        let chat = chat_payload(&llm_req);
        let first = chat.messages.first().ok_or("messages must not be empty")?;
        assert_eq!(first.role, "system");
        assert_eq!(
            first.content,
            Some(MessageContent::Text(CLAUDE_CODE_SYSTEM_MESSAGE.to_string()))
        );
        assert_eq!(
            first.extra.get("cache_control"),
            Some(&json!({ "type": "ephemeral" }))
        );
        // Go sets TransformOptions.ArrayInstructions = &true.
        assert_eq!(
            llm_req
                .extra
                .get("transform_options")
                .and_then(|options| options.get("array_instructions")),
            Some(&json!(true))
        );

        // Body-level: the system prompt carries the Claude Code message.
        let body = http_req.json_body.as_ref().ok_or("json body must be set")?;
        let system = body.get("system").ok_or("system must exist")?;
        assert!(
            serde_json::to_string(system)?.contains(CLAUDE_CODE_SYSTEM_MESSAGE),
            "system must contain the Claude Code message: {system}"
        );
        Ok(())
    }

    // Go "sets all Claude Code headers" (outbound_test.go:69-89).
    #[test]
    fn transform_request_sets_all_claude_code_headers() -> TestResult {
        let mut llm_req = base_llm_request();
        let provider = mock_token_provider("test-api-key");
        let http_req = build_claude_code_http_request(
            &mut llm_req,
            None,
            &ClaudeCodeParams::default(),
            &provider,
            None,
            None,
        )?;

        let beta = get_header(&http_req.headers, "Anthropic-Beta").unwrap_or("");
        assert!(
            beta.contains("interleaved-thinking-2025-05-14"),
            "beta: {beta}"
        );
        assert_eq!(
            get_header(&http_req.headers, "Anthropic-Version"),
            Some("2023-06-01")
        );
        assert_eq!(
            get_header(
                &http_req.headers,
                "Anthropic-Dangerous-Direct-Browser-Access"
            ),
            Some("true")
        );
        assert_eq!(get_header(&http_req.headers, "X-App"), Some("cli"));
        assert_eq!(
            get_header(&http_req.headers, "X-Stainless-Helper-Method"),
            Some("stream")
        );
        assert_eq!(
            get_header(&http_req.headers, "User-Agent"),
            Some(USER_AGENT)
        );
        Ok(())
    }

    // Go "adds beta=true query parameter" (outbound_test.go:91-105).
    #[test]
    fn transform_request_adds_beta_true_query_parameter() -> TestResult {
        let mut llm_req = base_llm_request();
        let provider = mock_token_provider("test-api-key");
        let http_req = build_claude_code_http_request(
            &mut llm_req,
            None,
            &ClaudeCodeParams::default(),
            &provider,
            None,
            None,
        )?;

        assert_eq!(
            http_req.query.get("beta").and_then(|values| values.first()),
            Some(&"true".to_string())
        );
        Ok(())
    }

    // Go "applies tool prefix for OAuth tokens from non-CLI clients"
    // (outbound_test.go:107-135).
    #[test]
    fn transform_request_applies_tool_prefix_for_official_non_cli() -> TestResult {
        let mut llm_req = base_llm_request();
        if let LlmRequestPayload::Chat(chat) = &mut llm_req.payload {
            chat.tools = vec![UnifiedTool {
                tool_type: "function".to_string(),
                name: Some("bash".to_string()),
                description: Some("Execute bash".to_string()),
                parameters: None,
                extra: ExtensionMap::new(),
            }];
        }
        let provider = mock_token_provider("sk-ant-oat01-test-oauth-token");
        let params = ClaudeCodeParams {
            is_official: true,
            ..ClaudeCodeParams::default()
        };
        let http_req =
            build_claude_code_http_request(&mut llm_req, None, &params, &provider, None, None)?;

        // Tool name should have the proxy_ prefix in the serialized body.
        let body = http_req.json_body.as_ref().ok_or("json body must be set")?;
        assert_eq!(
            body.get("tools")
                .and_then(|tools| tools.get(0))
                .and_then(|tool| tool.get("name"))
                .and_then(Value::as_str),
            Some("proxy_bash")
        );
        // Metadata should indicate the prefix was applied.
        assert_eq!(
            http_req.metadata.get(STRIP_TOOL_PREFIX_METADATA_KEY),
            Some(&json!("true"))
        );
        assert!(should_strip_tool_prefix(&http_req));
        Ok(())
    }

    // Go "does not apply tool prefix for Claude CLI clients"
    // (outbound_test.go:137-165). Note: the Go case uses a non-official
    // transformer AND a claude-cli UA; either alone suppresses the prefix.
    #[test]
    fn transform_request_no_tool_prefix_for_claude_cli_clients() -> TestResult {
        let mut llm_req = base_llm_request();
        if let LlmRequestPayload::Chat(chat) = &mut llm_req.payload {
            chat.tools = vec![UnifiedTool {
                tool_type: "function".to_string(),
                name: Some("bash".to_string()),
                description: Some("Execute bash".to_string()),
                parameters: None,
                extra: ExtensionMap::new(),
            }];
        }
        let mut inbound = HeaderMap::new();
        inbound.insert("User-Agent".to_string(), "claude-cli/1.0.83".to_string());

        let provider = mock_token_provider("sk-ant-oat01-test-oauth-token");
        let http_req = build_claude_code_http_request(
            &mut llm_req,
            Some(&inbound),
            &ClaudeCodeParams::default(),
            &provider,
            None,
            None,
        )?;

        let body = http_req.json_body.as_ref().ok_or("json body must be set")?;
        assert_eq!(
            body.get("tools")
                .and_then(|tools| tools.get(0))
                .and_then(|tool| tool.get("name"))
                .and_then(Value::as_str),
            Some("bash")
        );
        // Metadata should not indicate prefix.
        assert_eq!(http_req.metadata.get(STRIP_TOOL_PREFIX_METADATA_KEY), None);
        assert!(!should_strip_tool_prefix(&http_req));
        Ok(())
    }

    // Go "injects fake user ID" (outbound_test.go:167-184).
    #[test]
    fn transform_request_injects_fake_user_id() -> TestResult {
        let mut llm_req = base_llm_request();
        let provider = mock_token_provider("test-api-key");
        let http_req = build_claude_code_http_request(
            &mut llm_req,
            None,
            &ClaudeCodeParams::default(),
            &provider,
            None,
            None,
        )?;

        let body = http_req.json_body.as_ref().ok_or("json body must be set")?;
        let user_id = body
            .get("metadata")
            .and_then(|metadata| metadata.get("user_id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        assert!(!user_id.is_empty());
        assert!(
            parse_user_id(user_id).is_some(),
            "user_id must parse: {user_id}"
        );
        Ok(())
    }

    // Go "does not add billing cch when not official" (outbound_test.go:186-215).
    #[test]
    fn transform_request_no_billing_cch_when_not_official() -> TestResult {
        let billing = "x-anthropic-billing-header: cc_version=2.1.37.fbe; cc_entrypoint=cli;";
        let mut llm_req = base_llm_request();
        if let LlmRequestPayload::Chat(chat) = &mut llm_req.payload {
            chat.messages.insert(0, text_message("system", billing));
        }
        let provider = mock_token_provider("test-api-key");
        let params = ClaudeCodeParams {
            is_official: false,
            ..ClaudeCodeParams::default()
        };
        let http_req =
            build_claude_code_http_request(&mut llm_req, None, &params, &provider, None, None)?;

        let body = http_req.json_body.as_ref().ok_or("json body must be set")?;
        let system = serde_json::to_string(body.get("system").ok_or("system must exist")?)?;
        assert!(system.contains("x-anthropic-billing-header"));
        assert!(!system.contains("cch="), "no cch may be added: {system}");
        Ok(())
    }

    // Go "restores billing cch when official and stripped"
    // (outbound_test.go:217-254). The Go request carries
    // TransformerMetadata["claudecode_billing_cch"] = "38a80"; here the
    // extracted value is passed as the billing_cch argument.
    #[test]
    fn transform_request_restores_billing_cch_when_official() -> TestResult {
        let billing = "x-anthropic-billing-header: cc_version=2.1.42.c31; cc_entrypoint=cli;";
        let mut llm_req = base_llm_request();
        if let LlmRequestPayload::Chat(chat) = &mut llm_req.payload {
            chat.messages.insert(0, text_message("system", billing));
        }
        let provider = mock_token_provider("test-api-key");
        let params = ClaudeCodeParams {
            is_official: true,
            ..ClaudeCodeParams::default()
        };
        let http_req = build_claude_code_http_request(
            &mut llm_req,
            None,
            &params,
            &provider,
            None,
            Some("38a80"),
        )?;

        let body = http_req.json_body.as_ref().ok_or("json body must be set")?;
        let system = serde_json::to_string(body.get("system").ok_or("system must exist")?)?;
        assert!(
            system.contains("x-anthropic-billing-header") && system.contains("cch=38a80;"),
            "billing system message should restore cch for official channels: {system}"
        );
        Ok(())
    }

    // Go "disables thinking when tool_choice forces tool use"
    // (outbound_test.go:256-288): tool_choice "any" → the final body must not
    // carry `thinking`. Plus the source-logic cases of
    // disableThinkingIfToolChoiceForcedStructured (utils.go:34-60).
    #[test]
    fn transform_request_disables_thinking_when_tool_choice_forces() -> TestResult {
        let mut llm_req = base_llm_request();
        if let LlmRequestPayload::Chat(chat) = &mut llm_req.payload {
            chat.tool_choice = Some(json!("any"));
        }
        let provider = mock_token_provider("test-api-key");
        let http_req = build_claude_code_http_request(
            &mut llm_req,
            None,
            &ClaudeCodeParams::default(),
            &provider,
            None,
            None,
        )?;
        let body = http_req.json_body.as_ref().ok_or("json body must be set")?;
        assert_eq!(body.get("thinking"), None);
        Ok(())
    }

    #[test]
    fn disable_thinking_source_logic_cases() {
        // "any" + reasoning_effort set → cleared (incl. reasoning_budget).
        let mut chat = ChatRequest {
            tool_choice: Some(json!("any")),
            reasoning_effort: Some("high".to_string()),
            ..ChatRequest::default()
        };
        chat.extra
            .insert("reasoning_budget".to_string(), json!(10000));
        disable_thinking_if_tool_choice_forced(&mut chat);
        assert_eq!(chat.reasoning_effort, None);
        assert_eq!(chat.extra.get("reasoning_budget"), None);

        // Named {type:"tool"} choice + effort → cleared.
        let mut chat = ChatRequest {
            tool_choice: Some(json!({"type": "tool", "function": {"name": "bash"}})),
            reasoning_effort: Some("low".to_string()),
            ..ChatRequest::default()
        };
        disable_thinking_if_tool_choice_forced(&mut chat);
        assert_eq!(chat.reasoning_effort, None);

        // "auto" does not force → effort kept.
        let mut chat = ChatRequest {
            tool_choice: Some(json!("auto")),
            reasoning_effort: Some("high".to_string()),
            ..ChatRequest::default()
        };
        disable_thinking_if_tool_choice_forced(&mut chat);
        assert_eq!(chat.reasoning_effort, Some("high".to_string()));

        // Forced choice but no effort set → no-op (Go: ReasoningEffort == "").
        let mut chat = ChatRequest {
            tool_choice: Some(json!("any")),
            reasoning_effort: None,
            ..ChatRequest::default()
        };
        disable_thinking_if_tool_choice_forced(&mut chat);
        assert_eq!(chat.reasoning_effort, None);
    }

    // Go TestClaudeCodeTransformer_TransformResponse "strips tool prefix when
    // it was applied" (outbound_test.go:294-334). Response body verbatim from
    // the Go test.
    #[test]
    fn transform_response_strips_tool_prefix_when_applied() -> TestResult {
        let response_body = serde_json::to_vec(&json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-5",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_123",
                    "name": "proxy_bash",
                    "input": {"command": "ls"}
                }
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        }))?;

        // The request recorded strip_tool_prefix=true (Go outbound_test.go:319-325).
        let mut request = HttpRequest::default();
        request.metadata.insert(
            STRIP_TOOL_PREFIX_METADATA_KEY.to_string(),
            Value::String("true".to_string()),
        );
        assert!(should_strip_tool_prefix(&request));

        let stripped = strip_claude_tool_prefix_from_response(&response_body, TOOL_PREFIX);
        let parsed: Value = serde_json::from_slice(&stripped)?;
        assert_eq!(
            parsed
                .get("content")
                .and_then(|content| content.get(0))
                .and_then(|block| block.get("name"))
                .and_then(Value::as_str),
            Some("bash")
        );
        Ok(())
    }

    // Go "does not strip when prefix was not applied" (outbound_test.go:336-374):
    // without the metadata marker the body is passed through untouched.
    #[test]
    fn transform_response_does_not_strip_when_not_applied() -> TestResult {
        let response_body = serde_json::to_vec(&json!({
            "content": [
                {"type": "tool_use", "id": "toolu_123", "name": "bash", "input": {"command": "ls"}}
            ]
        }))?;

        let request = HttpRequest::default();
        assert!(!should_strip_tool_prefix(&request));

        // Even when stripping runs, an unprefixed name stays unchanged
        // (Go: TrimPrefix finds nothing).
        let stripped = strip_claude_tool_prefix_from_response(&response_body, TOOL_PREFIX);
        assert_eq!(stripped, response_body);
        Ok(())
    }

    // Go TestClaudeCodeTransformer_TransformStream "strips tool prefix from
    // streaming responses" (outbound_test.go:377-440). Event payload verbatim;
    // both the raw-event strip (AggregateStreamChunks pre-pass) and the
    // unified-response strip (toolPrefixStripperStream) are exercised.
    #[test]
    fn transform_stream_strips_tool_prefix() -> TestResult {
        let event_data = serde_json::to_string(&json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_123", "name": "proxy_bash"}
        }))?;

        // Raw stream-line strip (stripClaudeToolPrefixFromStreamLine).
        let stripped =
            strip_claude_tool_prefix_from_stream_line(event_data.as_bytes(), TOOL_PREFIX);
        let parsed: Value = serde_json::from_slice(&stripped)?;
        assert_eq!(
            parsed
                .get("content_block")
                .and_then(|block| block.get("name"))
                .and_then(Value::as_str),
            Some("bash"),
            "tool prefix should be stripped"
        );

        // Chunk pre-pass (AggregateStreamChunks, outbound.go:320-324).
        let mut chunks = vec![StreamEvent {
            event_type: Some("content_block_start".to_string()),
            data: Some(event_data.clone()),
            ..StreamEvent::default()
        }];
        strip_tool_prefix_from_stream_chunks(&mut chunks, TOOL_PREFIX);
        let data = chunks[0].data.as_deref().ok_or("chunk data must remain")?;
        assert!(data.contains(r#""name":"bash""#), "chunk: {data}");
        assert!(!data.contains("proxy_"), "proxy_ prefix should be removed");

        // Unified stream response strip (toolPrefixStripperStream.Next).
        let mut response = LlmResponse {
            choices: vec![Choice {
                delta: Some(LlmMessage {
                    tool_calls: vec![ToolCall {
                        function: json!({"name": "proxy_bash"}),
                        ..ToolCall::default()
                    }],
                    ..LlmMessage::default()
                }),
                ..Choice::default()
            }],
            ..LlmResponse::default()
        };
        strip_tool_prefix_from_stream_response(&mut response, TOOL_PREFIX);
        let delta = response.choices[0]
            .delta
            .as_ref()
            .ok_or("delta must exist")?;
        assert_eq!(
            delta.tool_calls[0]
                .function
                .get("name")
                .and_then(Value::as_str),
            Some("bash")
        );
        Ok(())
    }

    // Go "handles streams without tool calls" (outbound_test.go:442-470):
    // message_start events pass through unchanged.
    #[test]
    fn transform_stream_handles_streams_without_tool_calls() -> TestResult {
        let event_data = serde_json::to_string(&json!({
            "type": "message_start",
            "message": {"id": "msg_123", "model": "claude-3-5-sonnet-20241022"}
        }))?;

        let stripped =
            strip_claude_tool_prefix_from_stream_line(event_data.as_bytes(), TOOL_PREFIX);
        assert_eq!(stripped, event_data.as_bytes());

        let mut chunks = vec![StreamEvent {
            event_type: Some("message_start".to_string()),
            data: Some(event_data.clone()),
            ..StreamEvent::default()
        }];
        strip_tool_prefix_from_stream_chunks(&mut chunks, TOOL_PREFIX);
        assert_eq!(chunks[0].data.as_deref(), Some(event_data.as_str()));

        // Non-JSON lines pass through unchanged too (Go: unmarshal error → line).
        let raw = b"not-json";
        assert_eq!(
            strip_claude_tool_prefix_from_stream_line(raw, TOOL_PREFIX),
            raw.to_vec()
        );
        Ok(())
    }

    // Go TestClaudeCodeTransformer_APIFormat (outbound_test.go:473-478).
    #[test]
    fn api_format_is_anthropic_messages() {
        assert_eq!(CLAUDE_CODE_API_FORMAT, ApiFormat::AnthropicMessages);
    }

    // --- simulator flow — mirrors Go outbound_simulator_test.go ------------

    // Go TestClaudeCodeTransformer_WithSimulator (simulator_test.go:17-88):
    // URL/query/header/auth/body golden for the full inbound→outbound flow.
    #[test]
    fn simulator_full_request_shape() -> TestResult {
        let mut llm_req = LlmRequest {
            model: Some("claude-3-5-sonnet-20241022".to_string()),
            ..base_llm_request()
        };
        let mut inbound = HeaderMap::new();
        inbound.insert("Content-Type".to_string(), "application/json".to_string());
        inbound.insert("X-Api-Key".to_string(), "client-api-key".to_string());

        let provider = mock_token_provider("test-api-key");
        let http_req = build_claude_code_http_request(
            &mut llm_req,
            Some(&inbound),
            &ClaudeCodeParams::default(),
            &provider,
            None,
            None,
        )?;

        // Go: "https://api.anthropic.com/v1/messages?beta=true".
        assert_eq!(
            http_req.url.as_deref(),
            Some("https://api.anthropic.com/v1/messages")
        );
        assert_eq!(
            http_req.query.get("beta").and_then(|values| values.first()),
            Some(&"true".to_string())
        );
        assert_eq!(
            get_header(&http_req.headers, "Anthropic-Version"),
            Some("2023-06-01")
        );
        assert_eq!(
            get_header(
                &http_req.headers,
                "Anthropic-Dangerous-Direct-Browser-Access"
            ),
            Some("true")
        );
        assert_eq!(get_header(&http_req.headers, "X-App"), Some("cli"));
        // Bearer auth; the inbound X-Api-Key is never forwarded.
        let auth = http_req.auth.as_ref().ok_or("auth must be set")?;
        assert_eq!(
            (auth.scheme.as_str(), auth.token.as_deref()),
            ("bearer", Some("test-api-key"))
        );
        assert_eq!(get_header(&http_req.headers, "X-Api-Key"), None);

        // Body: system carries the Claude Code message, user message intact.
        let body = http_req.json_body.as_ref().ok_or("json body must be set")?;
        let system = serde_json::to_string(body.get("system").ok_or("system must exist")?)?;
        assert!(system.contains(CLAUDE_CODE_SYSTEM_MESSAGE));
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or("messages must be an array")?;
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("user")
        );
        Ok(())
    }

    // Go TestClaudeCodeTransformer_WithSimulator_AlreadyHasBetaQuery
    // (simulator_test.go:90-165): when the query already carries beta=true it
    // is not duplicated, and the default Claude Code headers still apply.
    #[test]
    fn simulator_beta_query_not_duplicated() -> TestResult {
        let mut llm_req = base_llm_request();
        let provider = mock_token_provider("test-api-key");
        let prepared = prepare_claude_code_request(
            &mut llm_req,
            None,
            &ClaudeCodeParams::default(),
            None,
            None,
        );

        let mut http_req = HttpRequest {
            method: "POST".to_string(),
            url: Some("https://api.anthropic.com/v1/messages".to_string()),
            path: "/messages".to_string(),
            ..HttpRequest::default()
        };
        http_req
            .query
            .insert("beta".to_string(), vec!["true".to_string()]);

        let creds = provider.get()?;
        decorate_claude_code_http_request(
            &mut http_req,
            &creds.access_token,
            false,
            None,
            &prepared,
        );

        assert_eq!(
            http_req.query.get("beta").map(Vec::as_slice),
            Some(["true".to_string()].as_slice()),
            "beta=true must not be duplicated"
        );
        let beta = get_header(&http_req.headers, "Anthropic-Beta").unwrap_or("");
        assert!(beta.contains("interleaved-thinking-2025-05-14"));
        assert_eq!(
            get_header(&http_req.headers, "Anthropic-Version"),
            Some("2023-06-01")
        );
        assert!(
            get_header(&http_req.headers, "User-Agent")
                .unwrap_or("")
                .starts_with("claude-cli/")
        );
        Ok(())
    }

    // Go TestClaudeCodeTransformer_WithSimulator_InboundHeadersPassthrough
    // (simulator_test.go:167-252) — table-driven: passthrough headers override
    // the defaults; Anthropic-Beta merges; the UA is kept only for claude-cli
    // clients; Bearer auth; X-Api-Key never forwarded.
    #[test]
    fn simulator_inbound_headers_passthrough() -> TestResult {
        struct Case {
            name: &'static str,
            inbound_ua: &'static str,
            want_final_ua: String,
        }
        let cases = [
            Case {
                name: "non-claude UA passthrough headers override defaults",
                inbound_ua: "conduit-test/0.0.1",
                want_final_ua: USER_AGENT.to_string(),
            },
            Case {
                name: "claude-cli UA passthrough headers override defaults",
                inbound_ua: "claude-cli/1.0.99 (external, cli)",
                want_final_ua: "claude-cli/1.0.99 (external, cli)".to_string(),
            },
        ];

        for case in cases {
            let mut inbound = HeaderMap::new();
            inbound.insert("Content-Type".to_string(), "application/json".to_string());
            inbound.insert("X-Api-Key".to_string(), "client-api-key".to_string());
            inbound.insert("User-Agent".to_string(), case.inbound_ua.to_string());
            inbound.insert("Anthropic-Beta".to_string(), "injected".to_string());
            inbound.insert("Anthropic-Version".to_string(), "1999-01-01".to_string());
            inbound.insert(
                "Anthropic-Dangerous-Direct-Browser-Access".to_string(),
                "false".to_string(),
            );
            inbound.insert("X-App".to_string(), "web".to_string());
            inbound.insert(
                "X-Stainless-Package-Version".to_string(),
                "999.0.0".to_string(),
            );

            let mut llm_req = base_llm_request();
            let provider = mock_token_provider("test-api-key");
            let http_req = build_claude_code_http_request(
                &mut llm_req,
                Some(&inbound),
                &ClaudeCodeParams::default(),
                &provider,
                None,
                None,
            )?;

            let name = case.name;
            let beta = get_header(&http_req.headers, "Anthropic-Beta").unwrap_or("");
            assert!(beta.contains("injected"), "{name}: beta {beta}");
            // The merge preserves the required default betas (utils.go:119-146).
            assert!(beta.contains("claude-code-20250219"), "{name}: beta {beta}");
            assert_eq!(
                get_header(&http_req.headers, "Anthropic-Version"),
                Some("1999-01-01"),
                "{name}"
            );
            assert_eq!(
                get_header(
                    &http_req.headers,
                    "Anthropic-Dangerous-Direct-Browser-Access"
                ),
                Some("false"),
                "{name}"
            );
            assert_eq!(
                get_header(&http_req.headers, "User-Agent"),
                Some(case.want_final_ua.as_str()),
                "{name}"
            );
            assert_eq!(
                get_header(&http_req.headers, "X-App"),
                Some("web"),
                "{name}"
            );
            assert_eq!(
                get_header(&http_req.headers, "X-Stainless-Package-Version"),
                Some("999.0.0"),
                "{name}"
            );
            let auth = http_req.auth.as_ref().ok_or("auth must be set")?;
            assert_eq!(auth.scheme, "bearer", "{name}");
            assert_eq!(auth.token.as_deref(), Some("test-api-key"), "{name}");
            assert_eq!(get_header(&http_req.headers, "X-Api-Key"), None, "{name}");
        }
        Ok(())
    }

    // X-Claude-Code-Session-Id rides the passthrough list (outbound.go:44-49).
    #[test]
    fn session_id_header_is_passed_through() -> TestResult {
        let mut inbound = HeaderMap::new();
        inbound.insert(
            "X-Claude-Code-Session-Id".to_string(),
            "sess-abc".to_string(),
        );
        let mut llm_req = base_llm_request();
        let provider = mock_token_provider("test-api-key");
        let http_req = build_claude_code_http_request(
            &mut llm_req,
            Some(&inbound),
            &ClaudeCodeParams::default(),
            &provider,
            None,
            None,
        )?;
        assert_eq!(
            get_header(&http_req.headers, "X-Claude-Code-Session-Id"),
            Some("sess-abc")
        );
        Ok(())
    }

    // --- utils.go source-logic unit tests -----------------------------------

    // mergeBetasIntoHeader (utils.go:119-146).
    #[test]
    fn merge_betas_into_header_cases() {
        assert_eq!(merge_betas_into_header("", &["x"]), "x");
        assert_eq!(merge_betas_into_header("a, b", &["b", "c"]), "a,b,c");
        assert_eq!(merge_betas_into_header(" a ,  ,b ", &[]), "a,b");
        // Duplicates within the base are preserved (Go only dedupes extras).
        assert_eq!(merge_betas_into_header("a,a", &["a"]), "a,a");
        assert_eq!(merge_betas_into_header("base", &["", "  "]), "base");
    }

    // injectClaudeCodeSystemMessageStructured (utils.go:265-291).
    #[test]
    fn inject_claude_code_system_message_cases() {
        // Prepends and sets the array-instructions flag.
        let mut llm_req = base_llm_request();
        inject_claude_code_system_message(&mut llm_req);
        {
            let chat = chat_payload(&llm_req);
            assert_eq!(chat.messages.len(), 2);
            assert_eq!(chat.messages[0].role, "system");
            assert_eq!(
                chat.messages[0].extra.get("cache_control"),
                Some(&json!({"type": "ephemeral"}))
            );
        }
        assert_eq!(
            llm_req
                .extra
                .get("transform_options")
                .and_then(|options| options.get("array_instructions")),
            Some(&json!(true))
        );

        // Idempotent: already-injected first message → early return, and the
        // flag stays untouched (Go returns before setting ArrayInstructions).
        let mut llm_req = base_llm_request();
        if let LlmRequestPayload::Chat(chat) = &mut llm_req.payload {
            chat.messages
                .insert(0, text_message("system", CLAUDE_CODE_SYSTEM_MESSAGE));
        }
        inject_claude_code_system_message(&mut llm_req);
        assert_eq!(chat_payload(&llm_req).messages.len(), 2);
        assert_eq!(llm_req.extra.get("transform_options"), None);

        // A different leading system message still gets the prepend.
        let mut llm_req = base_llm_request();
        if let LlmRequestPayload::Chat(chat) = &mut llm_req.payload {
            chat.messages.insert(0, text_message("system", "other"));
        }
        inject_claude_code_system_message(&mut llm_req);
        let chat = chat_payload(&llm_req);
        assert_eq!(chat.messages.len(), 3);
        assert_eq!(
            chat.messages[0].content,
            Some(MessageContent::Text(CLAUDE_CODE_SYSTEM_MESSAGE.to_string()))
        );
    }

    // removeBillingSystemMessages (utils.go:155-174).
    #[test]
    fn remove_billing_system_messages_cases() {
        let mut chat = ChatRequest {
            messages: vec![
                text_message("system", "  x-anthropic-billing-header: cc_version=1;"),
                text_message("system", "keep me"),
                text_message("user", "x-anthropic-billing-header: not a system role"),
            ],
            ..ChatRequest::default()
        };
        remove_billing_system_messages(&mut chat);
        assert_eq!(chat.messages.len(), 2);
        assert_eq!(
            chat.messages[0].content,
            Some(MessageContent::Text("keep me".to_string()))
        );
        // Go HasPrefix is case-sensitive here — an uppercase variant survives.
        let mut chat = ChatRequest {
            messages: vec![text_message("system", "X-Anthropic-Billing-Header: v=1;")],
            ..ChatRequest::default()
        };
        remove_billing_system_messages(&mut chat);
        assert_eq!(chat.messages.len(), 1);
    }

    // ensureBillingHeaderCCHInText (utils.go:226-262).
    #[test]
    fn ensure_billing_header_cch_in_text_cases() {
        // Appends when missing (trailing semicolon present).
        let (out, changed) = ensure_billing_header_cch_in_text(
            "x-anthropic-billing-header: cc_version=2.1.42.c31; cc_entrypoint=cli;",
            "38a80",
        );
        assert!(changed);
        assert_eq!(
            out,
            "x-anthropic-billing-header: cc_version=2.1.42.c31; cc_entrypoint=cli; cch=38a80;"
        );

        // Appends a semicolon first when the text lacks one.
        let (out, changed) =
            ensure_billing_header_cch_in_text("x-anthropic-billing-header: a=1", "z");
        assert!(changed);
        assert_eq!(out, "x-anthropic-billing-header: a=1; cch=z;");

        // Existing cch → untouched (case-insensitive key match).
        let (out, changed) =
            ensure_billing_header_cch_in_text("x-anthropic-billing-header: a=1; CCH=old;", "new");
        assert!(!changed);
        assert_eq!(out, "x-anthropic-billing-header: a=1; CCH=old;");

        // Case-insensitive prefix match (Go lowercases before HasPrefix).
        let (_, changed) =
            ensure_billing_header_cch_in_text("X-Anthropic-Billing-Header: a=1;", "z");
        assert!(changed);

        // Non-billing text and empty rest → untouched.
        assert!(!ensure_billing_header_cch_in_text("hello", "z").1);
        assert!(!ensure_billing_header_cch_in_text("x-anthropic-billing-header:   ", "z").1);
    }

    // applyClaudeToolPrefixStructured (utils.go:63-86).
    #[test]
    fn apply_claude_tool_prefix_cases() {
        let mut chat = ChatRequest {
            tools: vec![
                UnifiedTool {
                    tool_type: "function".to_string(),
                    name: Some("bash".to_string()),
                    description: None,
                    parameters: None,
                    extra: ExtensionMap::new(),
                },
                UnifiedTool {
                    tool_type: "function".to_string(),
                    name: Some("proxy_already".to_string()),
                    description: None,
                    parameters: None,
                    extra: ExtensionMap::new(),
                },
            ],
            tool_choice: Some(json!({"type": "tool", "function": {"name": "bash"}})),
            ..ChatRequest::default()
        };
        apply_claude_tool_prefix(&mut chat, TOOL_PREFIX);
        assert_eq!(chat.tools[0].name.as_deref(), Some("proxy_bash"));
        // Already prefixed → unchanged (Go HasPrefix guard).
        assert_eq!(chat.tools[1].name.as_deref(), Some("proxy_already"));
        assert_eq!(
            chat.tool_choice,
            Some(json!({"type": "tool", "function": {"name": "proxy_bash"}}))
        );

        // Non-"tool" named choices are untouched (Go checks Type == "tool").
        let mut chat = ChatRequest {
            tool_choice: Some(json!({"type": "function", "function": {"name": "bash"}})),
            ..ChatRequest::default()
        };
        apply_claude_tool_prefix(&mut chat, TOOL_PREFIX);
        assert_eq!(
            chat.tool_choice,
            Some(json!({"type": "function", "function": {"name": "bash"}}))
        );
    }

    // The TransformerMetadata lookup inside ensureBillingSystemMessageCCH
    // (utils.go:181-189).
    #[test]
    fn billing_cch_from_transformer_metadata_cases() {
        let mut metadata = ExtensionMap::new();
        assert_eq!(billing_cch_from_transformer_metadata(&metadata), None);
        metadata.insert(BILLING_CCH_METADATA_KEY.to_string(), json!(42));
        assert_eq!(billing_cch_from_transformer_metadata(&metadata), None);
        metadata.insert(BILLING_CCH_METADATA_KEY.to_string(), json!("   "));
        assert_eq!(billing_cch_from_transformer_metadata(&metadata), None);
        metadata.insert(BILLING_CCH_METADATA_KEY.to_string(), json!("  38a80 "));
        assert_eq!(
            billing_cch_from_transformer_metadata(&metadata),
            Some("38a80".to_string())
        );
    }

    // --- oauth credentials / token logic ------------------------------------

    // OAuthCredentials serde shape — Go credentials.go:10-18 tags: omitempty
    // fields skipped, access_token/refresh_token/expires_at always present,
    // zero time as Go's `0001-01-01T00:00:00Z`, declaration order preserved.
    #[test]
    fn oauth_credentials_json_shape() -> TestResult {
        let creds = OAuthCredentials {
            access_token: "tok".to_string(),
            ..OAuthCredentials::default()
        };
        assert_eq!(
            creds.to_json()?,
            r#"{"access_token":"tok","refresh_token":"","expires_at":"0001-01-01T00:00:00Z"}"#
        );

        let full = OAuthCredentials {
            client_id: "client-1".to_string(),
            access_token: "a".to_string(),
            refresh_token: "r".to_string(),
            id_token: "id".to_string(),
            expires_at: DateTime::parse_from_rfc3339("2026-07-04T10:00:00Z")?.with_timezone(&Utc),
            token_type: "Bearer".to_string(),
            scopes: vec!["openid".to_string(), "profile".to_string()],
        };
        assert_eq!(
            full.to_json()?,
            r#"{"client_id":"client-1","access_token":"a","refresh_token":"r","id_token":"id","expires_at":"2026-07-04T10:00:00Z","token_type":"Bearer","scopes":["openid","profile"]}"#
        );

        // Round trip.
        let parsed: OAuthCredentials = serde_json::from_str(&full.to_json()?)?;
        assert_eq!(parsed, full);
        Ok(())
    }

    // ParseCredentialsJSON (credentials.go:43-64).
    #[test]
    fn parse_credentials_json_cases() -> TestResult {
        let now = DateTime::parse_from_rfc3339("2026-07-04T10:00:00Z")?.with_timezone(&Utc);

        // Empty / blank input.
        assert_eq!(
            parse_credentials_json("", now).err().map(|err| err.message),
            Some("empty credentials".to_string())
        );
        assert_eq!(
            parse_credentials_json("   ", now)
                .err()
                .map(|err| err.message),
            Some("empty credentials".to_string())
        );

        // Missing access token.
        assert_eq!(
            parse_credentials_json(r#"{"refresh_token":"r"}"#, now)
                .err()
                .map(|err| err.message),
            Some("access_token is empty".to_string())
        );

        // refresh_token present + no expires_at → assume 1 hour (Go :58-61).
        let creds = parse_credentials_json(r#"{"access_token":"a","refresh_token":"r"}"#, now)?;
        assert_eq!(creds.expires_at, now + duration_secs(3600));

        // Explicit expires_at is preserved; no refresh token → zero stays.
        let creds = parse_credentials_json(
            r#"{"access_token":"a","refresh_token":"r","expires_at":"2026-07-04T12:00:00Z"}"#,
            now,
        )?;
        assert_eq!(creds.expires_at.to_rfc3339(), "2026-07-04T12:00:00+00:00");
        let creds = parse_credentials_json(r#"{"access_token":"a"}"#, now)?;
        assert!(creds.expires_at_is_zero());
        Ok(())
    }

    // IsExpired (credentials.go:66-77) — zero → expired; 3-minute early skew.
    #[test]
    fn is_expired_cases() -> TestResult {
        let now = DateTime::parse_from_rfc3339("2026-07-04T10:00:00Z")?.with_timezone(&Utc);
        let creds = |expires_at| OAuthCredentials {
            access_token: "a".to_string(),
            expires_at,
            ..OAuthCredentials::default()
        };

        assert!(creds(go_zero_time()).is_expired(now));
        assert!(creds(now - duration_secs(60)).is_expired(now));
        // Within the 3-minute skew window → treated expired.
        assert!(creds(now + duration_secs(2 * 60)).is_expired(now));
        // Beyond the window → valid.
        assert!(!creds(now + duration_secs(10 * 60)).is_expired(now));
        Ok(())
    }

    // ParseTokenResponse success — golden values from Go
    // TestTokenProviderExchangeSuccess (token_provider_test.go:55-122).
    #[test]
    fn parse_token_response_success() -> TestResult {
        let now = DateTime::parse_from_rfc3339("2026-07-04T10:00:00Z")?.with_timezone(&Utc);
        let body = serde_json::to_vec(&json!({
            "access_token": "access-1",
            "refresh_token": "refresh-1",
            "token_type": "Bearer",
            "scope": "openid profile",
            "expires_in": 3600
        }))?;

        let creds = parse_token_response(&body, "client-1", now)?;
        assert_eq!(creds.client_id, "client-1");
        assert_eq!(creds.access_token, "access-1");
        assert_eq!(creds.refresh_token, "refresh-1");
        assert_eq!(creds.token_type, "Bearer");
        assert_eq!(
            creds.scopes,
            vec!["openid".to_string(), "profile".to_string()]
        );
        assert_eq!(creds.expires_at, now + duration_secs(3600));
        Ok(())
    }

    // Token error payload — golden from Go TestTokenProviderExchangeErrorResponse
    // (token_provider_test.go:124-148): the refresh-path message keeps the
    // "token request failed" prefix; the exchange path rewrites it.
    #[test]
    fn parse_token_response_error_payload() -> TestResult {
        let now = Utc::now();
        let body = serde_json::to_vec(
            &json!({"error": "invalid_grant", "error_description": "bad code"}),
        )?;

        assert_eq!(
            parse_token_response(&body, "client-1", now)
                .err()
                .map(|err| err.message),
            Some("token request failed: invalid_grant - bad code".to_string())
        );
        assert_eq!(
            parse_exchange_response(&body, "client-1", now)
                .err()
                .map(|err| err.message),
            Some("token exchange failed: invalid_grant - bad code".to_string())
        );

        // Missing access_token without an error object (credentials.go:169-175).
        let body = serde_json::to_vec(&json!({"token_type": "Bearer"}))?;
        assert_eq!(
            parse_token_response(&body, "client-1", now)
                .err()
                .map(|err| err.message),
            Some("token response missing access_token".to_string())
        );

        // Undecodable body (Go "decode response: %w").
        let err = parse_token_response(b"not-json", "client-1", now)
            .err()
            .map(|err| err.message)
            .unwrap_or_default();
        assert!(err.starts_with("decode response:"), "{err}");
        Ok(())
    }

    // JSONStrategy.BuildRefreshRequest (exchange_strategy.go:126-160) +
    // claudecode.NewTokenProvider defaults (token_provider.go:9-19): JSON body
    // with alphabetical keys (Go map marshal), CT/Accept json, pinned CLI UA,
    // POST against the production token URL.
    #[test]
    fn build_refresh_request_shape() -> TestResult {
        let creds = OAuthCredentials {
            client_id: "client-1".to_string(),
            access_token: "access-1".to_string(),
            refresh_token: "refresh-1".to_string(),
            ..OAuthCredentials::default()
        };
        let request = build_refresh_request(&creds)?;

        assert_eq!(request.method, "POST");
        assert_eq!(request.url.as_deref(), Some(TOKEN_URL));
        assert_eq!(
            get_header(&request.headers, "Content-Type"),
            Some("application/json")
        );
        assert_eq!(
            get_header(&request.headers, "Accept"),
            Some("application/json")
        );
        assert_eq!(get_header(&request.headers, "User-Agent"), Some(USER_AGENT));
        let body = request.body.as_ref().ok_or("body must be set")?;
        assert_eq!(
            std::str::from_utf8(body)?,
            r#"{"client_id":"client-1","grant_type":"refresh_token","refresh_token":"refresh-1"}"#
        );

        // Missing refresh token (Go exchange_strategy.go:131-133).
        assert_eq!(
            build_refresh_request(&OAuthCredentials::default())
                .err()
                .map(|err| err.message),
            Some("refresh_token is empty".to_string())
        );
        Ok(())
    }

    // TokenProvider.Exchange validation order (token_provider.go:105-119,
    // mirrored from Go TestTokenProviderExchangeValidation) +
    // JSONStrategy.BuildExchangeRequest body incl. state
    // (exchange_strategy.go:90-123).
    #[test]
    fn build_exchange_request_shape_and_validation() -> TestResult {
        assert_eq!(
            build_exchange_request(&ExchangeParams::default())
                .err()
                .map(|err| err.message),
            Some("code is empty".to_string())
        );
        assert_eq!(
            build_exchange_request(&ExchangeParams {
                code: "code".to_string(),
                ..ExchangeParams::default()
            })
            .err()
            .map(|err| err.message),
            Some("code_verifier is empty".to_string())
        );
        assert_eq!(
            build_exchange_request(&ExchangeParams {
                code: "code".to_string(),
                code_verifier: "verifier".to_string(),
                ..ExchangeParams::default()
            })
            .err()
            .map(|err| err.message),
            Some("client_id is empty".to_string())
        );
        assert_eq!(
            build_exchange_request(&ExchangeParams {
                code: "code".to_string(),
                code_verifier: "verifier".to_string(),
                client_id: "client".to_string(),
                ..ExchangeParams::default()
            })
            .err()
            .map(|err| err.message),
            Some("redirect_uri is empty".to_string())
        );

        let request = build_exchange_request(&ExchangeParams {
            code: "code-1".to_string(),
            code_verifier: "verifier-1".to_string(),
            client_id: CLIENT_ID.to_string(),
            redirect_uri: REDIRECT_URI.to_string(),
            state: "state-1".to_string(),
        })?;
        assert_eq!(request.method, "POST");
        assert_eq!(request.url.as_deref(), Some(TOKEN_URL));
        let body = request.body.as_ref().ok_or("body must be set")?;
        assert_eq!(
            std::str::from_utf8(body)?,
            r#"{"client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e","code":"code-1","code_verifier":"verifier-1","grant_type":"authorization_code","redirect_uri":"http://localhost:54545/callback","state":"state-1"}"#
        );
        Ok(())
    }

    // TokenProvider.refresh tail (token_provider.go:487-495): the old refresh
    // token is preserved when the response omits one; client_id carries over.
    #[test]
    fn parse_refresh_response_preserves_refresh_token() -> TestResult {
        let now = Utc::now();
        let current = OAuthCredentials {
            client_id: "client-1".to_string(),
            access_token: "old".to_string(),
            refresh_token: "refresh-1".to_string(),
            ..OAuthCredentials::default()
        };

        let body = serde_json::to_vec(&json!({"access_token": "access-2", "expires_in": 120}))?;
        let refreshed = parse_refresh_response(&body, &current, now)?;
        assert_eq!(refreshed.access_token, "access-2");
        assert_eq!(refreshed.refresh_token, "refresh-1");
        assert_eq!(refreshed.client_id, "client-1");

        let body =
            serde_json::to_vec(&json!({"access_token": "access-2", "refresh_token": "refresh-2"}))?;
        let refreshed = parse_refresh_response(&body, &current, now)?;
        assert_eq!(refreshed.refresh_token, "refresh-2");
        Ok(())
    }

    // EnsureFresh decision (token_provider.go:208-231).
    #[test]
    fn should_refresh_cases() -> TestResult {
        let now = DateTime::parse_from_rfc3339("2026-07-04T10:00:00Z")?.with_timezone(&Utc);
        let creds = |refresh_token: &str, expires_at| OAuthCredentials {
            access_token: "a".to_string(),
            refresh_token: refresh_token.to_string(),
            expires_at,
            ..OAuthCredentials::default()
        };
        let five_min = duration_secs(5 * 60);

        // No refresh token → never refresh.
        assert!(!should_refresh(&creds("", go_zero_time()), now, five_min));
        // Zero expiry → refresh.
        assert!(should_refresh(&creds("r", go_zero_time()), now, five_min));
        // Inside the window → refresh; outside → not.
        assert!(should_refresh(
            &creds("r", now + duration_secs(60)),
            now,
            five_min
        ));
        assert!(!should_refresh(
            &creds("r", now + duration_secs(600)),
            now,
            five_min
        ));
        // refresh_before <= 0 defaults to 5 minutes (Go :221-223).
        assert!(should_refresh(
            &creds("r", now + duration_secs(60)),
            now,
            chrono::Duration::zero()
        ));
        assert!(!should_refresh(
            &creds("r", now + duration_secs(600)),
            now,
            chrono::Duration::zero()
        ));
        Ok(())
    }

    // nextAutoRefreshDelay (token_provider.go:393-414).
    #[test]
    fn next_auto_refresh_delay_cases() -> TestResult {
        let now = DateTime::parse_from_rfc3339("2026-07-04T10:00:00Z")?.with_timezone(&Utc);
        let five_min = duration_secs(5 * 60);
        let fallback = duration_secs(30);

        // nil creds / no refresh token / zero expiry → fallback (default 1m).
        assert_eq!(
            next_auto_refresh_delay(None, now, five_min, fallback),
            fallback
        );
        assert_eq!(
            next_auto_refresh_delay(None, now, five_min, chrono::Duration::zero()),
            duration_secs(60)
        );
        let no_refresh = OAuthCredentials {
            access_token: "a".to_string(),
            ..OAuthCredentials::default()
        };
        assert_eq!(
            next_auto_refresh_delay(Some(&no_refresh), now, five_min, fallback),
            fallback
        );

        // Sleep until expires_at - refresh_before, clamped at zero.
        let creds = OAuthCredentials {
            access_token: "a".to_string(),
            refresh_token: "r".to_string(),
            expires_at: now + duration_secs(600),
            ..OAuthCredentials::default()
        };
        assert_eq!(
            next_auto_refresh_delay(Some(&creds), now, five_min, fallback),
            duration_secs(300)
        );
        let expired = OAuthCredentials {
            expires_at: now - duration_secs(600),
            ..creds.clone()
        };
        assert_eq!(
            next_auto_refresh_delay(Some(&expired), now, five_min, fallback),
            chrono::Duration::zero()
        );
        Ok(())
    }

    // StaticTokenProvider (token_provider.go:416-427) + TokenGetter object safety.
    #[test]
    fn static_token_provider_returns_credentials() -> TestResult {
        let provider = mock_token_provider("tok-1");
        let getter: &dyn TokenGetter = &provider;
        let creds = getter.get()?;
        assert_eq!(creds.access_token, "tok-1");
        assert_eq!(creds.refresh_token, "mock-refresh-token");
        assert_eq!(creds.token_type, "Bearer");
        Ok(())
    }

    // DefaultTokenURLs (token_provider.go:22-25) + Params base URL default
    // (outbound.go:65-68).
    #[test]
    fn default_urls_and_base_url() {
        let urls = default_token_urls();
        assert_eq!(urls.authorize_url, AUTHORIZE_URL);
        assert_eq!(urls.token_url, TOKEN_URL);

        assert_eq!(
            ClaudeCodeParams::default().effective_base_url(),
            "https://api.anthropic.com/v1"
        );
        let custom = ClaudeCodeParams {
            base_url: "https://proxy.example.com/v1".to_string(),
            ..ClaudeCodeParams::default()
        };
        assert_eq!(custom.effective_base_url(), "https://proxy.example.com/v1");

        // Header tables (outbound.go:28-49): 13 default pairs + session id
        // appended to the passthrough list.
        assert_eq!(CLAUDE_CODE_HEADERS.len(), 13);
        assert_eq!(PASSTHROUGH_HEADERS.len(), 14);
        assert_eq!(PASSTHROUGH_HEADERS[13], "X-Claude-Code-Session-Id");
    }
}
