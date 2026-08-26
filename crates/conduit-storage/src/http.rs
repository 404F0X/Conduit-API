//! Production `reqwest`-backed [`StorageHttpClient`] (RUST-P13-001 S08/S09).
//!
//! Mirrors Go's `createWebDAVFs` HTTP transport setup
//! (`conduit/internal/server/biz/data_storage.go` lines 449-457):
//! Go builds a `gowebdav.Client` whose underlying `http.Client` carries a
//! 10-minute timeout (`client.SetTimeout(time.Minute * 10)`, line 451) and,
//! when `cfg.InsecureSkipTLS` is set, an `http.Transport` with
//! `TLSClientConfig: &tls.Config{InsecureSkipVerify: true}` (lines 452-457).
//!
//! The same transport shape is the natural production backend for the S3/GCS
//! adapters too: both ultimately send signed HTTP requests via the
//! [`crate::webdav::StorageHttpClient`] seam, and the Go side reuses the
//! stdlib `http.Client` (with similar timeout/TLS knobs set by the AWS / GCP
//! SDKs) for them.
//!
//! [`ReqwestStorageHttpClient`] is therefore the shared production enabler for
//! WebDAV/S3/GCS: construct one with the desired timeout + TLS policy and pass
//! it to [`crate::backend::build_webdav_backend`] /
//! [`crate::backend::build_s3_backend`] /
//! [`crate::backend::build_gcs_backend`]. For unit tests, inject the
//! in-memory fake ([`crate::webdav::InMemoryHttpClient`]) instead — this impl
//! is a thin adapter and the trait's behavior is already covered by those
//! fake-driven tests.

use crate::StorageError;
use crate::webdav::{StorageHttpClient, StorageHttpRequest, StorageHttpResponse};
use async_trait::async_trait;
use reqwest::{Client, Method};
use std::time::Duration;

/// Default WebDAV request timeout, matching Go's `time.Minute * 10`
/// (`data_storage.go` line 451).
pub const DEFAULT_WEBDAV_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// `reqwest`-backed [`StorageHttpClient`]. Construct via
/// [`ReqwestStorageHttpClient::new`] (which builds the underlying
/// [`reqwest::Client`] honoring Go's timeout + `InsecureSkipTLS` policy) or
/// [`ReqwestStorageHttpClient::with_client`] (caller-supplied client for
/// advanced configuration / connection pooling).
///
/// The client is `Send + Sync + Clone` (a thin wrapper around `reqwest::Client`,
/// which is itself an `Arc` internally) so it can be shared across adapters or
/// re-used for every storage backend in a process.
#[derive(Debug, Clone)]
pub struct ReqwestStorageHttpClient {
    client: Client,
}

impl ReqwestStorageHttpClient {
    /// Build a client mirroring Go's `createWebDAVFs` HTTP transport policy
    /// (`data_storage.go` lines 449-457):
    ///
    /// - `timeout` is the per-request ceiling; if `None`, the Go default of
    ///   10 minutes is used (`DEFAULT_WEBDAV_TIMEOUT`).
    /// - When `insecure_skip_tls` is true the underlying TLS verifier is
    ///   disabled (`danger_accept_invalid_certs(true)`), matching Go's
    ///   `tls.Config{InsecureSkipVerify: true}`. **This deliberately bypasses
    ///   certificate validation — only enable it for trusted internal NAS /
    ///   self-hosted WebDAV servers, exactly as the Go side warns.**
    pub fn new(timeout: Option<Duration>, insecure_skip_tls: bool) -> Result<Self, StorageError> {
        let timeout = timeout.unwrap_or(DEFAULT_WEBDAV_TIMEOUT);
        let mut builder = Client::builder()
            .timeout(timeout)
            .danger_accept_invalid_certs(insecure_skip_tls);
        // The workspace `reqwest` dep uses `rustls-tls`; configure the rustls
        // provider explicitly is not required (rustls is default-constructed).
        // Suppress unused-mut if rustls features ever change the builder shape.
        let _ = &mut builder;
        let client = builder
            .build()
            .map_err(|error| StorageError::Unavailable(format!("reqwest build failed: {error}")))?;
        Ok(Self { client })
    }

    /// Wrap a caller-supplied [`reqwest::Client`]. Use this when the caller
    /// has already configured a shared client (e.g. with a connection pool or
    /// a custom redirect/cookie policy). The caller remains responsible for
    /// the timeout/TLS configuration matching Go's contract.
    pub fn with_client(client: Client) -> Self {
        Self { client }
    }

    /// Inspectable form of [`ReqwestStorageHttpClient::build_request`] used by
    /// unit tests to assert on method/URL/header translation WITHOUT issuing a
    /// network call. Returns a fully-formed `reqwest::RequestBuilder` which
    /// tests can inspect via `RequestBuilder::try_clone` / build into a
    /// `reqwest::Request` for header + method assertions.
    ///
    /// Kept separate from the async `execute` path so production never pays
    /// for the indirection and so tests can call it without a runtime.
    pub(crate) fn build_request(
        &self,
        request: &StorageHttpRequest,
    ) -> Result<reqwest::RequestBuilder, StorageError> {
        // reqwest's `Method::from_bytes` accepts arbitrary uppercase verbs,
        // including WebDAV's `MKCOL`. The Go side sends the literal verb from
        // the storage request, so we forward `request.method` verbatim.
        let method = Method::from_bytes(request.method.as_bytes()).map_err(|error| {
            StorageError::Operation(format!("invalid http method {:?}: {error}", request.method))
        })?;
        let mut builder = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        Ok(builder)
    }
}

#[async_trait]
impl StorageHttpClient for ReqwestStorageHttpClient {
    async fn execute(
        &self,
        request: StorageHttpRequest,
    ) -> Result<StorageHttpResponse, StorageError> {
        let builder = self.build_request(&request)?;
        let response = builder
            .send()
            .await
            .map_err(|error| StorageError::Unavailable(format!("reqwest send failed: {error}")))?;
        let status = response.status().as_u16();
        // Collect every response header into a lower-cased-key map so the
        // adapter can read `content-length` / `content-type` without caring
        // about reqwest's `HeaderMap` casing rules. The Go side reads headers
        // via `http.Header.Get` which is canonically case-insensitive; our
        // adapter does a direct lowercase-key lookup, so we MUST lower-case
        // the names here.
        let mut headers = std::collections::BTreeMap::new();
        for (name, value) in response.headers().iter() {
            let name_lower = name.as_str().to_ascii_lowercase();
            if let Ok(value_str) = value.to_str() {
                headers.insert(name_lower, value_str.to_string());
            }
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| {
                StorageError::Unavailable(format!("reqwest body read failed: {error}"))
            })?
            .to_vec();
        Ok(StorageHttpResponse {
            status,
            body,
            headers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Build a `reqwest::Request` from a `StorageHttpRequest` and return it
    /// for inspection. Mirrors what `execute` does, minus the network send.
    fn render_request(
        client: &ReqwestStorageHttpClient,
        storage_request: &StorageHttpRequest,
    ) -> Result<reqwest::Request, Box<dyn std::error::Error>> {
        let builder = client.build_request(storage_request)?;
        Ok(builder.build()?)
    }

    /// Translation parity: method, URL, and headers from a `StorageHttpRequest`
    /// land on the corresponding `reqwest::Request` fields verbatim. We verify
    /// this WITHOUT a server by inspecting the built `reqwest::Request`.
    #[test]
    fn build_request_translates_method_url_and_headers() -> Result<(), Box<dyn std::error::Error>> {
        let client = ReqwestStorageHttpClient::new(Some(Duration::from_secs(30)), false)?;
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "Basic alice:s3cret".to_string(),
        );
        headers.insert("content-type".to_string(), "application/json".to_string());
        let request = StorageHttpRequest {
            method: "PUT",
            url: "https://dav.example.com/base/key.json".to_string(),
            body: Some(br#"{"x":1}"#.to_vec()),
            headers,
        };
        let rendered = render_request(&client, &request)?;
        assert_eq!(rendered.method(), reqwest::Method::PUT);
        assert_eq!(
            rendered.url().as_str(),
            "https://dav.example.com/base/key.json"
        );
        assert_eq!(
            rendered
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Basic alice:s3cret")
        );
        assert_eq!(
            rendered
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        // Body is a bytes::Bytes payload when set via `.body(Vec<u8>)`.
        assert!(rendered.body().is_some());
        Ok(())
    }

    /// MKCOL (and any other uppercase verb storage adapters emit) must round
    /// trip through `Method::from_bytes`. This guards the WebDAV mkdirAll
    /// path which issues MKCOL against ancestor collections.
    #[test]
    fn build_request_accepts_mkcol_verb() -> Result<(), Box<dyn std::error::Error>> {
        let client = ReqwestStorageHttpClient::new(Some(Duration::from_secs(5)), true)?;
        let request = StorageHttpRequest {
            method: "MKCOL",
            url: "https://dav.example.com/base/sub".to_string(),
            body: None,
            headers: BTreeMap::new(),
        };
        let rendered = render_request(&client, &request)?;
        assert_eq!(rendered.method().as_str(), "MKCOL");
        assert!(rendered.body().is_none());
        Ok(())
    }

    /// GET / DELETE / HEAD with no body must produce a request with no body
    /// (so reqwest doesn't add a Content-Length / Transfer-Encoding header
    /// the storage server doesn't expect). Mirrors Go's `http.Client` which
    /// omits the body for non-payload verbs.
    #[test]
    fn build_request_omits_body_for_get() -> Result<(), Box<dyn std::error::Error>> {
        let client = ReqwestStorageHttpClient::new(None, false)?;
        let request = StorageHttpRequest {
            method: "GET",
            url: "https://dav.example.com/base/key.json".to_string(),
            body: None,
            headers: BTreeMap::new(),
        };
        let rendered = render_request(&client, &request)?;
        assert_eq!(rendered.method(), reqwest::Method::GET);
        assert!(rendered.body().is_none());
        Ok(())
    }

    /// An invalid HTTP verb surfaces as a `StorageError::Operation` rather
    /// than panicking. This protects production from a malformed verb slipping
    /// through a future adapter change.
    #[test]
    fn build_request_rejects_invalid_method() -> Result<(), Box<dyn std::error::Error>> {
        let client = ReqwestStorageHttpClient::new(None, false)?;
        let request = StorageHttpRequest {
            method: "NOT A VERB",
            url: "https://dav.example.com/".to_string(),
            body: None,
            headers: BTreeMap::new(),
        };
        match client.build_request(&request) {
            Err(StorageError::Operation(msg)) => {
                assert!(msg.contains("invalid http method"), "msg was: {msg}");
            }
            other => return Err(format!("expected Operation error, got {other:?}").into()),
        }
        Ok(())
    }

    /// `new(Some(timeout), false)` builds without error when TLS verification
    /// is kept (the default safe path).
    #[test]
    fn new_with_default_timeout_builds() -> Result<(), Box<dyn std::error::Error>> {
        let _client = ReqwestStorageHttpClient::new(None, false)?;
        // If TLS feature/redirect setup were broken this would fail to build.
        Ok(())
    }

    /// `new(_, true)` succeeds and configures the permissive TLS verifier.
    /// We can't directly observe the verifier from outside reqwest, but
    /// successful construction with `insecure_skip_tls = true` is the
    /// observable contract (Go builds the same client unconditionally).
    #[test]
    fn new_with_insecure_skip_tls_builds() -> Result<(), Box<dyn std::error::Error>> {
        let _client = ReqwestStorageHttpClient::new(None, true)?;
        Ok(())
    }
}
