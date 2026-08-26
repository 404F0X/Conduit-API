//! HTTP client builder + upstream-error preservation. Mirrors the Go
//! `llm/httpclient/client.go` client construction semantics:
//!
//! - `NewHttpClient` / `NewHttpClientWithProxy`: build a client from options.
//! - `WithInsecureSkipVerify`: toggles TLS verification (Go sets
//!   `tls.Config{InsecureSkipVerify: true}`; reqwest uses
//!   `danger_accept_invalid_certs(true)`).
//! - `getProxyFunc`: resolves a proxy from `ProxyConfig` (Disabled / Environment
//!   / URL-with-optional-auth). reqwest handles `HTTP_PROXY`/`HTTPS_PROXY`/
//!   `NO_PROXY` natively when no proxy is configured, so the `Environment`
//!   variant maps to "no explicit proxy on the builder".
//! - Default transport: Go dials with `Timeout: 30s`, `KeepAlive: 30s`,
//!   `ForceAttemptHTTP2: true`, `MaxIdleConns: 100`, `IdleConnTimeout: 90s`,
//!   `TLSHandshakeTimeout: 10s`, `ExpectContinueTimeout: 1s`. reqwest's
//!   defaults are close; we expose the knobs that matter for parity.
//! - `WithProxy`: returns a new client preserving all other options — modelled
//!   as a builder re-derivation.

use std::time::Duration;

use reqwest::Client as ReqwestClient;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::http::decoder::mask_sensitive_headers;
use crate::model::HeaderMap;

/// Default cap on the size of an upstream error body we read into memory.
///
/// Go reads the entire body via `io.ReadAll` with no cap. To avoid
/// unbounded memory growth on huge upstream error/HTML pages, we cap at
/// 1 MiB. Bodies exceeding the cap are truncated and flagged. This is a
/// deliberate, documented divergence from Go.
pub const MAX_ERROR_BODY_BYTES: usize = 1024 * 1024;

/// Default connect timeout matching Go's `net.Dialer.Timeout` (30s).
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default connection pool idle timeout matching Go's
/// `http.Transport.IdleConnTimeout` (90s).
pub const DEFAULT_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Default per-request timeout. Go leaves this unset on `http.Client`
/// (no overall timeout); we keep it `None` by default to match. Channel
/// configuration can set it explicitly.
pub const DEFAULT_REQUEST_TIMEOUT: Option<Duration> = None;

/// Proxy type discriminator, mirroring Go's `ProxyType` string constants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// `ProxyTypeDisabled` — direct connection, never use a proxy.
    #[serde(alias = "DISABLED")]
    Disabled,
    /// `ProxyTypeEnvironment` — defer to reqwest's env handling
    /// (`HTTP_PROXY`/`HTTPS_PROXY`/`NO_PROXY`). This is the default and
    /// matches Go's `http.ProxyFromEnvironment`.
    #[default]
    #[serde(alias = "ENVIRONMENT")]
    Environment,
    /// `ProxyTypeURL` — use a configured proxy URL (http/https/socks5),
    /// with optional basic auth.
    #[serde(alias = "URL")]
    Url,
}

/// Proxy configuration, mirroring Go's `ProxyConfig` struct
/// (`llm/httpclient/proxy.go`). Go's json tags are single lowercase words
/// (`type`, `url`, `username`, `password`); `rename_all = "camelCase"` produces
/// identical output for these one-word fields. The Rust field is named `mode`
/// (since `type` is a reserved keyword) and mapped to Go's `json:"type"` via an
/// explicit `#[serde(rename = "type")]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    /// Proxy selection mode. Go field `Type` with tag `json:"type"`.
    #[serde(rename = "type", default)]
    pub mode: ProxyMode,
    /// Proxy URL. Required when `mode = Url`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Optional username for proxy basic auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Optional password for proxy basic auth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl ProxyConfig {
    /// Build a `ProxyConfig` that uses environment variables.
    pub fn from_env() -> Self {
        Self {
            mode: ProxyMode::Environment,
            url: None,
            username: None,
            password: None,
        }
    }

    /// Build a `ProxyConfig` that disables all proxying.
    pub fn disabled() -> Self {
        Self {
            mode: ProxyMode::Disabled,
            url: None,
            username: None,
            password: None,
        }
    }

    /// Build a `ProxyConfig` pointing at a fixed URL. Returns `Err` if the URL
    /// is not a valid absolute URI (mirrors Go's eager `url.Parse`).
    pub fn from_url(url: impl Into<String>) -> Result<Self, ClientBuildError> {
        let raw = url.into();
        let parsed = Url::parse(&raw).map_err(|source| ClientBuildError::InvalidProxyUrl {
            url: raw.clone(),
            source,
        })?;

        if parsed.host().is_none() {
            return Err(ClientBuildError::MissingProxyHost(raw));
        }

        Ok(Self {
            mode: ProxyMode::Url,
            url: Some(raw),
            username: None,
            password: None,
        })
    }

    /// Attach basic-auth credentials. Only meaningful for `mode = Url`.
    pub fn with_auth(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self.password = Some(password.into());
        self
    }

    /// Convert the configuration to a `reqwest::Proxy`, or `None` when the
    /// environment should be used (reqwest applies env proxies by default).
    ///
    /// Returns an error for the `Url` variant if the stored URL is empty or
    /// has become invalid — mirroring Go's `getProxyFunc` returning an error
    /// closure for an empty/invalid URL.
    pub(crate) fn to_reqwest_proxy(&self) -> Result<Option<reqwest::Proxy>, ClientBuildError> {
        match self.mode {
            ProxyMode::Disabled => {
                // A custom interceptor that always returns `None` forces a
                // direct connection and overrides any env proxies, matching
                // Go's `ProxyTypeDisabled` returning `nil, nil`.
                Ok(Some(reqwest::Proxy::custom(|_url: &url::Url| {
                    Option::<url::Url>::None
                })))
            }
            ProxyMode::Environment => {
                // reqwest reads HTTP_PROXY/HTTPS_PROXY/NO_PROXY natively; we
                // deliberately do NOT attach a proxy so env vars apply.
                Ok(None)
            }
            ProxyMode::Url => {
                let raw = self
                    .url
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .ok_or(ClientBuildError::MissingProxyUrl)?;
                let mut parsed =
                    Url::parse(raw).map_err(|source| ClientBuildError::InvalidProxyUrl {
                        url: raw.to_string(),
                        source,
                    })?;

                // Apply basic auth onto the parsed URL — reqwest reads it from
                // `Url::username`/`password`. Matches Go setting
                // `proxyURL.User = url.UserPassword(...)`.
                if let (Some(user), Some(pass)) =
                    (self.username.as_deref(), self.password.as_deref())
                {
                    let _ = parsed.set_username(user);
                    let _ = parsed.set_password(Some(pass));
                }

                reqwest::Proxy::all(parsed)
                    .map(Some)
                    .map_err(ClientBuildError::BuildProxy)
            }
        }
    }
}

/// Transport-level timing configuration. The defaults mirror Go's
/// `http.Transport` / `net.Dialer` settings used in `NewHttpClientWithProxy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportConfig {
    /// TCP connect timeout. Go: `net.Dialer.Timeout` (30s).
    pub connect_timeout: Duration,
    /// Idle connection pool timeout. Go: `IdleConnTimeout` (90s).
    pub pool_idle_timeout: Duration,
    /// Per-request timeout. Go leaves this unset on `http.Client` (no limit).
    pub request_timeout: Option<Duration>,
    /// Whether to negotiate HTTP/2. Go: `ForceAttemptHTTP2: true`.
    /// reqwest negotiates h2 by default over TLS; we keep it on.
    pub http2: bool,
    /// Skip TLS certificate verification. Go:
    /// `tls.Config{InsecureSkipVerify: true}`. reqwest:
    /// `danger_accept_invalid_certs(true)`.
    pub insecure_skip_verify: bool,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            pool_idle_timeout: DEFAULT_POOL_IDLE_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            http2: true,
            insecure_skip_verify: false,
        }
    }
}

/// Error raised while constructing the HTTP client.
#[derive(Debug, Error)]
pub enum ClientBuildError {
    /// The configured proxy URL is not a valid URL.
    #[error("invalid proxy URL {url:?}: {source}")]
    InvalidProxyUrl {
        url: String,
        #[source]
        source: url::ParseError,
    },
    /// The configured proxy URL has no host component.
    #[error("proxy URL {0:?} is missing a host")]
    MissingProxyHost(String),
    /// `ProxyMode::Url` was selected without providing a URL.
    #[error("proxy URL is required when mode is 'url'")]
    MissingProxyUrl,
    /// reqwest failed to build the proxy or client.
    #[error("failed to build reqwest client: {0}")]
    BuildProxy(#[source] reqwest::Error),
    /// reqwest failed to build the underlying client.
    #[error("failed to build reqwest client: {0}")]
    BuildClient(#[from] reqwest::Error),
    /// The configured base URL is not absolute.
    #[error("base URL {0:?} is not absolute")]
    InvalidBaseUrl(String),
}

/// A builder for `reqwest::Client` that mirrors the Go `HttpClient` option
/// pipeline.
///
/// State is observable so tests can assert on it without poking the opaque
/// `reqwest::Client`. `build()` is the only method that produces the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpClientBuilder {
    proxy: ProxyConfig,
    transport: TransportConfig,
    default_headers: HeaderMap,
    base_url: Option<Url>,
}

impl Default for HttpClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClientBuilder {
    /// Create a builder with Go-default transport settings and
    /// environment-proxy resolution.
    pub fn new() -> Self {
        Self {
            proxy: ProxyConfig::from_env(),
            transport: TransportConfig::default(),
            default_headers: HeaderMap::new(),
            base_url: None,
        }
    }

    /// Set the proxy configuration. Replaces any previous value.
    pub fn proxy(mut self, proxy: ProxyConfig) -> Self {
        self.proxy = proxy;
        self
    }

    /// Disable TLS certificate verification (Go `WithInsecureSkipVerify`).
    pub fn insecure_skip_verify(mut self, skip: bool) -> Self {
        self.transport.insecure_skip_verify = skip;
        self
    }

    /// Override the connect timeout (Go `net.Dialer.Timeout`).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.transport.connect_timeout = timeout;
        self
    }

    /// Override the idle-pool timeout (Go `IdleConnTimeout`).
    pub fn pool_idle_timeout(mut self, timeout: Duration) -> Self {
        self.transport.pool_idle_timeout = timeout;
        self
    }

    /// Override the per-request timeout. `None` means no overall timeout,
    /// matching Go's `http.Client` default.
    pub fn request_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.transport.request_timeout = timeout;
        self
    }

    /// Toggle HTTP/2 negotiation (Go `ForceAttemptHTTP2`).
    pub fn http2(mut self, enable: bool) -> Self {
        self.transport.http2 = enable;
        self
    }

    /// Add a default header applied to every outbound request. Matches Go's
    /// request-builder `WithHeader`. Replaces an existing value for the key.
    pub fn default_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.default_headers.insert(key.into(), value.into());
        self
    }

    /// Merge a set of default headers.
    pub fn default_headers(mut self, headers: HeaderMap) -> Self {
        for (k, v) in headers {
            self.default_headers.insert(k, v);
        }
        self
    }

    /// Set the base URL used to resolve relative request URLs. Mirrors the
    /// per-channel `base_url` join behavior expected by S12.
    pub fn base_url(mut self, base: impl Into<String>) -> Result<Self, ClientBuildError> {
        let raw = base.into();
        let parsed = Url::parse(&raw).map_err(|_| ClientBuildError::InvalidBaseUrl(raw))?;
        if parsed.host().is_none() {
            return Err(ClientBuildError::InvalidBaseUrl(parsed.to_string()));
        }
        self.base_url = Some(parsed);
        Ok(self)
    }

    /// Resolve a (possibly relative) request path against the configured base
    /// URL. Absolute URLs are returned unchanged. If no base URL is set, the
    /// input is returned as-is.
    pub fn resolve_url(&self, request_url: &str) -> Result<String, ClientBuildError> {
        // If the input is already an absolute URL (has a host), hand it back
        // verbatim. Do not consume the parsed value — we only need the host
        // check.
        let is_absolute = Url::parse(request_url)
            .ok()
            .and_then(|u| u.host_str().map(|_| ()))
            .is_some();
        if is_absolute {
            return Ok(request_url.to_string());
        }

        let Some(base) = &self.base_url else {
            return Ok(request_url.to_string());
        };

        base.join(request_url)
            .map(|u| u.to_string())
            .map_err(|_| ClientBuildError::InvalidBaseUrl(request_url.to_string()))
    }

    /// Snapshot the proxy configuration (for assertions).
    pub fn proxy_config(&self) -> &ProxyConfig {
        &self.proxy
    }

    /// Snapshot the transport configuration (for assertions).
    pub fn transport_config(&self) -> &TransportConfig {
        &self.transport
    }

    /// Snapshot the default headers (for assertions).
    pub fn default_headers_map(&self) -> &HeaderMap {
        &self.default_headers
    }

    /// Snapshot the base URL (for assertions).
    pub fn base_url_ref(&self) -> Option<&Url> {
        self.base_url.as_ref()
    }

    /// Produce a `reqwest::Client`. Mirrors the Go
    /// `NewHttpClientWithProxy`/`NewHttpClient` pipeline.
    pub fn build(self) -> Result<ReqwestClient, ClientBuildError> {
        let mut builder = ReqwestClient::builder()
            .connect_timeout(self.transport.connect_timeout)
            .pool_idle_timeout(Some(self.transport.pool_idle_timeout))
            .danger_accept_invalid_certs(self.transport.insecure_skip_verify);

        if !self.transport.http2 {
            builder = builder.http1_only();
        }

        if let Some(timeout) = self.transport.request_timeout {
            builder = builder.timeout(timeout);
        }

        if let Some(proxy) = self.proxy.to_reqwest_proxy()? {
            builder = builder.proxy(proxy);
        }

        if !self.default_headers.is_empty() {
            let mut headers = reqwest::header::HeaderMap::new();
            for (k, v) in &self.default_headers {
                // HeaderName/HeaderValue construction failures fall back to
                // skipping the entry rather than failing the whole client
                // build; invalid header keys/values from config should not
                // hard-crash startup.
                let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_bytes()) else {
                    continue;
                };
                let Ok(value) = reqwest::header::HeaderValue::from_str(v) else {
                    continue;
                };
                headers.insert(name, value);
            }
            builder = builder.default_headers(headers);
        }

        builder.build().map_err(ClientBuildError::BuildClient)
    }

    /// Returns a new builder that swaps the proxy configuration while keeping
    /// every other option. Mirrors Go's `HttpClient.WithProxy`.
    pub fn with_proxy(self, proxy: ProxyConfig) -> Self {
        Self {
            proxy,
            transport: self.transport,
            default_headers: self.default_headers,
            base_url: self.base_url,
        }
    }
}

/// Error returned for non-2xx upstream responses. Mirrors Go's
/// `httpclient.Error` struct (status + body preservation, S15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamError {
    /// HTTP method of the failed request.
    pub method: String,
    /// Resolved URL of the failed request.
    pub url: String,
    /// Numeric HTTP status code (e.g. 429, 500).
    pub status_code: u16,
    /// HTTP status line text (e.g. "429 Too Many Requests").
    pub status: String,
    /// Response body, truncated to [`MAX_ERROR_BODY_BYTES`]. `truncated` is
    /// `true` when the original body exceeded the cap.
    pub body: Vec<u8>,
    /// Whether the body was truncated due to the size cap.
    pub truncated: bool,
    /// Response headers with sensitive values masked via
    /// [`mask_sensitive_headers`].
    pub headers: HeaderMap,
}

impl UpstreamError {
    /// Read an upstream non-2xx response to completion, applying the
    /// configured body cap. Sensitive headers are masked.
    ///
    /// Matches the Go `Do`/`DoStream` paths that construct an `Error` on
    /// `StatusCode >= 400`. Go reads with `io.ReadAll` (no cap); we cap at
    /// [`MAX_ERROR_BODY_BYTES`] and flag truncation.
    pub async fn from_response(
        method: &str,
        url: &str,
        status_code: u16,
        status_text: &str,
        raw_body: &[u8],
        raw_headers: &HeaderMap,
    ) -> Self {
        let (body, truncated) = if raw_body.len() > MAX_ERROR_BODY_BYTES {
            (raw_body[..MAX_ERROR_BODY_BYTES].to_vec(), true)
        } else {
            (raw_body.to_vec(), false)
        };

        Self {
            method: method.to_string(),
            url: url.to_string(),
            status_code,
            status: status_text.to_string(),
            body,
            truncated,
            headers: mask_sensitive_headers(raw_headers),
        }
    }
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Mirrors Go's `Error.Error()`: "<METHOD> - <URL> with status <STATUS>".
        write!(
            formatter,
            "{} - {} with status {}",
            self.method, self.url, self.status
        )
    }
}

impl std::error::Error for UpstreamError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // ---- S04: proxy config ----

    #[test]
    fn proxy_from_env_is_default_mode() {
        let cfg = ProxyConfig::from_env();
        assert_eq!(cfg.mode, ProxyMode::Environment);
        assert!(cfg.url.is_none());
    }

    #[test]
    fn proxy_disabled_forces_direct_connection() -> Result<(), Box<dyn std::error::Error>> {
        let cfg = ProxyConfig::disabled();
        assert_eq!(cfg.mode, ProxyMode::Disabled);
        // The Disabled mode must yield a proxy that reqwest accepts.
        assert!(cfg.to_reqwest_proxy()?.is_some());
        Ok(())
    }

    #[test]
    fn proxy_from_url_parses_and_attaches_auth() -> Result<(), Box<dyn std::error::Error>> {
        let cfg = ProxyConfig::from_url("socks5://localhost:1080")?.with_auth("alice", "secret");
        assert_eq!(cfg.mode, ProxyMode::Url);
        assert_eq!(cfg.url.as_deref(), Some("socks5://localhost:1080"));
        assert_eq!(cfg.username.as_deref(), Some("alice"));
        assert_eq!(cfg.password.as_deref(), Some("secret"));

        let proxy = cfg.to_reqwest_proxy()?.ok_or("expected a proxy")?;
        // reqwest::Proxy is opaque; assert it exists rather than its internals.
        let _ = format!("{proxy:?}");
        Ok(())
    }

    #[test]
    fn proxy_from_url_rejects_missing_host() {
        // No unwrap_err(): workspace lints forbid `unwrap*`.
        match ProxyConfig::from_url("socks5://") {
            Err(ClientBuildError::MissingProxyHost(_)) => {}
            other => panic!("expected MissingProxyHost, got {other:?}"),
        }
    }

    #[test]
    fn proxy_url_mode_requires_a_url() {
        let cfg = ProxyConfig {
            mode: ProxyMode::Url,
            url: None,
            username: None,
            password: None,
        };
        // No unwrap_err(): workspace lints forbid `unwrap*`.
        match cfg.to_reqwest_proxy() {
            Err(ClientBuildError::MissingProxyUrl) => {}
            other => panic!("expected MissingProxyUrl, got {other:?}"),
        }
    }

    #[test]
    fn proxy_environment_mode_yields_no_explicit_proxy() -> Result<(), Box<dyn std::error::Error>> {
        // Environment mode must NOT attach a proxy so reqwest falls back to
        // HTTP_PROXY/HTTPS_PROXY/NO_PROXY natively.
        assert!(ProxyConfig::from_env().to_reqwest_proxy()?.is_none());
        Ok(())
    }

    // ---- S05: insecure skip verify ----

    #[test]
    fn insecure_flag_propagates_into_transport_config() {
        let builder = HttpClientBuilder::new().insecure_skip_verify(true);
        assert!(builder.transport_config().insecure_skip_verify);
    }

    #[test]
    fn builder_with_insecure_can_build_a_client() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go's `TestNewHttpClient_WithInsecureSkipVerify_PreservesDefaultTransportSettings`.
        let client = HttpClientBuilder::new()
            .insecure_skip_verify(true)
            .build()?;
        let _ = format!("{client:?}"); // opaque; just assert it constructed.
        Ok(())
    }

    // ---- S06: default transport ----

    #[test]
    fn default_transport_matches_go_defaults() {
        // TransportConfig is Copy; no clone() needed.
        let transport = *HttpClientBuilder::new().transport_config();
        assert_eq!(transport.connect_timeout, Duration::from_secs(30));
        assert_eq!(transport.pool_idle_timeout, Duration::from_secs(90));
        assert!(transport.request_timeout.is_none(), "Go leaves this unset");
        assert!(transport.http2, "Go ForceAttemptHTTP2 = true");
        assert!(!transport.insecure_skip_verify);
    }

    #[test]
    fn transport_overrides_propagate() {
        let builder = HttpClientBuilder::new()
            .connect_timeout(Duration::from_secs(5))
            .pool_idle_timeout(Duration::from_secs(10))
            .request_timeout(Some(Duration::from_secs(60)))
            .http2(false);
        let transport = builder.transport_config();
        assert_eq!(transport.connect_timeout, Duration::from_secs(5));
        assert_eq!(transport.pool_idle_timeout, Duration::from_secs(10));
        assert_eq!(transport.request_timeout, Some(Duration::from_secs(60)));
        assert!(!transport.http2);
    }

    // ---- S12: per-channel builder details (base_url join + headers) ----

    #[test]
    fn base_url_resolves_relative_paths() -> Result<(), Box<dyn std::error::Error>> {
        // Absolute-path refs replace the base path entirely.
        let builder = HttpClientBuilder::new().base_url("https://api.example.com/v1")?;
        assert_eq!(
            builder.resolve_url("/chat/completions")?,
            "https://api.example.com/chat/completions"
        );

        // Per RFC 3986, a relative ref against a non-directory base (no
        // trailing slash) replaces the last segment. To append under `/v1/`,
        // the channel config must use a trailing slash.
        let builder_dir = HttpClientBuilder::new().base_url("https://api.example.com/v1/")?;
        assert_eq!(
            builder_dir.resolve_url("embeddings")?,
            "https://api.example.com/v1/embeddings"
        );
        Ok(())
    }

    #[test]
    fn base_url_leaves_absolute_urls_untouched() -> Result<(), Box<dyn std::error::Error>> {
        let builder = HttpClientBuilder::new().base_url("https://api.example.com")?;
        assert_eq!(
            builder.resolve_url("https://other.example.com/foo")?,
            "https://other.example.com/foo"
        );
        Ok(())
    }

    #[test]
    fn resolve_url_without_base_returns_input_as_is() -> Result<(), Box<dyn std::error::Error>> {
        let builder = HttpClientBuilder::new();
        assert_eq!(builder.resolve_url("/path")?, "/path");
        Ok(())
    }

    #[test]
    fn base_url_rejects_non_absolute() {
        // No unwrap_err(): workspace lints forbid `unwrap*`.
        match HttpClientBuilder::new().base_url("/not/absolute") {
            Err(ClientBuildError::InvalidBaseUrl(_)) => {}
            other => panic!("expected InvalidBaseUrl, got {other:?}"),
        }
    }

    #[test]
    fn default_headers_are_stored_and_propagated() {
        let mut headers = BTreeMap::new();
        headers.insert("X-Channel".to_string(), "openai".to_string());
        let builder = HttpClientBuilder::new()
            .default_header("X-Trace-Id", "abc")
            .default_headers(headers);
        let map = builder.default_headers_map();
        assert_eq!(map.get("X-Trace-Id").map(String::as_str), Some("abc"));
        assert_eq!(map.get("X-Channel").map(String::as_str), Some("openai"));
    }

    #[test]
    fn builder_with_proxy_returns_new_builder_preserving_other_options() {
        // Mirrors Go's `HttpClient.WithProxy` semantics.
        let original = HttpClientBuilder::new()
            .insecure_skip_verify(true)
            .connect_timeout(Duration::from_secs(15));
        let new = original.clone().with_proxy(ProxyConfig::disabled());
        assert_eq!(new.proxy_config().mode, ProxyMode::Disabled);
        // Other options preserved.
        assert!(new.transport_config().insecure_skip_verify);
        assert_eq!(
            new.transport_config().connect_timeout,
            Duration::from_secs(15)
        );
    }

    #[test]
    fn build_succeeds_with_proxy_and_headers() -> Result<(), Box<dyn std::error::Error>> {
        let client = HttpClientBuilder::new()
            .proxy(ProxyConfig::from_url("http://localhost:3128")?)
            .insecure_skip_verify(false)
            .default_header("User-Agent", "conduit/1.0")
            .base_url("https://api.example.com")?
            .build()?;
        let _ = format!("{client:?}");
        Ok(())
    }

    // ---- S15: upstream non-2xx preservation ----

    #[tokio::test]
    async fn upstream_error_preserves_status_body_and_masks_headers()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut headers = BTreeMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Authorization".to_string(), "Bearer secret".to_string());
        headers.insert("Retry-After".to_string(), "30".to_string());

        let err = UpstreamError::from_response(
            "POST",
            "https://api.example.com/v1/chat",
            429,
            "429 Too Many Requests",
            b"{\"error\":\"rate limited\"}",
            &headers,
        )
        .await;

        assert_eq!(err.status_code, 429);
        assert_eq!(err.status, "429 Too Many Requests");
        assert_eq!(err.method, "POST");
        assert_eq!(err.url, "https://api.example.com/v1/chat");
        assert_eq!(err.body, b"{\"error\":\"rate limited\"}".to_vec());
        assert!(!err.truncated);

        // Sensitive header masked.
        assert_eq!(
            err.headers.get("Authorization").map(String::as_str),
            Some("[redacted]")
        );
        // Non-sensitive header preserved.
        assert_eq!(
            err.headers.get("Retry-After").map(String::as_str),
            Some("30")
        );

        // Display mirrors Go's Error.Error().
        assert_eq!(
            err.to_string(),
            "POST - https://api.example.com/v1/chat with status 429 Too Many Requests"
        );
        Ok(())
    }

    #[tokio::test]
    async fn upstream_error_truncates_oversized_body() {
        let big = vec![b'x'; MAX_ERROR_BODY_BYTES + 1];
        let headers = BTreeMap::new();
        let err = UpstreamError::from_response(
            "GET",
            "https://api.example.com/big",
            500,
            "500 Internal Server Error",
            &big,
            &headers,
        )
        .await;

        assert!(err.truncated);
        assert_eq!(err.body.len(), MAX_ERROR_BODY_BYTES);
    }
}
