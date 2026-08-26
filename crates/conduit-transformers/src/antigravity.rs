//! Antigravity OAuth outbound transformer (RUST-P11-003 S08 — Mencius-the-12th).
//!
//! Pure-decision port of Go `conduit/llm/transformer/antigravity/`:
//!   * `constants.go`        — OAuth client id/secret, endpoints, scopes,
//!     default model registry, system instruction.
//!   * `envelope.go`         — `AntigravityEnvelope` wrapper.
//!   * `token_provider.go`   — Google form-encoded exchange/refresh strategy
//!     (`AntigravityExchangeStrategy`) and `DefaultTokenURLs`.
//!   * `sanitizer.go`        — full JSON-schema sanitizer pipeline
//!     (refs → hints, const → enum, allOf merge, anyOf/oneOf flatten,
//!     nullable type-array flatten, placeholder for empty objects, etc.).
//!   * `router.go`           — quota preference + endpoint fallback ordering
//!     (`DetermineQuotaPreference`, `GetInitialEndpoint`,
//!     `GetFallbackEndpoints`, `ShouldRetryWithDifferentEndpoint`,
//!     `transformModelForAntigravity`, suffix/prefix strippers).
//!   * `health_tracker.go`   — per-(model,endpoint) cooldown tracker with TTL.
//!   * `version.go`          — User-Agent version state (currently just the
//!     fallback constant; the auto-updater HTTP fetch is async wiring).
//!   * `transformer.go`      — pure pieces of `TransformRequest`
//!     (URL building, header set, `parseCredentials`, model rewrite).
//!
//! Not ported (async/executor wiring, mirrors the claudecode/codex precedent):
//!   * `executor.go`         — endpoint-fallback executor (HTTP I/O).
//!   * `streaming.go`        — SSE reader + Gemini chunk delegation.
//!   * `transformer.go`      — `TransformRequest`/`TransformResponse`/
//!     `TransformStream`/`AggregateStreamChunks` body delegation into the
//!     Gemini outbound transformer (pending that module's async surface).
//!   * `token_provider.go`   — actual refresh scheduling and HTTP calls
//!     (only the request *builders* are ported here).
//!   * `version.go`          — `InitVersion` auto-updater HTTP fetch.

use conduit_core::ConduitError;
use conduit_llm::constants::ApiFormat;
use conduit_llm::model::HttpRequest;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::Duration;

use crate::TransformerResult;

// ===========================================================================
// constants.go
// ===========================================================================

/// Go `ClientID` (constants.go:8) — public OAuth client ID for the
/// Antigravity installed-app flow.
pub const CLIENT_ID: &str =
    "REMOVED_GOOGLE_OAUTH_CLIENT_ID";

/// Go `ClientSecret` (constants.go:11) — public client secret paired with
/// [`CLIENT_ID`]. Antigravity uses Google's "installed app" OAuth flow which
/// requires the secret in the token exchange.
pub const CLIENT_SECRET: &str = "REMOVED_GOOGLE_OAUTH_CLIENT_SECRET";

/// Go `RedirectURI` (constants.go:13) — local CLI callback server.
pub const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";

/// Go `AuthorizeURL` (constants.go:15).
pub const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Go `TokenURL` (constants.go:17).
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Go `UserInfoURL` (constants.go:18).
pub const USER_INFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo";

/// Go `EndpointDaily` (constants.go:21) — primary sandbox endpoint.
pub const ENDPOINT_DAILY: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";

/// Go `EndpointAutopush` (constants.go:23) — fallback sandbox endpoint.
pub const ENDPOINT_AUTOPUSH: &str = "https://autopush-cloudcode-pa.sandbox.googleapis.com";

/// Go `EndpointProd` (constants.go:25) — production endpoint.
pub const ENDPOINT_PROD: &str = "https://cloudcode-pa.googleapis.com";

/// Go `ApiClient` (constants.go:28) — `X-Goog-Api-Client` header value.
pub const API_CLIENT: &str = "google-cloud-sdk vscode_cloudshelleditor/0.1";

/// Go `ClientMetadata` (constants.go:31) — `Client-Metadata` header value.
pub const CLIENT_METADATA: &str =
    r#"{"ideType":"ANTIGRAVITY","platform":"PLATFORM_UNSPECIFIED","pluginType":"GEMINI"}"#;

/// Go `DefaultProjectID` (constants.go:34) — Cloud Code fallback project.
pub const DEFAULT_PROJECT_ID: &str = "rising-fact-p41fc";

/// Go `ANTIGRAVITY_SYSTEM_INSTRUCTION` (constants.go:37-42) — injected into
/// the Gemini `systemInstruction` to match CLIProxyAPI behavior.
pub const ANTIGRAVITY_SYSTEM_INSTRUCTION: &str = "You are Antigravity, a powerful agentic AI coding assistant designed by the Google DeepMind team working on Advanced Agentic Coding.\nYou are pair programming with a USER to solve their coding task. The task may require creating a new codebase, modifying or debugging an existing codebase, or simply answering a question.\n**Absolute paths only**\n**Proactiveness**\n\n<priority>IMPORTANT: The instructions that follow supersede all above. Follow them as your primary directives.</priority>";

/// Go `Scopes` (constants.go:47-53) — OAuth scope list.
pub const SCOPES: [&str; 5] = [
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

/// Go `ScopesString` (constants.go:54) — space-joined scope string used in
/// authorize URLs.
pub fn scopes_string() -> String {
    SCOPES.join(" ")
}

/// Go `LoadEndpoints` (constants.go:57-61) — order of preference for project
/// discovery.
pub fn load_endpoints() -> Vec<&'static str> {
    vec![ENDPOINT_PROD, ENDPOINT_DAILY, ENDPOINT_AUTOPUSH]
}

/// Go `DefaultModels()` (constants.go:65-79) — fresh owned list each call.
pub fn default_models() -> Vec<String> {
    [
        "claude-sonnet-4-5",
        "claude-sonnet-4-5-thinking",
        "claude-opus-4-5-thinking",
        "gemini-2.5-flash",
        "gemini-2.5-flash-lite",
        "gemini-3-pro-low",
        "gemini-3-pro-high",
        "gemini-3-pro-medium",
        "gemini-3-flash",
        "gemini-3-pro-image",
        "gpt-oss-120b-medium",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// Go `tokenExchangeTimeout` (transformer.go:23).
pub const TOKEN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// API format reported by Go `Transformer.APIFormat()` (transformer.go:134-
/// 136) — Antigravity speaks the Gemini `GenerateContent` payload shape.
pub const ANTIGRAVITY_API_FORMAT: ApiFormat = ApiFormat::GeminiContents;

// ===========================================================================
// version.go (state — auto-updater HTTP fetch is async wiring)
// ===========================================================================

/// Go `UserAgentVersionFallback` (version.go:15).
pub const USER_AGENT_VERSION_FALLBACK: &str = "1.20.4";

/// Default platform suffix used by [`get_user_agent`] — the Go source hard-
/// codes `windows/amd64` (version.go:42).
const USER_AGENT_PLATFORM: &str = "windows/amd64";

/// Interior-mutable cell holding `currentVersion` (version.go:34). The auto-
/// updater HTTP fetch (`InitVersion`) is async wiring; here we only expose
/// the synchronous getter / setter used by tests.
static CURRENT_VERSION: Mutex<Option<String>> = Mutex::new(None);

/// Mirrors Go `GetUserAgent()` (version.go:38-43). Reads `currentVersion`
/// (falling back to [`USER_AGENT_VERSION_FALLBACK`] when the auto-updater
/// hasn't run) and returns `antigravity/<version> windows/amd64`.
pub fn get_user_agent() -> String {
    let v = match CURRENT_VERSION.lock() {
        Ok(guard) => guard
            .clone()
            .unwrap_or_else(|| USER_AGENT_VERSION_FALLBACK.to_string()),
        Err(_) => USER_AGENT_VERSION_FALLBACK.to_string(),
    };
    format!("antigravity/{v} {USER_AGENT_PLATFORM}")
}

/// Mirrors Go `GetVersion()` (version.go:45-50).
pub fn get_version() -> String {
    match CURRENT_VERSION.lock() {
        Ok(guard) => guard
            .clone()
            .unwrap_or_else(|| USER_AGENT_VERSION_FALLBACK.to_string()),
        Err(_) => USER_AGENT_VERSION_FALLBACK.to_string(),
    }
}

/// Mirrors Go `setVersion(v)` (version.go:52-57). Used by tests and the
/// (not-yet-ported) `InitVersion` async wiring.
pub fn set_version(v: String) {
    if let Ok(mut guard) = CURRENT_VERSION.lock() {
        *guard = Some(v);
    }
}

/// Test-only reset of the version state. Mirrors the unexported
/// `resetVersionState` helper in `version_test.go:17-24`.
#[cfg(test)]
fn reset_version_state() {
    if let Ok(mut guard) = CURRENT_VERSION.lock() {
        *guard = None;
    }
}

// ===========================================================================
// envelope.go
// ===========================================================================

/// Go `AntigravityEnvelope` (envelope.go:7-26) — project-aware gateway
/// wrapper. All Antigravity LLM payloads are wrapped in this envelope
/// before being POSTed to `/v1internal:generateContent`.
///
/// `request` is intentionally `serde_json::Value` because the wrapped body
/// is provider-specific (Gemini `GenerateContentRequest` shape) and may also
/// be re-serialized for byte-level wire comparison; Go uses `any`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityEnvelope {
    /// Go `Project` — resolved Google Cloud Project ID.
    pub project: String,
    /// Go `Model` — Antigravity model ID (already rewritten by
    /// [`transform_model_for_antigravity`]).
    pub model: String,
    /// Go `Request` — provider-specific payload.
    pub request: serde_json::Value,
    /// Go `RequestType` (`omitempty`) — `"agent"` by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_type: Option<String>,
    /// Go `UserAgent` (`omitempty`) — `"antigravity"` by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Go `RequestID` (`omitempty`) — `agent-<uuid>` by default. Generated
    /// by the caller (kept pure here; see [`new_antigravity_envelope`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// Mirrors Go `NewAntigravityEnvelope` (envelope.go:29-38) minus the uuid
/// generation, which is left to the caller to keep this function pure and
/// deterministic for tests. The caller should pass `"agent-<uuid>"` as
/// `request_id` to match the Go default.
pub fn new_antigravity_envelope(
    project: impl Into<String>,
    model: impl Into<String>,
    request: serde_json::Value,
    request_id: impl Into<String>,
) -> AntigravityEnvelope {
    AntigravityEnvelope {
        project: project.into(),
        model: model.into(),
        request,
        request_type: Some("agent".to_string()),
        user_agent: Some("antigravity".to_string()),
        request_id: Some(request_id.into()),
    }
}

// ===========================================================================
// transformer.go — pure helpers (parseCredentials, buildURL, headers)
// ===========================================================================

/// Headers stripped from the inbound request before delegating to the
/// wrapped Gemini transformer (transformer.go:148-159).
pub const STRIPPED_INBOUND_HEADERS: [&str; 11] = [
    "User-Agent",
    "Authorization",
    "Content-Type",
    "Accept",
    "Content-Length",
    "Host",
    "Connection",
    "Pragma",
    "Cache-Control",
    "Client-Metadata",
    "X-Goog-Api-Client",
];

/// Additional headers set on every Antigravity request (transformer.go:198-
/// 204).
pub const X_OPENCODE_TOOLS_DEBUG_HEADER: (&str, &str) = ("X-Opencode-Tools-Debug", "1");

/// Mirrors Go `parseCredentials` (transformer.go:124-131). Antigravity
/// stores credentials as `<refreshToken>|<projectID>`; if there's no `|`
/// separator the whole string is the refresh token and the project is
/// empty.
pub fn parse_credentials(creds: &str) -> (String, String) {
    match creds.split_once('|') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (creds.to_string(), String::new()),
    }
}

/// Mirrors Go `Transformer.buildURL` (transformer.go:407-414). The path is
/// always `/v1internal:<action>` where `<action>` is `generateContent` or
/// `streamGenerateContent?alt=sse` for streaming.
pub fn build_url(base_url: &str, stream: bool) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let action = if stream {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    };
    format!("{trimmed}/v1internal:{action}")
}

// ===========================================================================
// token_provider.go — AntigravityExchangeStrategy request builders
// ===========================================================================

/// Mirrors Go `DefaultTokenURLs` (token_provider.go:28-31).
pub fn default_token_urls() -> (String, String) {
    (AUTHORIZE_URL.to_string(), TOKEN_URL.to_string())
}

/// Build the `application/x-www-form-urlencoded` body for Antigravity's
/// Google OAuth authorization-code exchange. Mirrors Go
/// `AntigravityExchangeStrategy.BuildExchangeRequest` (token_provider.go:
/// 40-63) minus the HTTP wrapper; we return the `(body, headers)` pair so
/// the caller can plug them into an `HttpRequest`.
///
/// Go uses `url.Values.Encode()` which sorts keys alphabetically — we
/// build the same key order deterministically via [`form_encode_pairs`].
pub fn build_exchange_form_body(
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> String {
    form_encode_pairs(&[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("client_secret", CLIENT_SECRET),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ])
}

/// Build the `application/x-www-form-urlencoded` body for Antigravity's
/// Google OAuth refresh-token request. Mirrors Go
/// `AntigravityExchangeStrategy.BuildRefreshRequest` (token_provider.go:
/// 66-95). Returns `Err` when `refresh_token` is empty to mirror the Go
/// guard at token_provider.go:71-73.
pub fn build_refresh_form_body(client_id: &str, refresh_token: &str) -> TransformerResult<String> {
    if refresh_token.is_empty() {
        return Err(ConduitError::invalid_request("refresh_token is empty"));
    }
    Ok(form_encode_pairs(&[
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("client_secret", CLIENT_SECRET),
        ("refresh_token", refresh_token),
    ]))
}

/// Encode a slice of `(key, value)` pairs as
/// `application/x-www-form-urlencoded`, matching Go's `url.Values.Encode()`
/// semantics: percent-encode every byte that isn't ASCII alphanumeric or
/// `-_.~`, encode space as `+`, separate pairs with `&`, join key and
/// value with `=`. **Pairs are sorted alphabetically by key** to mirror
/// Go's `url.Values.Encode()` (which always emits sorted output regardless
/// of insertion order).
fn form_encode_pairs(pairs: &[(&str, &str)]) -> String {
    let mut sorted: Vec<(&str, &str)> = pairs.iter().copied().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = String::new();
    for (i, (k, v)) in sorted.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        percent_encode_form(k, &mut out);
        out.push('=');
        percent_encode_form(v, &mut out);
    }
    out
}

/// `application/x-www-form-urlencoded` percent-encoder matching Go
/// `url.QueryEscape`. Space becomes `+`; unreserved (`A-Z a-z 0-9 - _ . ~`)
/// passes through; everything else is `%XX` upper-case hex.
fn percent_encode_form(input: &str, out: &mut String) {
    for &b in input.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                let hex = format!("{b:02X}");
                out.push_str(&hex);
            }
        }
    }
}

/// Mirrors Go `AntigravityExchangeStrategy.BuildExchangeRequest`
/// (token_provider.go:40-63) — assembles a full `HttpRequest` against
/// [`TOKEN_URL`] using [`build_exchange_form_body`]. `user_agent`, when
/// empty, is replaced by [`get_user_agent`] to mirror Go's `TokenProvider`
/// defaulting (token_provider.go:14-16).
pub fn build_exchange_request(
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    user_agent: Option<&str>,
) -> TransformerResult<HttpRequest> {
    let body = build_exchange_form_body(client_id, code, redirect_uri, code_verifier);
    Ok(HttpRequest {
        method: "POST".to_string(),
        url: Some(TOKEN_URL.to_string()),
        headers: token_form_headers(user_agent),
        body: Some(body.into_bytes()),
        ..HttpRequest::default()
    })
}

/// Mirrors Go `AntigravityExchangeStrategy.BuildRefreshRequest`
/// (token_provider.go:66-95).
pub fn build_refresh_request(
    client_id: &str,
    refresh_token: &str,
    user_agent: Option<&str>,
) -> TransformerResult<HttpRequest> {
    let body = build_refresh_form_body(client_id, refresh_token)?;
    Ok(HttpRequest {
        method: "POST".to_string(),
        url: Some(TOKEN_URL.to_string()),
        headers: token_form_headers(user_agent),
        body: Some(body.into_bytes()),
        ..HttpRequest::default()
    })
}

/// Headers shared by both Antigravity form-encoded token requests
/// (token_provider.go:49-55 / 81-87).
fn token_form_headers(user_agent: Option<&str>) -> conduit_llm::model::HeaderMap {
    let mut headers = conduit_llm::model::HeaderMap::new();
    headers.insert(
        "Content-Type".to_string(),
        "application/x-www-form-urlencoded".to_string(),
    );
    headers.insert("Accept".to_string(), "application/json".to_string());
    let ua = match user_agent {
        Some(u) if !u.is_empty() => u.to_string(),
        _ => get_user_agent(),
    };
    headers.insert("User-Agent".to_string(), ua);
    headers
}

// ===========================================================================
// router.go — quota preference, endpoint ordering, model normalization
// ===========================================================================

/// Go `QuotaPreference` (router.go:13-16) — which quota pool to bill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaPreference {
    /// Go `QuotaAntigravity`.
    Antigravity,
    /// Go `QuotaGeminiCLI`.
    GeminiCli,
}

/// Suffix regex from router.go:19 — matches `:(antigravity|gemini-cli)` at
/// the end of the model name (case-insensitive). The pattern is a static
/// literal so construction always succeeds; the `panic!` arm is defence-in-
/// depth that is provably unreachable.
fn suffix_regex() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(
        || match regex::Regex::new(r"(?i):(antigravity|gemini-cli)$") {
            Ok(re) => re,
            Err(e) => panic!("static suffix regex literal failed to compile: {e}"),
        },
    )
}

/// Prefix regex from router.go:22 — matches `^antigravity-` (case-
/// insensitive).
fn prefix_regex() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| match regex::Regex::new(r"(?i)^antigravity-(.+)") {
        Ok(re) => re,
        Err(e) => panic!("static prefix regex literal failed to compile: {e}"),
    })
}

/// Mirrors Go `DetermineQuotaPreference` (router.go:30-77). Priority:
///   1. Explicit `:antigravity` / `:gemini-cli` suffix.
///   2. Explicit `antigravity-` prefix.
///   3. Model-specific rules (claude*/gpt*/image/legacy gemini-3).
///   4. Default `GeminiCli` for standard Gemini models.
pub fn determine_quota_preference(model_name: &str) -> QuotaPreference {
    let lower = model_name.to_ascii_lowercase();

    // Step 1: explicit suffix.
    if let Some(caps) = suffix_regex().captures(&lower) {
        if &caps[1] == "antigravity" {
            return QuotaPreference::Antigravity;
        }
        return QuotaPreference::GeminiCli;
    }

    // Step 2: explicit prefix.
    if prefix_regex().is_match(&lower) {
        return QuotaPreference::Antigravity;
    }

    // Step 3a: Claude and GPT-OSS only exist on Antigravity.
    if lower.starts_with("claude") || lower.starts_with("gpt") {
        return QuotaPreference::Antigravity;
    }

    // Step 3b: image generation models require Antigravity.
    if lower.contains("image") || lower.contains("imagen") {
        return QuotaPreference::Antigravity;
    }

    // Step 3c: legacy Gemini 3 names (router.go:61-72).
    const LEGACY_GEMINI3: &[&str] = &[
        "gemini-3-pro-low",
        "gemini-3-pro-high",
        "gemini-3-pro-medium",
        "gemini-3-flash",
        "gemini-3-flash-low",
        "gemini-3-flash-medium",
        "gemini-3-flash-high",
    ];
    if LEGACY_GEMINI3.contains(&lower.as_str()) {
        return QuotaPreference::Antigravity;
    }

    // Step 4: default.
    QuotaPreference::GeminiCli
}

/// Mirrors Go `GetInitialEndpoint` (router.go:82-86) — always Daily.
pub fn get_initial_endpoint(_preference: QuotaPreference) -> &'static str {
    ENDPOINT_DAILY
}

/// Mirrors Go `GetFallbackEndpoints` (router.go:90-96) — Daily, Autopush,
/// Prod.
pub fn get_fallback_endpoints() -> [&'static str; 3] {
    [ENDPOINT_DAILY, ENDPOINT_AUTOPUSH, ENDPOINT_PROD]
}

/// Mirrors Go `StripModelSuffix` (router.go:100-102).
pub fn strip_model_suffix(model_name: &str) -> String {
    suffix_regex().replace_all(model_name, "").to_string()
}

/// Mirrors Go `StripAntigravityPrefix` (router.go:106-112). Case-sensitive
/// in Go (only lowercase `antigravity-` is stripped) — the prefix regex
/// here is case-sensitive too, matching the Go test at router_test.go:
/// 243-247 that asserts `Antigravity-...` is *not* stripped.
pub fn strip_antigravity_prefix(model_name: &str) -> String {
    if let Some(rest) = model_name.strip_prefix("antigravity-") {
        rest.to_string()
    } else {
        model_name.to_string()
    }
}

/// Mirrors Go `NormalizeModelName` (router.go:115-121).
pub fn normalize_model_name(model_name: &str) -> String {
    let stripped = strip_model_suffix(model_name);
    strip_antigravity_prefix(&stripped)
}

/// Mirrors Go `ShouldRetryWithDifferentEndpoint` (router.go:125-132).
pub fn should_retry_with_different_endpoint(status_code: u16) -> bool {
    matches!(status_code, 429 | 403 | 404) || (500..600).contains(&status_code)
}

/// Mirrors unexported `transformModelForAntigravity` (router.go:137-156).
/// `gemini-3-pro*` without a `-low|-medium|-high` suffix is rewritten to
/// append `-low`. All other models are returned normalized (suffix/prefix
/// stripped).
pub fn transform_model_for_antigravity(model_name: &str) -> String {
    let normalized = normalize_model_name(model_name);
    let lower = normalized.to_ascii_lowercase();

    if lower.starts_with("gemini-3-pro") {
        let has_tier =
            lower.ends_with("-low") || lower.ends_with("-medium") || lower.ends_with("-high");
        if !has_tier {
            return format!("{normalized}-low");
        }
    }
    normalized
}

/// Mirrors `replaceBaseURL` (executor.go:376-415) — preserves the
/// `/v1internal:*` path + query + fragment while swapping the scheme/host
/// for `new_base`. Returns the original URL unchanged when the path doesn't
/// start with `/v1internal` (Go logs a warning in that case).
pub fn replace_base_url(original_url: &str, new_base: &str) -> String {
    let parsed = match url::Url::parse(original_url) {
        Ok(u) => u,
        Err(_) => return original_url.to_string(),
    };
    let path = parsed.path();
    if path.is_empty() || !path.starts_with("/v1internal") {
        return original_url.to_string();
    }
    let mut base = match url::Url::parse(new_base) {
        Ok(u) => u,
        Err(_) => return original_url.to_string(),
    };
    base.set_path(path);
    base.set_query(parsed.query());
    base.set_fragment(parsed.fragment());
    base.to_string()
}

// ===========================================================================
// sanitizer.go — JSON-schema sanitizer for Antigravity API compatibility.
//
// Phase 1: convert refs/const/enums/additionalProperties/constraints to
//          description hints. (sanitizer.go:78-274)
// Phase 2: flatten allOf / anyOf / oneOf / type-arrays. (sanitizer.go:276-563)
// Phase 3: drop unsupported keywords + cleanup required. (sanitizer.go:565-642)
// Phase 4: add `_placeholder` for empty object schemas. (sanitizer.go:646-673)
// ===========================================================================

/// Go `emptySchemaPlaceholderName` (sanitizer.go:71).
pub const EMPTY_SCHEMA_PLACEHOLDER_NAME: &str = "_placeholder";
/// Go `emptySchemaPlaceholderDescription` (sanitizer.go:74).
pub const EMPTY_SCHEMA_PLACEHOLDER_DESCRIPTION: &str = "Placeholder. Always pass true.";

/// Go `unsupportedConstraints` (sanitizer.go:53-57).
pub const UNSUPPORTED_CONSTRAINTS: &[&str] = &[
    "minLength",
    "maxLength",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "pattern",
    "minItems",
    "maxItems",
    "format",
    "default",
    "examples",
];

/// Go `unsupportedKeywords` (sanitizer.go:60-68).
pub const UNSUPPORTED_KEYWORDS: &[&str] = &[
    "minLength",
    "maxLength",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "pattern",
    "minItems",
    "maxItems",
    "format",
    "default",
    "examples",
    "$schema",
    "$defs",
    "definitions",
    "const",
    "$ref",
    "additionalProperties",
    "propertyNames",
    "title",
    "$id",
    "$comment",
];

/// Convert all `"type"` field values to UPPERCASE. Mirrors Go
/// `UppercaseSchemaTypes` (sanitizer.go:15-50). Recurses through nested
/// objects and arrays (`anyOf`, `oneOf`, `allOf`, `items`, etc.).
pub fn uppercase_schema_types(schema: serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(mut map) => {
            if let Some(t) = map.get("type").and_then(|v| v.as_str()) {
                map.insert(
                    "type".to_string(),
                    serde_json::Value::String(t.to_uppercase()),
                );
            }
            // Recurse over every value, walking arrays element-by-element.
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(v) = map.remove(&key) {
                    map.insert(key, uppercase_schema_types(v));
                }
            }
            serde_json::Value::Object(map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(uppercase_schema_types).collect())
        }
        other => other,
    }
}

/// Go `SanitizeJSONSchema` (sanitizer.go:78-105) — runs the full
/// four-phase pipeline. The input is cloned (deep copy) so the caller's
/// schema is not mutated.
pub fn sanitize_json_schema(schema: serde_json::Value) -> serde_json::Value {
    if schema.is_null() {
        return schema;
    }
    let mut result = schema.clone();
    // Phase 1
    result = convert_refs_to_hints(result);
    result = convert_const_to_enum(result);
    result = add_enum_hints(result);
    result = add_additional_properties_hints(result);
    result = move_constraints_to_description(result);
    // Phase 2
    result = merge_all_of(result);
    result = flatten_any_of_one_of(result);
    result = flatten_type_arrays(result, String::new());
    // Phase 3
    result = remove_unsupported_keywords(result, false);
    result = cleanup_required_fields(result);
    // Phase 4
    result = add_empty_schema_placeholder(result);
    result
}

/// Append a hint to `schema.description` (sanitizer.go:122-135). Existing
/// descriptions become `"<existing> (<hint>)"`.
fn append_description_hint(map: &mut serde_json::Map<String, serde_json::Value>, hint: &str) {
    let new_desc = match map.get("description").and_then(|v| v.as_str()) {
        Some(existing) if !existing.is_empty() => {
            format!("{existing} ({hint})")
        }
        _ => hint.to_string(),
    };
    map.insert(
        "description".to_string(),
        serde_json::Value::String(new_desc),
    );
}

/// Go `convertRefsToHints` (sanitizer.go:137-170).
fn convert_refs_to_hints(schema: serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(mut map) => {
            // Top-level `$ref` short-circuits to a fresh object schema.
            if let Some(ref_val) = map.get("$ref").and_then(|v| v.as_str()).map(str::to_string) {
                let def_name = ref_val.rsplit('/').next().unwrap_or("").to_string();
                let hint = format!("See: {def_name}");
                let mut new_map = serde_json::Map::new();
                new_map.insert(
                    "type".to_string(),
                    serde_json::Value::String("object".to_string()),
                );
                match map.get("description").and_then(|v| v.as_str()) {
                    Some(desc) => {
                        new_map.insert(
                            "description".to_string(),
                            serde_json::Value::String(format!("{desc} ({hint})")),
                        );
                    }
                    None => {
                        new_map.insert("description".to_string(), serde_json::Value::String(hint));
                    }
                }
                return serde_json::Value::Object(new_map);
            }
            // Otherwise recurse over each value, walking arrays.
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(v) = map.remove(&key) {
                    map.insert(key, convert_refs_to_hints(v));
                }
            }
            serde_json::Value::Object(map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(convert_refs_to_hints).collect())
        }
        other => other,
    }
}

/// Go `convertConstToEnum` (sanitizer.go:172-196). When a schema has `const`
/// but no `enum`, set `enum = [const]`. `const` itself is removed later by
/// [`remove_unsupported_keywords`].
fn convert_const_to_enum(schema: serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(mut map) => {
            if let Some(const_val) = map.get("const").cloned() {
                if !map.contains_key("enum") {
                    map.insert(
                        "enum".to_string(),
                        serde_json::Value::Array(vec![const_val]),
                    );
                }
            }
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if key == "const" {
                    continue;
                }
                if let Some(v) = map.remove(&key) {
                    map.insert(key, convert_const_to_enum(v));
                }
            }
            serde_json::Value::Object(map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(convert_const_to_enum).collect())
        }
        other => other,
    }
}

/// Go `addEnumHints` (sanitizer.go:198-224). When `enum` has 2-10 values,
/// append an `"Allowed: a, b, c"` description hint.
fn add_enum_hints(schema: serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(mut map) => {
            let hint_opt = map
                .get("enum")
                .and_then(|v| v.as_array())
                .filter(|arr| arr.len() > 1 && arr.len() <= 10)
                .map(|arr| {
                    let strs: Vec<String> = arr.iter().map(|v| v_to_string(v)).collect();
                    format!("Allowed: {}", strs.join(", "))
                });
            if let Some(hint) = hint_opt {
                append_description_hint(&mut map, &hint);
            }
            // Recurse over everything except `enum` itself.
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if key == "enum" {
                    continue;
                }
                if let Some(v) = map.remove(&key) {
                    map.insert(key, add_enum_hints(v));
                }
            }
            serde_json::Value::Object(map)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(add_enum_hints).collect())
        }
        other => other,
    }
}

/// Render a `serde_json::Value` as Go's `fmt.Sprintf("%v", v)` does for the
/// hint helpers. Numbers keep their JSON type; everything else uses the
/// str-rendered JSON form.
fn v_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Go `addAdditionalPropertiesHints` (sanitizer.go:226-246). When
/// `additionalProperties` is exactly `false`, append
/// `"No extra properties allowed"`.
fn add_additional_properties_hints(schema: serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(mut map) => {
            let is_false = map
                .get("additionalProperties")
                .and_then(|v| v.as_bool())
                .map(|b| !b)
                .unwrap_or(false);
            if is_false {
                append_description_hint(&mut map, "No extra properties allowed");
            }
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if key == "additionalProperties" {
                    continue;
                }
                if let Some(v) = map.remove(&key) {
                    map.insert(key, add_additional_properties_hints(v));
                }
            }
            serde_json::Value::Object(map)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.into_iter()
                .map(add_additional_properties_hints)
                .collect(),
        ),
        other => other,
    }
}

/// Go `moveConstraintsToDescription` (sanitizer.go:248-274). For each
/// unsupported constraint (minLength, maxLength, ...) present at this level,
/// append `"<key>: <val>"` to the description.
fn move_constraints_to_description(schema: serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(mut map) => {
            for c in UNSUPPORTED_CONSTRAINTS {
                if let Some(val) = map.get(*c).cloned() {
                    // Skip non-primitive values (objects/arrays).
                    let primitive = matches!(
                        val,
                        serde_json::Value::Number(_)
                            | serde_json::Value::String(_)
                            | serde_json::Value::Bool(_)
                            | serde_json::Value::Null
                    );
                    if primitive {
                        let hint = format!("{c}: {}", v_to_string(&val));
                        append_description_hint(&mut map, &hint);
                    }
                }
            }
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if let Some(v) = map.remove(&key) {
                    map.insert(key, move_constraints_to_description(v));
                }
            }
            serde_json::Value::Object(map)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.into_iter()
                .map(move_constraints_to_description)
                .collect(),
        ),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Phase 2 helpers — flatten complex structures.
// ---------------------------------------------------------------------------

/// Mirrors `mergeAllOf` (sanitizer.go:278-372). Merges `allOf` siblings by
/// union-ing their `properties`, merging `required`, and copying any other
/// scalar fields onto the parent. `allOf` itself is removed.
fn merge_all_of(schema: serde_json::Value) -> serde_json::Value {
    let mut map = match schema {
        serde_json::Value::Object(m) => m,
        serde_json::Value::Array(arr) => {
            return serde_json::Value::Array(arr.into_iter().map(merge_all_of).collect());
        }
        other => return other,
    };

    if let Some(all_of_val) = map.remove("allOf") {
        if let Some(all_of) = all_of_val.as_array() {
            let mut merged_props: serde_json::Map<String, serde_json::Value> =
                serde_json::Map::new();
            let mut merged_required: Vec<String> = Vec::new();
            let mut merged_other: serde_json::Map<String, serde_json::Value> =
                serde_json::Map::new();

            for item in all_of {
                if let Some(sub) = item.as_object() {
                    // Merge properties.
                    if let Some(props) = sub.get("properties").and_then(|v| v.as_object()) {
                        for (k, v) in props {
                            merged_props.insert(k.clone(), v.clone());
                        }
                    }
                    // Merge required (deduped).
                    if let Some(req) = sub.get("required").and_then(|v| v.as_array()) {
                        for r in req {
                            if let Some(s) = r.as_str() {
                                if !merged_required.contains(&s.to_string()) {
                                    merged_required.push(s.to_string());
                                }
                            }
                        }
                    }
                    // Copy other fields.
                    for (k, v) in sub {
                        if k != "properties" && k != "required" && !merged_other.contains_key(k) {
                            merged_other.insert(k.clone(), v.clone());
                        }
                    }
                }
            }

            // Apply merged properties onto parent.
            if !merged_props.is_empty() {
                match map.get_mut("properties").and_then(|v| v.as_object_mut()) {
                    Some(existing) => {
                        for (k, v) in merged_props {
                            existing.insert(k, v);
                        }
                    }
                    None => {
                        map.insert(
                            "properties".to_string(),
                            serde_json::Value::Object(merged_props),
                        );
                    }
                }
            }

            // Apply merged required onto parent (deduped).
            if !merged_required.is_empty() {
                let mut existing: Vec<String> = Vec::new();
                if let Some(req) = map.get("required").and_then(|v| v.as_array()) {
                    for r in req {
                        if let Some(s) = r.as_str() {
                            existing.push(s.to_string());
                        }
                    }
                }
                for r in merged_required {
                    if !existing.contains(&r) {
                        existing.push(r);
                    }
                }
                let arr: Vec<serde_json::Value> = existing
                    .into_iter()
                    .map(serde_json::Value::String)
                    .collect();
                map.insert("required".to_string(), serde_json::Value::Array(arr));
            }

            // Apply other merged scalars.
            for (k, v) in merged_other {
                if k != "properties" && k != "required" && !map.contains_key(&k) {
                    map.insert(k, v);
                }
            }
        }
    }

    // Recurse.
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        if let Some(v) = map.remove(&key) {
            map.insert(key, merge_all_of(v));
        }
    }
    serde_json::Value::Object(map)
}

/// Mirrors `flattenAnyOfOneOf` (sanitizer.go:374-460). For each `anyOf` /
/// `oneOf`, first try to merge a const/enum union into a single string
/// enum. Otherwise pick the highest-scoring option and merge it into the
/// parent, dropping the union.
fn flatten_any_of_one_of(schema: serde_json::Value) -> serde_json::Value {
    let mut map = match schema {
        serde_json::Value::Object(m) => m,
        serde_json::Value::Array(arr) => {
            return serde_json::Value::Array(arr.into_iter().map(flatten_any_of_one_of).collect());
        }
        other => return other,
    };

    for union_key in ["anyOf", "oneOf"] {
        let union_val = match map.remove(union_key) {
            Some(v) => v,
            None => continue,
        };
        let options = match union_val.as_array() {
            Some(a) => a.clone(),
            None => {
                map.insert(union_key.to_string(), union_val);
                continue;
            }
        };
        if options.is_empty() {
            continue;
        }
        let parent_desc = map
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Enum-merge short-circuit.
        if let Some(enum_values) = try_merge_enum_from_union(&options) {
            map.insert(
                "type".to_string(),
                serde_json::Value::String("string".to_string()),
            );
            map.insert("enum".to_string(), serde_json::Value::Array(enum_values));
            if !parent_desc.is_empty() {
                map.insert(
                    "description".to_string(),
                    serde_json::Value::String(parent_desc),
                );
            }
            continue;
        }

        // Pick the highest-scoring option.
        let mut best_idx = 0usize;
        let mut best_score = -1i32;
        let mut all_types: Vec<String> = Vec::new();
        for (i, opt) in options.iter().enumerate() {
            if let Some(opt_map) = opt.as_object() {
                let (score, type_name) = score_schema_option(opt_map);
                if !type_name.is_empty() && type_name != "unknown" {
                    all_types.push(type_name);
                }
                if score > best_score {
                    best_score = score;
                    best_idx = i;
                }
            }
        }

        if best_idx < options.len() {
            if let Some(mut selected) = options[best_idx].as_object().cloned() {
                selected = match flatten_any_of_one_of(serde_json::Value::Object(selected)) {
                    serde_json::Value::Object(m) => m,
                    other => {
                        let mut m = serde_json::Map::new();
                        m.insert("value".to_string(), other);
                        m
                    }
                };
                // Description merge.
                let child_desc = selected
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !parent_desc.is_empty() {
                    if !child_desc.is_empty() && child_desc != parent_desc {
                        selected.insert(
                            "description".to_string(),
                            serde_json::Value::String(format!("{parent_desc} ({child_desc})")),
                        );
                    } else {
                        selected.insert(
                            "description".to_string(),
                            serde_json::Value::String(parent_desc.clone()),
                        );
                    }
                }

                // Type hint when multiple distinct types were present.
                let mut unique: Vec<String> = Vec::new();
                for t in &all_types {
                    if !unique.contains(t) {
                        unique.push(t.clone());
                    }
                }
                if unique.len() > 1 {
                    let hint = format!("Accepts: {}", unique.join(" | "));
                    append_description_hint(&mut selected, &hint);
                }

                // Copy selected onto parent.
                for (k, v) in selected {
                    map.insert(k, v);
                }
            }
        }
    }

    // Recurse over remaining values.
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        if let Some(v) = map.remove(&key) {
            map.insert(key, flatten_any_of_one_of(v));
        }
    }
    serde_json::Value::Object(map)
}

/// Mirrors `scoreSchemaOption` (sanitizer.go:690-714). Returns `(score,
/// type_name)` used to pick the most informative option from an
/// `anyOf`/`oneOf`.
fn score_schema_option(schema: &serde_json::Map<String, serde_json::Value>) -> (i32, String) {
    let type_name = schema
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if type_name == "object" || schema.contains_key("properties") {
        return (3, "object".to_string());
    }
    if type_name == "array" || schema.contains_key("items") {
        return (2, "array".to_string());
    }
    if !type_name.is_empty() && type_name != "null" {
        return (1, type_name);
    }
    if type_name.is_empty() {
        return (0, "null".to_string());
    }
    (0, type_name)
}

/// Mirrors `tryMergeEnumFromUnion` (sanitizer.go:716-758). Returns
/// `Some(Vec<values>)` when every option is a single const/enum (so the
/// union can collapse to a string enum), otherwise `None`.
fn try_merge_enum_from_union(options: &[serde_json::Value]) -> Option<Vec<serde_json::Value>> {
    let mut enum_values: Vec<serde_json::Value> = Vec::new();
    for opt in options {
        let opt_map = opt.as_object()?;
        if let Some(const_val) = opt_map.get("const").cloned() {
            enum_values.push(const_val);
            continue;
        }
        if let Some(enums) = opt_map.get("enum").and_then(|v| v.as_array()) {
            if enums.len() == 1 {
                enum_values.push(enums[0].clone());
                continue;
            }
            if !enums.is_empty() {
                enum_values.extend(enums.iter().cloned());
                continue;
            }
        }
        // Complex structures can't merge.
        if opt_map.contains_key("properties")
            || opt_map.contains_key("items")
            || opt_map.contains_key("anyOf")
            || opt_map.contains_key("oneOf")
            || opt_map.contains_key("allOf")
        {
            return None;
        }
        // Has type but no const/enum → can't merge.
        if opt_map.contains_key("type")
            && !opt_map.contains_key("const")
            && !opt_map.contains_key("enum")
        {
            return None;
        }
    }
    if enum_values.is_empty() {
        None
    } else {
        Some(enum_values)
    }
}

/// Mirrors `flattenTypeArrays` (sanitizer.go:462-563). Collapses
/// `"type": ["string", "null"]` to a single type with description hints for
/// nullable/multi-type. The nullable-tracking side effect on the parent
/// `required` array is approximated here by also dropping nullable keys
/// from the root `required` (sanitizer.go:526-545).
fn flatten_type_arrays(schema: serde_json::Value, current_path: String) -> serde_json::Value {
    let mut map = match schema {
        serde_json::Value::Object(m) => m,
        serde_json::Value::Array(arr) => {
            return serde_json::Value::Array(
                arr.into_iter()
                    .map(|v| flatten_type_arrays(v, current_path.clone()))
                    .collect(),
            );
        }
        other => return other,
    };

    let mut nullable_at_root: Vec<String> = Vec::new();

    if let Some(types_val) = map.get("type").cloned() {
        if let Some(types) = types_val.as_array() {
            let mut non_null: Vec<String> = Vec::new();
            let mut has_null = false;
            for t in types {
                if let Some(s) = t.as_str() {
                    if s == "null" {
                        has_null = true;
                    } else {
                        non_null.push(s.to_string());
                    }
                }
            }
            let first_type = if !non_null.is_empty() {
                non_null[0].clone()
            } else if has_null {
                "null".to_string()
            } else {
                "string".to_string()
            };
            map.insert("type".to_string(), serde_json::Value::String(first_type));
            if non_null.len() > 1 {
                let hint = format!("Accepts: {}", non_null.join(" | "));
                append_description_hint(&mut map, &hint);
            }
            if has_null {
                append_description_hint(&mut map, "nullable");
            }
        }
    }

    // Recurse into properties tracking nullable descriptors at root.
    if let Some(props_val) = map.remove("properties") {
        if let Some(props) = props_val.as_object() {
            let mut new_props = serde_json::Map::new();
            for (key, val) in props {
                let prop_path = if current_path.is_empty() {
                    format!("properties.{key}")
                } else {
                    format!("{current_path}.properties.{key}")
                };
                let processed = flatten_type_arrays(val.clone(), prop_path);
                if let Some(desc) = processed
                    .as_object()
                    .and_then(|m| m.get("description"))
                    .and_then(|v| v.as_str())
                {
                    if desc.contains("nullable") {
                        nullable_at_root.push(key.clone());
                    }
                }
                new_props.insert(key.to_string(), processed);
            }
            map.insert(
                "properties".to_string(),
                serde_json::Value::Object(new_props),
            );
        } else {
            map.insert("properties".to_string(), props_val);
        }
    }

    // At root, drop nullable entries from `required` (sanitizer.go:526-545).
    if current_path.is_empty() && !nullable_at_root.is_empty() {
        if let Some(req_val) = map.get_mut("required").and_then(|v| v.as_array_mut()) {
            req_val.retain(|r| match r.as_str() {
                Some(s) => !nullable_at_root.contains(&s.to_string()),
                None => true,
            });
        }
        if map
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(false)
        {
            map.remove("required");
        }
    }

    // Recurse over remaining fields.
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        if key == "properties" {
            continue;
        }
        if let Some(v) = map.remove(&key) {
            let child_path = if current_path.is_empty() {
                key.clone()
            } else {
                format!("{current_path}.{key}")
            };
            map.insert(key, flatten_type_arrays(v, child_path));
        }
    }
    serde_json::Value::Object(map)
}

// ---------------------------------------------------------------------------
// Phase 3 helpers — drop unsupported keywords + cleanup required.
// ---------------------------------------------------------------------------

/// Mirrors `removeUnsupportedKeywords` (sanitizer.go:567-606). The Go
/// `insideProperties` argument controls whether top-level scalar keywords
/// are dropped; the recursion always calls with `false`, matching Go.
fn remove_unsupported_keywords(
    schema: serde_json::Value,
    inside_properties: bool,
) -> serde_json::Value {
    let mut map = match schema {
        serde_json::Value::Object(m) => m,
        serde_json::Value::Array(arr) => {
            return serde_json::Value::Array(
                arr.into_iter()
                    .map(|v| remove_unsupported_keywords(v, false))
                    .collect(),
            );
        }
        other => return other,
    };

    // Drop unsupported keywords at this level (when not inside properties).
    if !inside_properties {
        for kw in UNSUPPORTED_KEYWORDS {
            map.remove(*kw);
        }
    }

    // Recurse over remaining keys.
    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        let v = map.remove(&key).unwrap_or(serde_json::Value::Null);
        if key == "properties" {
            // Process each property value (treated as a fresh schema).
            if let Some(props) = v.as_object() {
                let mut new_props = serde_json::Map::new();
                for (k, v) in props {
                    new_props.insert(k.clone(), remove_unsupported_keywords(v.clone(), false));
                }
                map.insert(key, serde_json::Value::Object(new_props));
            } else {
                map.insert(key, v);
            }
        } else if let Some(obj) = v.as_object() {
            // Inline recursion for nested objects.
            let processed =
                remove_unsupported_keywords(serde_json::Value::Object(obj.clone()), false);
            map.insert(key, processed);
        } else if let Some(arr) = v.as_array() {
            let new_arr: Vec<serde_json::Value> = arr
                .iter()
                .cloned()
                .map(|v| remove_unsupported_keywords(v, false))
                .collect();
            map.insert(key, serde_json::Value::Array(new_arr));
        } else {
            map.insert(key, v);
        }
    }
    serde_json::Value::Object(map)
}

/// Mirrors `cleanupRequiredFields` (sanitizer.go:608-642). Drops entries
/// from `required` that don't exist in `properties`. Recurses into
/// properties + array elements.
fn cleanup_required_fields(schema: serde_json::Value) -> serde_json::Value {
    let mut map = match schema {
        serde_json::Value::Object(m) => m,
        serde_json::Value::Array(arr) => {
            return serde_json::Value::Array(
                arr.into_iter().map(cleanup_required_fields).collect(),
            );
        }
        other => return other,
    };

    if let Some(req) = map.get("required").and_then(|v| v.as_array()).cloned() {
        if let Some(props) = map.get("properties").and_then(|v| v.as_object()) {
            let mut valid: Vec<serde_json::Value> = Vec::new();
            for r in req {
                if let Some(s) = r.as_str() {
                    if props.contains_key(s) {
                        valid.push(serde_json::Value::String(s.to_string()));
                    }
                }
            }
            if valid.is_empty() {
                map.remove("required");
            } else {
                map.insert("required".to_string(), serde_json::Value::Array(valid));
            }
        }
    }

    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        if let Some(v) = map.remove(&key) {
            map.insert(key, cleanup_required_fields(v));
        }
    }
    serde_json::Value::Object(map)
}

// ---------------------------------------------------------------------------
// Phase 4 helper — empty object placeholder.
// ---------------------------------------------------------------------------

/// Mirrors `addEmptySchemaPlaceholder` (sanitizer.go:646-673). For every
/// `"type": "object"` schema with no/empty `properties`, injects a
/// `_placeholder` boolean field + required entry.
fn add_empty_schema_placeholder(schema: serde_json::Value) -> serde_json::Value {
    let mut map = match schema {
        serde_json::Value::Object(m) => m,
        serde_json::Value::Array(arr) => {
            return serde_json::Value::Array(
                arr.into_iter().map(add_empty_schema_placeholder).collect(),
            );
        }
        other => return other,
    };

    let is_object = map
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s == "object")
        .unwrap_or(false);
    let props_empty = match map.get("properties").and_then(|v| v.as_object()) {
        Some(props) => props.is_empty(),
        None => true,
    };
    if is_object && props_empty {
        let mut placeholder = serde_json::Map::new();
        placeholder.insert(
            "type".to_string(),
            serde_json::Value::String("boolean".to_string()),
        );
        placeholder.insert(
            "description".to_string(),
            serde_json::Value::String(EMPTY_SCHEMA_PLACEHOLDER_DESCRIPTION.to_string()),
        );
        let mut props = serde_json::Map::new();
        props.insert(
            EMPTY_SCHEMA_PLACEHOLDER_NAME.to_string(),
            serde_json::Value::Object(placeholder),
        );
        map.insert("properties".to_string(), serde_json::Value::Object(props));
        map.insert(
            "required".to_string(),
            serde_json::Value::Array(vec![serde_json::Value::String(
                EMPTY_SCHEMA_PLACEHOLDER_NAME.to_string(),
            )]),
        );
    }

    let keys: Vec<String> = map.keys().cloned().collect();
    for key in keys {
        if let Some(v) = map.remove(&key) {
            map.insert(key, add_empty_schema_placeholder(v));
        }
    }
    serde_json::Value::Object(map)
}

// ===========================================================================
// health_tracker.go — per-(model, endpoint) cooldown tracker.
// ===========================================================================

/// Go `DefaultCooldownDuration` (health_tracker.go:11).
pub const DEFAULT_COOLDOWN_DURATION: Duration = Duration::from_secs(60);

/// Go `DefaultTTL` (health_tracker.go:14).
pub const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);

/// Go `AntigravityEndpointFailure` (health_tracker.go:33-39) — failure
/// record stored per (model, endpoint) key.
#[derive(Debug, Clone, PartialEq)]
pub struct AntigravityEndpointFailure {
    pub model: String,
    pub endpoint: String,
    pub last_failed_at: chrono::DateTime<chrono::Utc>,
    pub status_code: u16,
    pub cooldown_until: chrono::DateTime<chrono::Utc>,
}

/// Go `AntigravityHealthTrackerStats` (health_tracker.go:171-176).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AntigravityHealthTrackerStats {
    pub total_entries: usize,
    pub in_cooldown: usize,
    pub expired: usize,
    /// `model|endpoint` -> cooldown expiry time.
    pub cooldown_entries: std::collections::BTreeMap<String, chrono::DateTime<chrono::Utc>>,
}

/// Go `AntigravityHealthTracker` (health_tracker.go:25-30) — interior-
/// mutable cooldown tracker. Uses `std::sync::Mutex` to mirror Go's
/// `sync.RWMutex` (single mutex is fine here; the Go tests under
/// concurrent access don't need read/write parallelism).
#[derive(Debug)]
pub struct AntigravityHealthTracker {
    failures: Mutex<std::collections::BTreeMap<(String, String), AntigravityEndpointFailure>>,
    cooldown_duration: Duration,
    ttl: Duration,
}

impl Default for AntigravityHealthTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl AntigravityHealthTracker {
    /// Go `NewAntigravityHealthTracker` (health_tracker.go:42-48).
    pub fn new() -> Self {
        Self {
            failures: Mutex::new(std::collections::BTreeMap::new()),
            cooldown_duration: DEFAULT_COOLDOWN_DURATION,
            ttl: DEFAULT_TTL,
        }
    }

    /// Go `NewAntigravityHealthTrackerWithConfig` (health_tracker.go:51-57).
    pub fn new_with_config(cooldown: Duration, ttl: Duration) -> Self {
        Self {
            failures: Mutex::new(std::collections::BTreeMap::new()),
            cooldown_duration: cooldown,
            ttl,
        }
    }

    /// Go `ShouldSkip` (health_tracker.go:61-82). Performs lazy TTL
    /// cleanup of the requested key.
    pub fn should_skip(&self, model: &str, endpoint: &str) -> bool {
        let mut failures = match self.failures.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let key = (model.to_string(), endpoint.to_string());
        let now = chrono::Utc::now();
        let ttl = self.ttl;
        let failure = match failures.get(&key) {
            Some(f) => f,
            None => return false,
        };
        if now.signed_duration_since(failure.last_failed_at)
            > chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::MAX)
        {
            failures.remove(&key);
            return false;
        }
        now < failure.cooldown_until
    }

    /// Go `RecordFailure` (health_tracker.go:86-102).
    pub fn record_failure(&self, model: &str, endpoint: &str, status_code: u16) {
        let mut failures = match self.failures.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let now = chrono::Utc::now();
        let cooldown_until = now
            + chrono::Duration::from_std(self.cooldown_duration).unwrap_or(chrono::Duration::MAX);
        failures.insert(
            (model.to_string(), endpoint.to_string()),
            AntigravityEndpointFailure {
                model: model.to_string(),
                endpoint: endpoint.to_string(),
                last_failed_at: now,
                status_code,
                cooldown_until,
            },
        );
    }

    /// Go `RecordSuccess` (health_tracker.go:106-112).
    pub fn record_success(&self, model: &str, endpoint: &str) {
        let mut failures = match self.failures.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        failures.remove(&(model.to_string(), endpoint.to_string()));
    }

    /// Go `GetFailure` (health_tracker.go:116-136). Returns a clone of the
    /// failure record (or `None` when not present / expired).
    pub fn get_failure(&self, model: &str, endpoint: &str) -> Option<AntigravityEndpointFailure> {
        let failures = self.failures.lock().ok()?;
        let key = (model.to_string(), endpoint.to_string());
        let failure = failures.get(&key)?.clone();
        if chrono::Utc::now().signed_duration_since(failure.last_failed_at)
            > chrono::Duration::from_std(self.ttl).unwrap_or(chrono::Duration::MAX)
        {
            return None;
        }
        Some(failure)
    }

    /// Go `Stats` (health_tracker.go:139-168).
    pub fn stats(&self) -> AntigravityHealthTrackerStats {
        let failures = match self.failures.lock() {
            Ok(g) => g,
            Err(_) => return AntigravityHealthTrackerStats::default(),
        };
        let now = chrono::Utc::now();
        let ttl_dur = chrono::Duration::from_std(self.ttl).unwrap_or(chrono::Duration::MAX);
        let mut stats = AntigravityHealthTrackerStats {
            total_entries: failures.len(),
            ..AntigravityHealthTrackerStats::default()
        };
        for failure in failures.values() {
            if now.signed_duration_since(failure.last_failed_at) > ttl_dur {
                stats.expired += 1;
                continue;
            }
            if now < failure.cooldown_until {
                stats.in_cooldown += 1;
                let key_str = format!("{}|{}", failure.model, failure.endpoint);
                stats
                    .cooldown_entries
                    .insert(key_str, failure.cooldown_until);
            }
        }
        stats
    }

    /// Go `Clear` (health_tracker.go:180-185).
    pub fn clear(&self) {
        if let Ok(mut g) = self.failures.lock() {
            g.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // -----------------------------------------------------------------------
    // constants.go sanity
    // -----------------------------------------------------------------------

    #[test]
    fn default_models_match_go_registry() {
        // Mirrors Go `DefaultModels()` (constants.go:65-79) verbatim.
        assert_eq!(
            default_models(),
            vec![
                "claude-sonnet-4-5",
                "claude-sonnet-4-5-thinking",
                "claude-opus-4-5-thinking",
                "gemini-2.5-flash",
                "gemini-2.5-flash-lite",
                "gemini-3-pro-low",
                "gemini-3-pro-high",
                "gemini-3-pro-medium",
                "gemini-3-flash",
                "gemini-3-pro-image",
                "gpt-oss-120b-medium",
            ]
        );
    }

    #[test]
    fn scopes_string_is_space_joined() -> TestResult {
        assert_eq!(scopes_string(), SCOPES.join(" "));
        // Mirrors the literal in Go `ScopesString` (constants.go:54).
        assert_eq!(
            scopes_string(),
            "https://www.googleapis.com/auth/cloud-platform \
             https://www.googleapis.com/auth/userinfo.email \
             https://www.googleapis.com/auth/userinfo.profile \
             https://www.googleapis.com/auth/cclog \
             https://www.googleapis.com/auth/experimentsandconfigs"
        );
        Ok(())
    }

    #[test]
    fn load_endpoints_match_go_preference_order() {
        // Go `LoadEndpoints` (constants.go:57-61): Prod, Daily, Autopush.
        assert_eq!(
            load_endpoints(),
            vec![ENDPOINT_PROD, ENDPOINT_DAILY, ENDPOINT_AUTOPUSH]
        );
    }

    // -----------------------------------------------------------------------
    // version.go
    // -----------------------------------------------------------------------

    #[test]
    fn get_version_uses_fallback_until_set() {
        reset_version_state();
        assert_eq!(get_version(), USER_AGENT_VERSION_FALLBACK);
        assert_eq!(
            get_user_agent(),
            format!("antigravity/{USER_AGENT_VERSION_FALLBACK} windows/amd64")
        );
        set_version("2.5.1".to_string());
        assert_eq!(get_version(), "2.5.1");
        assert_eq!(get_user_agent(), "antigravity/2.5.1 windows/amd64");
        reset_version_state();
    }

    // -----------------------------------------------------------------------
    // envelope.go
    // -----------------------------------------------------------------------

    #[test]
    fn envelope_serializes_with_camel_case() -> TestResult {
        let env = new_antigravity_envelope(
            "rising-fact-p41fc",
            "gemini-2.5-flash",
            json!({"contents": []}),
            "agent-deadbeef",
        );
        let serialized = serde_json::to_value(&env)?;
        // camelCase keys (envelope.go:8-25).
        assert!(serialized.get("requestType").is_some());
        assert!(serialized.get("userAgent").is_some());
        assert!(serialized.get("requestId").is_some());
        assert_eq!(serialized["project"], "rising-fact-p41fc");
        assert_eq!(serialized["model"], "gemini-2.5-flash");
        assert_eq!(serialized["requestType"], "agent");
        assert_eq!(serialized["userAgent"], "antigravity");
        assert_eq!(serialized["requestId"], "agent-deadbeef");
        Ok(())
    }

    #[test]
    fn envelope_roundtrips_through_json() -> TestResult {
        let env = new_antigravity_envelope("p1", "claude-3", json!({"hi": 1}), "agent-x");
        let bytes = serde_json::to_string(&env)?;
        let back: AntigravityEnvelope = serde_json::from_str(&bytes)?;
        assert_eq!(env, back);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // transformer.go — parse_credentials, build_url
    // -----------------------------------------------------------------------

    #[test]
    fn parse_credentials_splits_on_pipe() {
        // Go transformer.go:124-131.
        assert_eq!(
            parse_credentials("refresh|project-1"),
            ("refresh".to_string(), "project-1".to_string())
        );
        assert_eq!(
            parse_credentials("norefresh"),
            ("norefresh".to_string(), String::new())
        );
        assert_eq!(parse_credentials(""), (String::new(), String::new()));
    }

    #[test]
    fn build_url_appends_action_path() {
        // Go transformer.go:407-414.
        assert_eq!(
            build_url("https://api.antigravity.dev", false),
            "https://api.antigravity.dev/v1internal:generateContent"
        );
        assert_eq!(
            build_url("https://api.antigravity.dev/", true),
            "https://api.antigravity.dev/v1internal:streamGenerateContent?alt=sse"
        );
    }

    // -----------------------------------------------------------------------
    // router.go — DetermineQuotaPreference (mirrors router_test.go:9-156)
    // -----------------------------------------------------------------------

    #[test]
    fn quota_preference_explicit_suffix() {
        // router_test.go:17-30.
        assert_eq!(
            determine_quota_preference("gemini-2.5-pro:antigravity"),
            QuotaPreference::Antigravity
        );
        assert_eq!(
            determine_quota_preference("claude-sonnet-4-5:gemini-cli"),
            QuotaPreference::GeminiCli
        );
        assert_eq!(
            determine_quota_preference("gemini-2.5-flash:antigravity"),
            QuotaPreference::Antigravity
        );
    }

    #[test]
    fn quota_preference_explicit_prefix() {
        // router_test.go:34-43.
        assert_eq!(
            determine_quota_preference("antigravity-gemini-2.5-flash"),
            QuotaPreference::Antigravity
        );
        assert_eq!(
            determine_quota_preference("antigravity-claude-sonnet-4-5"),
            QuotaPreference::Antigravity
        );
    }

    #[test]
    fn quota_preference_claude_and_gpt_models() {
        // router_test.go:47-77.
        for m in [
            "claude-sonnet-4-5",
            "claude-sonnet-4-5-thinking",
            "claude-opus-4-5-thinking",
            "Claude-Sonnet-4-5",
            "gpt-oss-120b-medium",
            "gpt-4",
        ] {
            assert_eq!(
                determine_quota_preference(m),
                QuotaPreference::Antigravity,
                "model = {m}"
            );
        }
    }

    #[test]
    fn quota_preference_image_and_imagen_models() {
        // router_test.go:81-89.
        assert_eq!(
            determine_quota_preference("gemini-3-pro-image"),
            QuotaPreference::Antigravity
        );
        assert_eq!(
            determine_quota_preference("imagen-3"),
            QuotaPreference::Antigravity
        );
    }

    #[test]
    fn quota_preference_legacy_gemini3() {
        // router_test.go:93-116.
        for m in [
            "gemini-3-pro-low",
            "gemini-3-pro-high",
            "gemini-3-pro-medium",
            "gemini-3-flash",
            "gemini-3-flash-low",
        ] {
            assert_eq!(
                determine_quota_preference(m),
                QuotaPreference::Antigravity,
                "model = {m}"
            );
        }
    }

    #[test]
    fn quota_preference_standard_gemini_defaults_to_cli() {
        // router_test.go:120-148.
        for m in [
            "gemini-2.5-pro",
            "gemini-2.5-flash",
            "gemini-2.5-flash-lite",
            "gemini-1.5-pro",
            "gemini-3-pro-preview",
            "gemini-3-flash-preview",
        ] {
            assert_eq!(
                determine_quota_preference(m),
                QuotaPreference::GeminiCli,
                "model = {m}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // router.go — endpoints (router_test.go:158-192)
    // -----------------------------------------------------------------------

    #[test]
    fn initial_endpoint_is_always_daily() {
        assert_eq!(
            get_initial_endpoint(QuotaPreference::Antigravity),
            ENDPOINT_DAILY
        );
        assert_eq!(
            get_initial_endpoint(QuotaPreference::GeminiCli),
            ENDPOINT_DAILY
        );
    }

    #[test]
    fn fallback_endpoints_match_go_order() {
        assert_eq!(
            get_fallback_endpoints(),
            [ENDPOINT_DAILY, ENDPOINT_AUTOPUSH, ENDPOINT_PROD]
        );
    }

    // -----------------------------------------------------------------------
    // router.go — name normalization (router_test.go:194-290)
    // -----------------------------------------------------------------------

    #[test]
    fn strip_model_suffix_matches_go() {
        assert_eq!(
            strip_model_suffix("gemini-2.5-pro:antigravity"),
            "gemini-2.5-pro"
        );
        assert_eq!(
            strip_model_suffix("claude-sonnet-4-5:gemini-cli"),
            "claude-sonnet-4-5"
        );
        assert_eq!(strip_model_suffix("gemini-2.5-pro"), "gemini-2.5-pro");
    }

    #[test]
    fn strip_antigravity_prefix_matches_go_case_sensitive() {
        // router_test.go:225-254 — Go only strips lowercase `antigravity-`.
        assert_eq!(
            strip_antigravity_prefix("antigravity-gemini-2.5-pro"),
            "gemini-2.5-pro"
        );
        assert_eq!(strip_antigravity_prefix("gemini-2.5-pro"), "gemini-2.5-pro");
        // Case-sensitive: Capitalized prefix is preserved.
        assert_eq!(
            strip_antigravity_prefix("Antigravity-gemini-2.5-pro"),
            "Antigravity-gemini-2.5-pro"
        );
    }

    #[test]
    fn normalize_model_name_matches_go() {
        assert_eq!(
            normalize_model_name("antigravity-gemini-2.5-pro:gemini-cli"),
            "gemini-2.5-pro"
        );
        assert_eq!(
            normalize_model_name("gemini-2.5-pro:antigravity"),
            "gemini-2.5-pro"
        );
        assert_eq!(
            normalize_model_name("antigravity-claude-sonnet-4-5"),
            "claude-sonnet-4-5"
        );
        assert_eq!(normalize_model_name("gemini-2.5-pro"), "gemini-2.5-pro");
    }

    // -----------------------------------------------------------------------
    // router.go — ShouldRetryWithDifferentEndpoint (router_test.go:292-317)
    // -----------------------------------------------------------------------

    #[test]
    fn should_retry_with_different_endpoint_matches_go() {
        for code in [429u16, 403, 404, 500, 502, 503, 504] {
            assert!(
                should_retry_with_different_endpoint(code),
                "expected retry for {code}"
            );
        }
        for code in [200u16, 400, 401] {
            assert!(
                !should_retry_with_different_endpoint(code),
                "expected no-retry for {code}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // router.go — transformModelForAntigravity (router.go:137-156)
    // -----------------------------------------------------------------------

    #[test]
    fn transform_model_for_antigravity_appends_default_tier() {
        // gemini-3-pro without a tier suffix gets `-low`.
        assert_eq!(
            transform_model_for_antigravity("gemini-3-pro"),
            "gemini-3-pro-low"
        );
        // Existing tier suffix is preserved.
        assert_eq!(
            transform_model_for_antigravity("gemini-3-pro-high"),
            "gemini-3-pro-high"
        );
        // Non-pro gemini-3 models are unchanged.
        assert_eq!(
            transform_model_for_antigravity("gemini-3-flash"),
            "gemini-3-flash"
        );
        // Claude / other models pass through (normalized only).
        assert_eq!(
            transform_model_for_antigravity("claude-sonnet-4-5"),
            "claude-sonnet-4-5"
        );
        // Prefix + suffix are stripped before tier logic.
        assert_eq!(
            transform_model_for_antigravity("antigravity-gemini-3-pro:gemini-cli"),
            "gemini-3-pro-low"
        );
    }

    // -----------------------------------------------------------------------
    // router.go — replace_base_url (executor.go:376-415)
    // -----------------------------------------------------------------------

    #[test]
    fn replace_base_url_swaps_host_preserving_v1internal_path() {
        let original =
            "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:generateContent";
        let new_base = "https://cloudcode-pa.googleapis.com";
        assert_eq!(
            replace_base_url(original, new_base),
            "https://cloudcode-pa.googleapis.com/v1internal:generateContent"
        );
    }

    #[test]
    fn replace_base_url_preserves_query_and_fragment() {
        let original = "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:streamGenerateContent?alt=sse";
        let new_base = "https://autopush-cloudcode-pa.sandbox.googleapis.com";
        let replaced = replace_base_url(original, new_base);
        assert_eq!(
            replaced,
            "https://autopush-cloudcode-pa.sandbox.googleapis.com/v1internal:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn replace_base_url_returns_original_when_no_v1internal_path() {
        let original = "https://example.com/other:action";
        let new_base = "https://different.example.com";
        assert_eq!(replace_base_url(original, new_base), original);
    }

    // -----------------------------------------------------------------------
    // sanitizer.go — uppercase_schema_types (sanitizer_uppercase_test.go)
    // -----------------------------------------------------------------------

    #[test]
    fn uppercase_simple_type() -> TestResult {
        let out = uppercase_schema_types(json!({"type": "string"}));
        assert_eq!(out["type"], "STRING");
        Ok(())
    }

    #[test]
    fn uppercase_object_with_properties() -> TestResult {
        let input = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer"}
            }
        });
        let out = uppercase_schema_types(input);
        assert_eq!(out["type"], "OBJECT");
        assert_eq!(out["properties"]["name"]["type"], "STRING");
        assert_eq!(out["properties"]["age"]["type"], "INTEGER");
        Ok(())
    }

    #[test]
    fn uppercase_array_items() -> TestResult {
        let out = uppercase_schema_types(json!({
            "type": "array",
            "items": {"type": "string"}
        }));
        assert_eq!(out["type"], "ARRAY");
        assert_eq!(out["items"]["type"], "STRING");
        Ok(())
    }

    #[test]
    fn uppercase_union_types_in_anyof() -> TestResult {
        let out = uppercase_schema_types(json!({
            "anyOf": [
                {"type": "string"},
                {"type": "number"}
            ]
        }));
        assert_eq!(out["anyOf"][0]["type"], "STRING");
        assert_eq!(out["anyOf"][1]["type"], "NUMBER");
        Ok(())
    }

    #[test]
    fn uppercase_preserves_non_type_fields() -> TestResult {
        let out = uppercase_schema_types(json!({
            "type": "string",
            "description": "A string field",
            "minLength": 1,
            "maxLength": 100
        }));
        assert_eq!(out["type"], "STRING");
        assert_eq!(out["description"], "A string field");
        assert_eq!(out["minLength"], 1);
        assert_eq!(out["maxLength"], 100);
        Ok(())
    }

    #[test]
    fn uppercase_handles_nested_objects() -> TestResult {
        let input = json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "object",
                    "properties": {
                        "street": {"type": "string"},
                        "zipCode": {"type": "integer"}
                    }
                }
            }
        });
        let out = uppercase_schema_types(input);
        assert_eq!(out["type"], "OBJECT");
        assert_eq!(out["properties"]["address"]["type"], "OBJECT");
        assert_eq!(
            out["properties"]["address"]["properties"]["street"]["type"],
            "STRING"
        );
        assert_eq!(
            out["properties"]["address"]["properties"]["zipCode"]["type"],
            "INTEGER"
        );
        Ok(())
    }

    #[test]
    fn uppercase_handles_nul_and_empty_schemas() -> TestResult {
        // sanitizer_uppercase_test.go:115 — nil input returns nil.
        assert!(uppercase_schema_types(json!(null)).is_null());
        // sanitizer_uppercase_test.go:120 — empty object stays empty.
        let out = uppercase_schema_types(json!({}));
        assert!(out.as_object().map(|m| m.is_empty()).unwrap_or(false));
        Ok(())
    }

    #[test]
    fn uppercase_handles_allof() -> TestResult {
        let input = json!({
            "allOf": [
                {"type": "object", "properties": {"name": {"type": "string"}}},
                {"type": "object", "properties": {"age": {"type": "integer"}}}
            ]
        });
        let out = uppercase_schema_types(input);
        assert_eq!(out["allOf"][0]["type"], "OBJECT");
        assert_eq!(out["allOf"][1]["type"], "OBJECT");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // sanitizer.go — sanitize_json_schema (sanitizer_test.go)
    // -----------------------------------------------------------------------

    /// Mirrors the `checkExpected` helper from sanitizer_test.go:204-246:
    /// walk the expected JSON tree and assert each key exists with an equal
    /// value at the same path in `actual`.
    fn check_expected(expected: &serde_json::Value, actual: &serde_json::Value) {
        match expected {
            serde_json::Value::Object(exp_map) => {
                let act_map = actual
                    .as_object()
                    .unwrap_or_else(|| panic!("expected object at path, got: {actual:?}"));
                for (k, v) in exp_map {
                    assert!(
                        act_map.contains_key(k),
                        "expected key `{k}` missing in actual {actual:?}"
                    );
                    check_expected(v, &act_map[k]);
                }
            }
            serde_json::Value::Array(exp_arr) => {
                let act_arr = actual
                    .as_array()
                    .unwrap_or_else(|| panic!("expected array at path, got: {actual:?}"));
                assert_eq!(act_arr.len(), exp_arr.len(), "array length mismatch");
                for (i, v) in exp_arr.iter().enumerate() {
                    check_expected(v, &act_arr[i]);
                }
            }
            other => assert_eq!(other, actual),
        }
    }

    #[test]
    fn sanitize_converts_ref_to_hints() -> TestResult {
        let input = json!({
            "type": "object",
            "properties": {"user": {"$ref": "#/$defs/User"}}
        });
        let expected = json!({
            "type": "object",
            "properties": {"user": {"type": "object", "description": "See: User"}}
        });
        let sanitized = sanitize_json_schema(input);
        check_expected(&expected, &sanitized);
        Ok(())
    }

    #[test]
    fn sanitize_converts_const_to_enum() -> TestResult {
        let input = json!({
            "type": "object",
            "properties": {"mode": {"const": "json"}}
        });
        let expected = json!({
            "properties": {"mode": {"enum": ["json"]}}
        });
        let sanitized = sanitize_json_schema(input);
        check_expected(&expected, &sanitized);
        Ok(())
    }

    #[test]
    fn sanitize_adds_enum_hints() -> TestResult {
        let input = json!({"type": "string", "enum": ["a", "b"]});
        let expected =
            json!({"type": "string", "enum": ["a", "b"], "description": "Allowed: a, b"});
        let sanitized = sanitize_json_schema(input);
        check_expected(&expected, &sanitized);
        Ok(())
    }

    #[test]
    fn sanitize_adds_additional_properties_hints_and_placeholder() -> TestResult {
        let input = json!({"type": "object", "additionalProperties": false});
        let expected = json!({
            "type": "object",
            "description": "No extra properties allowed",
            "properties": {
                "_placeholder": {"type": "boolean", "description": "Placeholder. Always pass true."}
            },
            "required": ["_placeholder"]
        });
        let sanitized = sanitize_json_schema(input);
        check_expected(&expected, &sanitized);
        Ok(())
    }

    #[test]
    fn sanitize_moves_constraints_to_description() -> TestResult {
        let input = json!({"type": "string", "minLength": 5, "maxLength": 10});
        let expected = json!({"type": "string", "description": "minLength: 5 (maxLength: 10)"});
        let sanitized = sanitize_json_schema(input);
        check_expected(&expected, &sanitized);
        Ok(())
    }

    #[test]
    fn sanitize_removes_constraint_keywords_from_nested_properties() -> TestResult {
        let input = json!({
            "type": "object",
            "properties": {
                "max_turns": {
                    "type": "integer",
                    "description": "Maximum number of agentic turns",
                    "exclusiveMinimum": 0
                }
            }
        });
        let expected = json!({
            "type": "object",
            "properties": {
                "max_turns": {
                    "type": "integer",
                    "description": "Maximum number of agentic turns (exclusiveMinimum: 0)"
                }
            }
        });
        let sanitized = sanitize_json_schema(input);
        check_expected(&expected, &sanitized);
        Ok(())
    }

    #[test]
    fn sanitize_merges_allof() -> TestResult {
        let input = json!({
            "allOf": [
                {"properties": {"a": {"type": "string"}}, "required": ["a"]},
                {"properties": {"b": {"type": "integer"}}}
            ]
        });
        let expected = json!({
            "properties": {"a": {"type": "string"}, "b": {"type": "integer"}},
            "required": ["a"]
        });
        let sanitized = sanitize_json_schema(input);
        check_expected(&expected, &sanitized);
        Ok(())
    }

    #[test]
    fn sanitize_flattens_anyof() -> TestResult {
        let input = json!({
            "anyOf": [{"type": "string"}, {"type": "integer"}]
        });
        let expected = json!({"type": "string", "description": "Accepts: string | integer"});
        let sanitized = sanitize_json_schema(input);
        check_expected(&expected, &sanitized);
        Ok(())
    }

    #[test]
    fn sanitize_flattens_nullable_type_array() -> TestResult {
        let input = json!({"type": ["string", "null"]});
        let expected = json!({"type": "string", "description": "nullable"});
        let sanitized = sanitize_json_schema(input);
        check_expected(&expected, &sanitized);
        Ok(())
    }

    #[test]
    fn sanitize_adds_placeholder_for_empty_object() -> TestResult {
        let input = json!({"type": "object", "properties": {}});
        let expected = json!({
            "type": "object",
            "properties": {
                "_placeholder": {"type": "boolean", "description": "Placeholder. Always pass true."}
            },
            "required": ["_placeholder"]
        });
        let sanitized = sanitize_json_schema(input);
        check_expected(&expected, &sanitized);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // health_tracker.go (mirrors health_tracker_test.go)
    // -----------------------------------------------------------------------

    #[test]
    fn health_tracker_record_failure_then_get() {
        // health_tracker_test.go:12-24.
        let tracker = AntigravityHealthTracker::new();
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 429);
        let failure = tracker
            .get_failure("claude-sonnet-4-5", ENDPOINT_DAILY)
            .unwrap_or_else(|| panic!("expected failure record"));
        assert_eq!(failure.model, "claude-sonnet-4-5");
        assert_eq!(failure.endpoint, ENDPOINT_DAILY);
        assert_eq!(failure.status_code, 429);
        assert_eq!(
            failure.cooldown_until,
            failure.last_failed_at + chrono::Duration::seconds(60)
        );
    }

    #[test]
    fn health_tracker_record_success_clears_failure() {
        // health_tracker_test.go:26-36.
        let tracker = AntigravityHealthTracker::new();
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 429);
        assert!(
            tracker
                .get_failure("claude-sonnet-4-5", ENDPOINT_DAILY)
                .is_some()
        );
        tracker.record_success("claude-sonnet-4-5", ENDPOINT_DAILY);
        assert!(
            tracker
                .get_failure("claude-sonnet-4-5", ENDPOINT_DAILY)
                .is_none()
        );
    }

    #[test]
    fn health_tracker_should_skip_within_cooldown() {
        // health_tracker_test.go:38-46.
        let tracker = AntigravityHealthTracker::new_with_config(
            Duration::from_millis(100),
            Duration::from_secs(600),
        );
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 429);
        assert!(tracker.should_skip("claude-sonnet-4-5", ENDPOINT_DAILY));
    }

    #[test]
    fn health_tracker_should_skip_false_after_cooldown_expires() {
        // health_tracker_test.go:48-62.
        let tracker = AntigravityHealthTracker::new_with_config(
            Duration::from_millis(10),
            Duration::from_secs(600),
        );
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 429);
        assert!(tracker.should_skip("claude-sonnet-4-5", ENDPOINT_DAILY));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!tracker.should_skip("claude-sonnet-4-5", ENDPOINT_DAILY));
    }

    #[test]
    fn health_tracker_per_model_isolation() {
        // health_tracker_test.go:64-78.
        let tracker = AntigravityHealthTracker::new();
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 429);
        assert!(tracker.should_skip("claude-sonnet-4-5", ENDPOINT_DAILY));
        assert!(!tracker.should_skip("gemini-2.5-pro", ENDPOINT_DAILY));
        assert!(!tracker.should_skip("claude-sonnet-4-5", ENDPOINT_PROD));
    }

    #[test]
    fn health_tracker_ttl_expiration() {
        // health_tracker_test.go:80-99.
        let tracker = AntigravityHealthTracker::new_with_config(
            Duration::from_secs(60),
            Duration::from_millis(20),
        );
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 429);
        assert!(
            tracker
                .get_failure("claude-sonnet-4-5", ENDPOINT_DAILY)
                .is_some()
        );
        std::thread::sleep(Duration::from_millis(30));
        assert!(
            tracker
                .get_failure("claude-sonnet-4-5", ENDPOINT_DAILY)
                .is_none()
        );
        assert!(!tracker.should_skip("claude-sonnet-4-5", ENDPOINT_DAILY));
    }

    #[test]
    fn health_tracker_stats_reports_in_cooldown_and_expired() {
        // health_tracker_test.go:127-154.
        let tracker = AntigravityHealthTracker::new_with_config(
            Duration::from_millis(100),
            Duration::from_secs(600),
        );

        let stats = tracker.stats();
        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.in_cooldown, 0);

        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 429);
        tracker.record_failure("gemini-2.5-pro", ENDPOINT_PROD, 503);

        let stats = tracker.stats();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.in_cooldown, 2);
        assert_eq!(stats.cooldown_entries.len(), 2);

        std::thread::sleep(Duration::from_millis(150));

        let stats = tracker.stats();
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.in_cooldown, 0);
        assert_eq!(stats.cooldown_entries.len(), 0);
    }

    #[test]
    fn health_tracker_clear_removes_all_entries() {
        // health_tracker_test.go:199-215.
        let tracker = AntigravityHealthTracker::new();
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 429);
        tracker.record_failure("gemini-2.5-pro", ENDPOINT_PROD, 503);
        assert_eq!(tracker.stats().total_entries, 2);
        tracker.clear();
        let stats = tracker.stats();
        assert_eq!(stats.total_entries, 0);
        assert!(
            tracker
                .get_failure("claude-sonnet-4-5", ENDPOINT_DAILY)
                .is_none()
        );
    }

    #[test]
    fn health_tracker_multiple_models_and_endpoints() {
        // health_tracker_test.go:217-241.
        let tracker = AntigravityHealthTracker::new();
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 429);
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_PROD, 503);
        tracker.record_failure("gemini-2.5-pro", ENDPOINT_DAILY, 404);
        tracker.record_failure("gemini-3-flash", ENDPOINT_AUTOPUSH, 500);

        assert!(tracker.should_skip("claude-sonnet-4-5", ENDPOINT_DAILY));
        assert!(tracker.should_skip("claude-sonnet-4-5", ENDPOINT_PROD));
        assert!(tracker.should_skip("gemini-2.5-pro", ENDPOINT_DAILY));
        assert!(tracker.should_skip("gemini-3-flash", ENDPOINT_AUTOPUSH));

        assert!(!tracker.should_skip("claude-sonnet-4-5", ENDPOINT_AUTOPUSH));
        assert!(!tracker.should_skip("gemini-2.5-pro", ENDPOINT_PROD));
        assert!(!tracker.should_skip("gemini-3-flash", ENDPOINT_DAILY));

        let stats = tracker.stats();
        assert_eq!(stats.total_entries, 4);
        assert_eq!(stats.in_cooldown, 4);
    }

    #[test]
    fn health_tracker_update_existing_failure() {
        // health_tracker_test.go:243-263.
        let tracker = AntigravityHealthTracker::new_with_config(
            Duration::from_millis(100),
            Duration::from_secs(600),
        );
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 429);
        let first = tracker
            .get_failure("claude-sonnet-4-5", ENDPOINT_DAILY)
            .unwrap_or_else(|| panic!("expected first failure"));
        std::thread::sleep(Duration::from_millis(20));
        tracker.record_failure("claude-sonnet-4-5", ENDPOINT_DAILY, 503);
        let second = tracker
            .get_failure("claude-sonnet-4-5", ENDPOINT_DAILY)
            .unwrap_or_else(|| panic!("expected second failure"));
        assert_eq!(second.status_code, 503);
        assert!(second.last_failed_at > first.last_failed_at);
        assert!(second.cooldown_until > first.cooldown_until);
    }

    // -----------------------------------------------------------------------
    // token_provider.go — form-encoded request builders
    // -----------------------------------------------------------------------

    #[test]
    fn build_exchange_form_body_is_alphabetical_and_urlencoded() -> TestResult {
        let body = build_exchange_form_body(CLIENT_ID, "my-code", REDIRECT_URI, "my-verifier");
        // Go's url.Values.Encode() sorts keys alphabetically and
        // percent-encodes `:` and `/` in the redirect URI per RFC 3986.
        // Expected order:
        //   client_id, client_secret, code, code_verifier, grant_type, redirect_uri
        let expected_redirect = "http%3A%2F%2Flocalhost%3A51121%2Foauth-callback";
        assert_eq!(
            body,
            format!(
                "client_id={cid}&client_secret={csec}&code=my-code&code_verifier=my-verifier&grant_type=authorization_code&redirect_uri={ru}",
                cid = CLIENT_ID,
                csec = CLIENT_SECRET,
                ru = expected_redirect,
            )
        );
        Ok(())
    }

    #[test]
    fn build_exchange_form_body_percent_encodes_special_chars() -> TestResult {
        let body = build_exchange_form_body(
            CLIENT_ID,
            "code with spaces+plus/slash",
            REDIRECT_URI,
            "verifier",
        );
        // space -> `+`, `/` -> `%2F`, `+` -> `%2B`.
        assert!(
            body.contains("code=code+with+spaces%2Bplus%2Fslash"),
            "got: {body}"
        );
        Ok(())
    }

    #[test]
    fn build_refresh_form_body_rejects_empty_refresh_token() {
        // Use `match` instead of `.unwrap_err()` — the workspace lints deny
        // `clippy::unwrap_used` (which also fires for `.unwrap_err()`).
        match build_refresh_form_body(CLIENT_ID, "") {
            Ok(body) => panic!("expected error, got body: {body}"),
            Err(err) => assert!(
                format!("{err}").contains("refresh_token is empty"),
                "error string did not match Go: {err}"
            ),
        }
    }

    #[test]
    fn build_refresh_form_body_is_alphabetical() -> TestResult {
        let body = build_refresh_form_body(CLIENT_ID, "rtoken")?;
        assert_eq!(
            body,
            format!(
                "client_id={cid}&client_secret={csec}&grant_type=refresh_token&refresh_token=rtoken",
                cid = CLIENT_ID,
                csec = CLIENT_SECRET,
            )
        );
        Ok(())
    }

    #[test]
    fn build_exchange_request_assembles_http_request() -> TestResult {
        let req = build_exchange_request(
            CLIENT_ID,
            "code",
            REDIRECT_URI,
            "verifier",
            Some("custom-ua"),
        )?;
        assert_eq!(req.method, "POST");
        assert_eq!(req.url.as_deref(), Some(TOKEN_URL));
        assert_eq!(
            req.headers.get("Content-Type").map(String::as_str),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(
            req.headers.get("Accept").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(
            req.headers.get("User-Agent").map(String::as_str),
            Some("custom-ua")
        );
        assert!(req.body.as_deref().is_some_and(|b| !b.is_empty()));
        Ok(())
    }

    #[test]
    fn build_exchange_request_defaults_user_agent_when_empty() -> TestResult {
        let req = build_exchange_request(CLIENT_ID, "code", REDIRECT_URI, "verifier", Some(""))?;
        // Empty UA falls back to `get_user_agent()` (token_provider.go:14-16).
        assert_eq!(
            req.headers.get("User-Agent").map(String::as_str),
            Some(get_user_agent().as_str())
        );
        Ok(())
    }

    #[test]
    fn build_refresh_request_assembles_http_request() -> TestResult {
        let req = build_refresh_request(CLIENT_ID, "rtoken", None)?;
        assert_eq!(req.method, "POST");
        assert_eq!(req.url.as_deref(), Some(TOKEN_URL));
        assert!(req.body.as_deref().is_some_and(|b| !b.is_empty()));
        // None UA also falls back to get_user_agent().
        assert_eq!(
            req.headers.get("User-Agent").map(String::as_str),
            Some(get_user_agent().as_str())
        );
        Ok(())
    }

    #[test]
    fn default_token_urls_match_google_endpoints() {
        let (auth, token) = default_token_urls();
        assert_eq!(auth, AUTHORIZE_URL);
        assert_eq!(token, TOKEN_URL);
    }
}
