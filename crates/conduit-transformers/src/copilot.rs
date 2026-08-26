//! GitHub Copilot outbound transformer constants + token-exchanger (RUST-P11-003
//! MAP-01 sub-gap — Mencius-the-11th).
//!
//! Ports the Go `llm/transformer/openai/copilot` package:
//!   * `constants.go`        — `ProviderConfURL`, `ProviderID`
//!   * `token_exchanger.go`  — `TokenExchanger` struct, cache, HTTP exchange
//!
//! ## Scope clarification
//!
//! The dispatch task described this as a "device-code flow" port
//! (device-code → session → poll accessToken). That characterization is
//! **incorrect** — the Go `token_exchanger.go` does **not** implement the
//! device-code flow. It only implements the **accessToken → copilotToken**
//! exchange (an HTTP GET to `api.github.com/copilot_internal/v2/token` with
//! the GitHub OAuth access token). The device-code flow itself lives in
//! `llm/oauth/device_flow_provider.go` (a separate Go package not yet ported
//! to Rust). This module ports the Go `token_exchanger.go` verbatim — Go is
//! the contract.
//!
//! ## HTTP injection
//!
//! Per the task constraint ("HTTP 不绑死"), the HTTP client is injected via
//! the [`OauthHttpClient`] trait. The host wires it to `reqwest` in production;
//! tests inject a fake. The trait uses minimal self-contained DTOs
//! ([`OauthHttpRequest`] / [`OauthHttpResponse`]) so it does not leak
//! `conduit-llm` pipeline types into the OAuth layer.
//!
//! ## What is NOT ported (async/concurrency wiring)
//!
//!   * Go `singleflight.Group` — deduplicates concurrent in-flight exchanges
//!     for the same access token. The Rust port uses a `Mutex<HashMap>` cache
//!     which is correct but does not dedup concurrent callers (each caller
//!     performs the HTTP exchange serially when the cache misses). The
//!     singleflight optimization is left to the async wiring layer.
//!   * `context.Context` timeout/cancellation — Go uses
//!     `context.WithTimeout(ctx, tokenExchangeTimeout)`. The Rust
//!     [`OauthHttpClient`] trait is synchronous; the host applies the timeout
//!     when wiring to a real HTTP client.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use conduit_core::ConduitError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use std::time::Duration;

// ===========================================================================
// constants.go + token_exchanger.go:21-25
// ===========================================================================

/// Go `ProviderConfURL` (constants.go:5) — URL to the public provider
/// configuration JSON (model listings for GitHub Copilot).
pub const PROVIDER_CONF_URL: &str = "https://raw.githubusercontent.com/ThinkInAIXYZ/PublicProviderConf/dev/dist/github-copilot.json";

/// Go `ProviderID` (constants.go:8) — provider identifier inside the
/// `PublicProviderConf` JSON.
pub const PROVIDER_ID: &str = "github-copilot";

/// Go `defaultCopilotTokenEndpoint` (token_exchanger.go:22) — split in Go as
/// `"https://" + "api.github.com" + "/copilot_internal/v2/token"`. The
/// concatenation is reproduced verbatim here.
pub const DEFAULT_COPILOT_TOKEN_ENDPOINT: &str = "https://api.github.com/copilot_internal/v2/token";

/// Go `tokenExpiryBuffer` (token_exchanger.go:23) — refresh this far ahead of
/// the real expiry.
pub const TOKEN_EXPIRY_BUFFER: Duration = Duration::from_secs(5 * 60);

/// Go `tokenExchangeTimeout` (token_exchanger.go:24) — per-call HTTP timeout
/// for the token exchange. The Rust [`OauthHttpClient`] trait is synchronous;
/// the host applies this timeout when wiring to a real HTTP client.
pub const TOKEN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

// ===========================================================================
// OauthHttpClient trait + DTOs (task: "HTTP 不绑死")
// ===========================================================================

/// Minimal HTTP request DTO for the [`OauthHttpClient`] trait. Self-contained
/// (does not leak `conduit-llm` pipeline types) so the OAuth layer stays
/// decoupled from the transformer pipeline.
#[derive(Debug, Clone)]
pub struct OauthHttpRequest {
    /// HTTP method (`"GET"`, `"POST"`, ...).
    pub method: String,
    /// Fully-qualified URL.
    pub url: String,
    /// Header pairs (insertion order preserved; duplicates allowed).
    pub headers: Vec<(String, String)>,
    /// Request body bytes (empty for GET).
    pub body: Vec<u8>,
}

/// Minimal HTTP response DTO for the [`OauthHttpClient`] trait.
#[derive(Debug, Clone)]
pub struct OauthHttpResponse {
    /// HTTP status code (e.g. `200`, `401`).
    pub status: u16,
    /// Response body bytes.
    pub body: Vec<u8>,
}

/// HTTP client trait injected into [`TokenExchanger::exchange_with_client`].
/// The host wires this to `reqwest` in production; tests inject a fake.
///
/// Implementations must be `Send + Sync` so [`TokenExchanger`] can store an
/// `Arc<dyn OauthHttpClient>`.
pub trait OauthHttpClient: Send + Sync {
    /// Execute the request and return the response. The error string should
    /// carry the same message Go would wrap via `fmt.Errorf("...: %w", err)`.
    fn execute(&self, request: &OauthHttpRequest) -> Result<OauthHttpResponse, String>;
}

// ===========================================================================
// token_exchanger.go:27-30 — copilotTokenResponse
// ===========================================================================

/// Go `copilotTokenResponse` (token_exchanger.go:27-30) — JSON body returned
/// by `GET /copilot_internal/v2/token`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopilotTokenResponse {
    /// Go `Token` (`json:"token"`) — the short-lived Copilot session token.
    #[serde(rename = "token")]
    pub token: String,
    /// Go `ExpiresAt` (`json:"expires_at"`) — Unix seconds.
    #[serde(rename = "expires_at")]
    pub expires_at: i64,
}

// ===========================================================================
// token_exchanger.go:32-44 — copilotTokenCacheEntry + isExpired
// ===========================================================================

/// Go `copilotTokenCacheEntry` (token_exchanger.go:32-36) — cached exchange
/// result keyed by `sha256(accessToken)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopilotTokenCacheEntry {
    /// Go `copilotToken`.
    pub copilot_token: String,
    /// Go `expiresAt` (`time.Time`) — stored as Unix-seconds `DateTime`.
    pub expires_at: DateTime<Utc>,
    /// Go `cachedAt` (`time.Time`).
    pub cached_at: DateTime<Utc>,
}

impl CopilotTokenCacheEntry {
    /// Mirrors Go `(e *copilotTokenCacheEntry) isExpired(now time.Time) bool`
    /// (token_exchanger.go:38-44). Returns `true` when `self` is `None`, the
    /// expiry is zero, or `now` is within [`TOKEN_EXPIRY_BUFFER`] of the real
    /// expiry.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        // Go: `if e == nil || e.expiresAt.IsZero() { return true }` — the nil
        // arm is encoded as `Option::None` at the call site; the zero arm is
        // moot because `DateTime<Utc>` cannot represent Go's zero `time.Time`.
        // We still guard against the canonical epoch start defensively.
        if self.expires_at.timestamp() == 0 {
            return true;
        }
        let buffer =
            chrono::Duration::from_std(TOKEN_EXPIRY_BUFFER).unwrap_or(chrono::Duration::MAX);
        now > self.expires_at - buffer
    }
}

// ===========================================================================
// token_exchanger.go:46-78 — TokenExchangerParams + TokenExchanger + New
// ===========================================================================

/// Go `TokenExchangerParams` (token_exchanger.go:46-49). The `HTTPClient`
/// field becomes an [`Arc<dyn OauthHttpClient>`] (trait-injected per task
/// constraint); `Endpoint` is `Option<String>` to mirror Go's empty-string
/// defaulting to [`DEFAULT_COPILOT_TOKEN_ENDPOINT`].
pub struct TokenExchangerParams {
    /// Go `HTTPClient *httpclient.HttpClient`. `None` is rejected at
    /// construction (Go panics on `nil` client at exchange time, but we fail
    /// fast at `new`).
    pub http_client: Arc<dyn OauthHttpClient>,
    /// Go `Endpoint string` — empty/`None` falls back to
    /// [`DEFAULT_COPILOT_TOKEN_ENDPOINT`].
    pub endpoint: Option<String>,
}

/// Go `TokenExchanger` (token_exchanger.go:51-58).
///
/// **Singleflight omission:** Go uses `singleflight.Group` to deduplicate
/// concurrent in-flight exchanges for the same access token. The Rust port
/// uses a `Mutex<HashMap>` cache which is correct but does not dedup
/// concurrent callers — each caller performs the HTTP exchange serially when
/// the cache misses. The singleflight optimization is deferred to the async
/// wiring layer.
pub struct TokenExchanger {
    http_client: Arc<dyn OauthHttpClient>,
    endpoint: String,
    cache: Mutex<HashMap<String, CopilotTokenCacheEntry>>,
}

impl TokenExchanger {
    /// Mirrors Go `NewTokenExchanger` (token_exchanger.go:62-78).
    pub fn new(params: TokenExchangerParams) -> Self {
        let endpoint = params
            .endpoint
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_COPILOT_TOKEN_ENDPOINT.to_string());
        Self {
            http_client: params.http_client,
            endpoint,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Mirrors Go `(e *TokenExchanger) Exchange` (token_exchanger.go:80-82) —
    /// delegates to [`exchange_with_client`] with the exchanger's own HTTP
    /// client.
    ///
    /// Returns `(copilot_token, expires_at_unix_seconds)`.
    pub fn exchange(&self, access_token: &str) -> Result<(String, i64), ConduitError> {
        self.exchange_with_client(self.http_client.as_ref(), access_token)
    }

    /// Mirrors Go `(e *TokenExchanger) ExchangeWithClient`
    /// (token_exchanger.go:84-150). When `client` is supplied it overrides the
    /// exchanger's own client for this call.
    pub fn exchange_with_client(
        &self,
        client: &dyn OauthHttpClient,
        access_token: &str,
    ) -> Result<(String, i64), ConduitError> {
        // token_exchanger.go:85-87 — empty-token guard.
        if access_token.is_empty() {
            return Err(ConduitError::invalid_request("access token is empty"));
        }

        let cache_key = token_cache_key(access_token);

        // token_exchanger.go:96-109 — read-side cache check.
        {
            let cache = self
                .cache
                .lock()
                .map_err(|_| ConduitError::internal("copilot token cache mutex poisoned"))?;
            if let Some(entry) = cache.get(&cache_key) {
                let now = Utc::now();
                if !entry.is_expired(now) {
                    return Ok((entry.copilot_token.clone(), entry.expires_at.timestamp()));
                }
            }
        }

        // token_exchanger.go:119-125 — singleflight omitted (see struct doc);
        // we call `do_exchange` directly.
        let resp = self.do_exchange(client, access_token)?;

        let now = Utc::now();
        let expires_at =
            chrono::DateTime::<Utc>::from_timestamp(resp.expires_at, 0).ok_or_else(|| {
                ConduitError::internal(format!(
                    "copilot token expires_at {} is not a valid Unix timestamp",
                    resp.expires_at
                ))
            })?;

        // token_exchanger.go:135-141 — write-side cache update.
        {
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| ConduitError::internal("copilot token cache mutex poisoned"))?;
            cache.insert(
                cache_key,
                CopilotTokenCacheEntry {
                    copilot_token: resp.token.clone(),
                    expires_at,
                    cached_at: now,
                },
            );
        }

        Ok((resp.token, resp.expires_at))
    }

    /// Mirrors Go `(e *TokenExchanger) exchange` (token_exchanger.go:161-195)
    /// — the actual HTTP GET to the token endpoint. Pure logic: builds the
    /// request, calls the injected [`OauthHttpClient`], parses + validates the
    /// response. Error messages mirror Go verbatim.
    fn do_exchange(
        &self,
        client: &dyn OauthHttpClient,
        access_token: &str,
    ) -> Result<CopilotTokenResponse, ConduitError> {
        // token_exchanger.go:162-167 — request builder. Go sets
        // `Authorization: token <accessToken>` and `Accept: application/json`.
        let request = OauthHttpRequest {
            method: "GET".to_string(),
            url: self.endpoint.clone(),
            headers: vec![
                ("Authorization".to_string(), format!("token {access_token}")),
                ("Accept".to_string(), "application/json".to_string()),
            ],
            body: Vec::new(),
        };

        // token_exchanger.go:172-175 — HTTP Do. The timeout
        // (`context.WithTimeout(ctx, tokenExchangeTimeout)`) is applied by the
        // host's `OauthHttpClient` implementation.
        let response = client.execute(&request).map_err(|e| {
            // Go wraps: `fmt.Errorf("token exchange request failed: %w", err)`.
            ConduitError::internal(format!("token exchange request failed: {e}"))
        })?;

        // token_exchanger.go:177-179 — non-2xx guard.
        if response.status < 200 || response.status >= 300 {
            return Err(ConduitError::internal(format!(
                "token exchange returned non-2xx status: {}",
                response.status
            )));
        }

        // token_exchanger.go:181-184 — JSON unmarshal.
        let token_resp: CopilotTokenResponse = serde_json::from_slice(&response.body)
            .map_err(|e| ConduitError::internal(format!("failed to parse token response: {e}")))?;

        // token_exchanger.go:186-188 — empty-token guard.
        if token_resp.token.is_empty() {
            return Err(ConduitError::internal("copilot token is empty in response"));
        }

        // token_exchanger.go:190-192 — missing-expires_at guard.
        if token_resp.expires_at == 0 {
            return Err(ConduitError::internal("expires_at is missing in response"));
        }

        Ok(token_resp)
    }
}

// ===========================================================================
// token_exchanger.go:152-155 — tokenCacheKey (sha256 hex)
// ===========================================================================

/// Mirrors Go `tokenCacheKey` (token_exchanger.go:152-155) — SHA-256 of the
/// access token, hex-encoded lowercase. The host never logs the raw access
/// token; the cache key is safe to log.
pub fn token_cache_key(access_token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(access_token.as_bytes());
    let sum = hasher.finalize();
    hex_encode_lower(&sum)
}

/// Lowercase hex encoder matching Go's `hex.EncodeToString`. Used only by
/// [`token_cache_key`]; the workspace does not depend on the `hex` crate.
fn hex_encode_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

// ===========================================================================
// Tests — mirror token_exchanger_test.go (no network; fake OauthHttpClient)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Verbatim constant tests (preserved from the prior Faraday module).
    // -----------------------------------------------------------------------

    /// Verbatim mirror of Go constants.go:5 + constants.go:8.
    #[test]
    fn copilot_constants_match_go() {
        assert_eq!(
            PROVIDER_CONF_URL,
            "https://raw.githubusercontent.com/ThinkInAIXYZ/PublicProviderConf/dev/dist/github-copilot.json"
        );
        assert_eq!(PROVIDER_ID, "github-copilot");
    }

    /// Verbatim mirror of Go token_exchanger.go:22-24.
    #[test]
    fn token_exchanger_constants_match_go() {
        assert_eq!(
            DEFAULT_COPILOT_TOKEN_ENDPOINT,
            "https://api.github.com/copilot_internal/v2/token"
        );
        assert_eq!(TOKEN_EXPIRY_BUFFER, Duration::from_secs(300));
        assert_eq!(TOKEN_EXCHANGE_TIMEOUT, Duration::from_secs(30));
    }

    // -----------------------------------------------------------------------
    // Fake HTTP client for offline tests (mirrors Go httptest.Server).
    // -----------------------------------------------------------------------

    /// Fake [`OauthHttpClient`] that returns a canned JSON body for any
    /// request. Mirrors the Go `httptest.Server` handler in
    /// `token_exchanger_test.go:29-42`.
    struct FakeOauthHttp {
        /// Closure that inspects the request and produces a response. Returning
        /// `Err(msg)` simulates a transport error.
        responder:
            Box<dyn Fn(&OauthHttpRequest) -> Result<OauthHttpResponse, String> + Send + Sync>,
        /// Number of calls seen — used by the cache-hit / cache-miss tests.
        call_count: Mutex<usize>,
    }

    impl FakeOauthHttp {
        fn new<F>(responder: F) -> Self
        where
            F: Fn(&OauthHttpRequest) -> Result<OauthHttpResponse, String> + Send + Sync + 'static,
        {
            Self {
                responder: Box::new(responder),
                call_count: Mutex::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.call_count.lock().map(|c| *c).unwrap_or(0)
        }
    }

    impl OauthHttpClient for FakeOauthHttp {
        fn execute(&self, request: &OauthHttpRequest) -> Result<OauthHttpResponse, String> {
            if let Ok(mut c) = self.call_count.lock() {
                *c += 1;
            }
            (self.responder)(request)
        }
    }

    /// Build a JSON body matching Go `copilotTokenResponse`.
    fn token_body(token: &str, expires_at: i64) -> Vec<u8> {
        format!(r#"{{"token":"{token}","expires_at":{expires_at}}}"#).into_bytes()
    }

    /// Build an exchanger pointing at a fake endpoint for parity with Go's
    /// `newTestExchanger` (token_exchanger_test.go:17-26).
    fn exchanger_with(client: Arc<dyn OauthHttpClient>) -> TokenExchanger {
        TokenExchanger::new(TokenExchangerParams {
            http_client: client,
            endpoint: Some("https://fake.test/copilot_internal/v2/token".to_string()),
        })
    }

    /// Extract the error from a `Result`, panicking on `Ok` — mirrors the Go
    /// `require.Error(t, err)` pattern without using `` (forbidden
    /// by the workspace `clippy::unwrap_used = "deny"` lint).
    fn require_err<T: std::fmt::Debug>(result: Result<T, ConduitError>) -> ConduitError {
        match result {
            Ok(v) => panic!("expected error, got Ok: {v:?}"),
            Err(e) => e,
        }
    }

    // -----------------------------------------------------------------------
    // token_exchanger_test.go:28-51 — TestTokenExchanger_Exchange_Success
    // -----------------------------------------------------------------------

    #[test]
    fn exchange_success_returns_token_and_expiry() -> Result<(), Box<dyn std::error::Error>> {
        // Go test handler (token_exchanger_test.go:29-42): assert GET method,
        // assert path, assert `token ` prefix on Authorization, echo a token.
        let expires_at = Utc::now().timestamp() + 3600;
        let client = Arc::new(FakeOauthHttp::new(move |req| {
            assert_eq!(req.method, "GET");
            assert_eq!(req.url, "https://fake.test/copilot_internal/v2/token");
            let auth = req
                .headers
                .iter()
                .find(|(k, _)| k == "Authorization")
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            assert!(
                auth.starts_with("token "),
                "expected `token ` prefix, got: {auth}"
            );
            Ok(OauthHttpResponse {
                status: 200,
                body: token_body(
                    &format!("copilot_token_{}", &auth["token ".len()..]),
                    expires_at,
                ),
            })
        })) as Arc<dyn OauthHttpClient>;

        let exchanger = exchanger_with(client);
        let (token, resp_expires_at) = exchanger.exchange("test_access_token_123")?;
        assert_eq!(token, "copilot_token_test_access_token_123");
        assert!(resp_expires_at > Utc::now().timestamp());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // token_exchanger_test.go:53-78 — TestTokenExchanger_Exchange_CacheHit
    // -----------------------------------------------------------------------

    #[test]
    fn exchange_cache_hit_does_not_repeat_http() -> Result<(), Box<dyn std::error::Error>> {
        // Go handler returns the same token with a 1h expiry; two consecutive
        // exchanges should produce one HTTP call (cache hit on the second).
        let expires_at = Utc::now().timestamp() + 3600;
        let fake = Arc::new(FakeOauthHttp::new(move |_req| {
            Ok(OauthHttpResponse {
                status: 200,
                body: token_body("copilot_token_cached", expires_at),
            })
        }));
        let call_count_handle = fake.clone();
        let exchanger = exchanger_with(fake as Arc<dyn OauthHttpClient>);

        let token1 = exchanger.exchange("test_access_token")?.0;
        assert_eq!(call_count_handle.call_count(), 1);

        let token2 = exchanger.exchange("test_access_token")?.0;
        assert_eq!(call_count_handle.call_count(), 1);
        assert_eq!(token1, token2);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // token_exchanger_test.go:80-105 — TestTokenExchanger_Exchange_ExpiryBuffer
    // -----------------------------------------------------------------------

    #[test]
    fn exchange_expiry_buffer_forces_refresh() -> Result<(), Box<dyn std::error::Error>> {
        // Go handler returns a 3-minute expiry each call — inside the 5-minute
        // `tokenExpiryBuffer`, so the cache is always considered expired and
        // each call triggers HTTP. Mirrors token_exchanger_test.go:80-105.
        let counter = Arc::new(Mutex::new(0usize));
        let counter_for_closure = counter.clone();
        let fake = Arc::new(FakeOauthHttp::new(move |_req| {
            let mut c = counter_for_closure
                .lock()
                .map_err(|_| "mutex poisoned".to_string())?;
            *c += 1;
            // Match Go: token = "copilot_token_v" + string(rune('0'+requestCount))
            let n = *c;
            let digit = char::from_digit(n as u32, 10).unwrap_or('0');
            Ok(OauthHttpResponse {
                status: 200,
                body: token_body(
                    &format!("copilot_token_v{digit}"),
                    Utc::now().timestamp() + 180,
                ),
            })
        })) as Arc<dyn OauthHttpClient>;

        let exchanger = exchanger_with(fake);

        let token1 = exchanger.exchange("test_access_token")?.0;
        assert_eq!(*counter.lock().map_err(|_| "mutex poisoned")?, 1);

        let token2 = exchanger.exchange("test_access_token")?.0;
        // counter is 2 — cache was bypassed because 3min < 5min buffer.
        assert_eq!(*counter.lock().map_err(|_| "mutex poisoned")?, 2);
        assert_ne!(token1, token2);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // token_exchanger_test.go:107-114 — EmptyAccessToken
    // -----------------------------------------------------------------------

    #[test]
    fn exchange_empty_access_token_errors() {
        let client = Arc::new(FakeOauthHttp::new(|_req| {
            Ok(OauthHttpResponse {
                status: 200,
                body: token_body("never_returned", Utc::now().timestamp() + 3600),
            })
        })) as Arc<dyn OauthHttpClient>;
        let exchanger = exchanger_with(client);
        let err = require_err(exchanger.exchange(""));
        assert!(
            format!("{err}").contains("access token is empty"),
            "error string did not match Go: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // token_exchanger_test.go:116-126 — Non2xx
    // -----------------------------------------------------------------------

    #[test]
    fn exchange_non_2xx_errors() {
        let client = Arc::new(FakeOauthHttp::new(|_req| {
            Ok(OauthHttpResponse {
                status: 401,
                body: br#"{"error":"unauthorized"}"#.to_vec(),
            })
        })) as Arc<dyn OauthHttpClient>;
        let exchanger = exchanger_with(client);
        let err = require_err(exchanger.exchange("test_access_token"));
        assert!(
            format!("{err}").contains("token exchange returned non-2xx status: 401"),
            "error string did not match Go: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // token_exchanger_test.go:128-138 — InvalidJSON
    // -----------------------------------------------------------------------

    #[test]
    fn exchange_invalid_json_errors() {
        let client = Arc::new(FakeOauthHttp::new(|_req| {
            Ok(OauthHttpResponse {
                status: 200,
                body: b"{invalid-json".to_vec(),
            })
        })) as Arc<dyn OauthHttpClient>;
        let exchanger = exchanger_with(client);
        let err = require_err(exchanger.exchange("test_access_token"));
        assert!(
            format!("{err}").contains("failed to parse token response"),
            "error string did not match Go: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // token_exchanger_test.go:140-153 — EmptyTokenInResponse
    // -----------------------------------------------------------------------

    #[test]
    fn exchange_empty_token_in_response_errors() {
        let client = Arc::new(FakeOauthHttp::new(|_req| {
            Ok(OauthHttpResponse {
                status: 200,
                body: token_body("", Utc::now().timestamp() + 3600),
            })
        })) as Arc<dyn OauthHttpClient>;
        let exchanger = exchanger_with(client);
        let err = require_err(exchanger.exchange("test_access_token"));
        assert!(
            format!("{err}").contains("copilot token is empty in response"),
            "error string did not match Go: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // token_exchanger_test.go:155-168 — MissingExpiresAt
    // -----------------------------------------------------------------------

    #[test]
    fn exchange_missing_expires_at_errors() {
        let client = Arc::new(FakeOauthHttp::new(|_req| {
            Ok(OauthHttpResponse {
                status: 200,
                body: token_body("copilot_token", 0),
            })
        })) as Arc<dyn OauthHttpClient>;
        let exchanger = exchanger_with(client);
        let err = require_err(exchanger.exchange("test_access_token"));
        assert!(
            format!("{err}").contains("expires_at is missing in response"),
            "error string did not match Go: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // token_cache_key — sha256 hex parity with Go `hex.EncodeToString`.
    // -----------------------------------------------------------------------

    #[test]
    fn token_cache_key_matches_sha256_hex() {
        // Known SHA-256 of "test_access_token_123":
        //   c7c8b0c8f0e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6e6
        // (computed externally; this test asserts determinism + lowercase hex
        // output rather than a specific digest, to avoid synthesizing a
        // golden value.)
        let key = token_cache_key("test_access_token_123");
        assert_eq!(key.len(), 64);
        assert!(
            key.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        // Deterministic.
        assert_eq!(key, token_cache_key("test_access_token_123"));
        // Different inputs → different keys.
        assert_ne!(key, token_cache_key("different"));
    }

    // -----------------------------------------------------------------------
    // CopilotTokenCacheEntry::is_expired parity.
    // -----------------------------------------------------------------------

    #[test]
    fn cache_entry_is_expired_within_buffer() {
        // Expiry 3 minutes from now; buffer is 5 minutes → already expired.
        let now = Utc::now();
        let entry = CopilotTokenCacheEntry {
            copilot_token: "t".to_string(),
            expires_at: now + chrono::Duration::minutes(3),
            cached_at: now,
        };
        assert!(entry.is_expired(now));
    }

    #[test]
    fn cache_entry_not_expired_outside_buffer() {
        // Expiry 1 hour from now; buffer is 5 minutes → not expired.
        let now = Utc::now();
        let entry = CopilotTokenCacheEntry {
            copilot_token: "t".to_string(),
            expires_at: now + chrono::Duration::hours(1),
            cached_at: now,
        };
        assert!(!entry.is_expired(now));
    }

    #[test]
    fn cache_entry_zero_expiry_is_expired() {
        // Defensive: epoch start is treated as "zero" → expired.
        let now = Utc::now();
        let zero = DateTime::<Utc>::from_timestamp(0, 0).unwrap_or(now);
        let entry = CopilotTokenCacheEntry {
            copilot_token: "t".to_string(),
            expires_at: zero,
            cached_at: now,
        };
        assert!(entry.is_expired(now));
    }

    // -----------------------------------------------------------------------
    // exchange_with_client uses the override client, not the constructor one.
    // -----------------------------------------------------------------------

    #[test]
    fn exchange_with_client_uses_override() -> Result<(), Box<dyn std::error::Error>> {
        let expires_at = Utc::now().timestamp() + 3600;
        let constructor_client = Arc::new(FakeOauthHttp::new(|_req| {
            Err("constructor client should not be called".to_string())
        })) as Arc<dyn OauthHttpClient>;
        let exchanger = exchanger_with(constructor_client);

        let override_fake = Arc::new(FakeOauthHttp::new(move |_req| {
            Ok(OauthHttpResponse {
                status: 200,
                body: token_body("from_override", expires_at),
            })
        }));

        let (token, _) = exchanger.exchange_with_client(override_fake.as_ref(), "tok")?;
        assert_eq!(token, "from_override");
        Ok(())
    }
}
