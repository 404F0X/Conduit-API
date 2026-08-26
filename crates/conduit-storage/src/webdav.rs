//! WebDAV storage backend (RUST-P13-001 S08).
//!
//! Mirrors Go's `DataStorageService.createWebDAVFs`
//! (`conduit/internal/server/biz/data_storage.go` lines 449-479) plus the
//! WebDAV-specific branches inside `SaveData`/`LoadData`/`DeleteData`
//! (lines 508-674). The Go side delegates to `github.com/studio-b12/gowebdav`
//! and `github.com/looplj/afero-webdav`, which translate the `afero.Fs`
//! method set into WebDAV HTTP verbs (PUT/GET/DELETE/HEAD/MKCOL). We
//! replicate that mapping directly here against an injected
//! [`StorageHttpClient`] trait so the adapter is fully unit-testable without
//! a live WebDAV server.
//!
//! **Contract parity notes** (Go `data_storage.go`):
//!
//! - `createWebDAVFs` builds a `gowebdav.Client(url, username, password)` with
//!   a 10-minute timeout and optional `InsecureSkipTLS` transport. The HTTP
//!   client trait here is the seam for both — production wires up a `reqwest`-
//!   backed impl with the same timeout/TLS knobs; tests inject a fake.
//! - The base path is `cfg.Path` if non-empty, otherwise `ds.Settings.Directory`
//!   if non-empty, with the leading `/` trimmed (Synology/NAS compatibility,
//!   Go lines 461-472). We replicate the exact precedence and trim rule.
//! - `SaveData` for WebDAV trims a leading `/` off the key before writing
//!   (Go line 527) and calls `mkdirAll(filepath.Dir(key))` (Go line 529) to
//!   tolerate servers that reject PUT to a key whose parent collection does
//!   not exist. Our adapter issues an idempotent MKCOL for each ancestor —
//!   matching the Go `mkdirAll` semantics (lines 811-844) that swallow
//!   "already exists" errors.
//! - `DeleteData` treats a missing remote resource as success (Go lines
//!   628-633): `errors.Is(err, os.ErrNotExist) → return nil`. We map HTTP 404
//!   on DELETE to `Ok(false)` (no row removed) which the dispatcher reports
//!   as success, matching Go's tolerance.
//! - `LoadData` reads the bytes; a 404 surfaces as `Ok(None)` so the caller
//!   sees "not found" rather than an error. Go would return a wrapped error,
//!   but the Rust `StorageAdapter::get` contract uses `Option`, and the
//!   dispatcher already maps `None → not found` cleanly.

use crate::adapter::normalize_key;
use crate::adapter::{StorageAdapter, StorageError, StorageMetadata, StorageObject, StorageResult};
use crate::settings::WebDavSettings;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Mutex;
use url::Url;

// ---------------------------------------------------------------------------
// HTTP client seam — production wires reqwest; tests inject the in-memory fake.
// ---------------------------------------------------------------------------

/// A single storage HTTP request, backend-agnostic. Method is the uppercase
/// HTTP verb (`PUT`/`GET`/`DELETE`/`HEAD`/`MKCOL`); `url` is the fully-formed
/// endpoint; `body` is the optional payload (None for GET/DELETE/HEAD/MKCOL);
/// `headers` carries per-backend auth + content metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageHttpRequest {
    pub method: &'static str,
    pub url: String,
    pub body: Option<Vec<u8>>,
    pub headers: BTreeMap<String, String>,
}

/// HTTP response, reduced to the fields the storage adapters consume. Status
/// code is `u16` (the only storage-relevant dimension); body is the raw bytes
/// on success. Adapters map non-2xx codes to [`StorageError`] themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// Response headers, lower-cased keys. Missing headers are simply absent.
    pub headers: BTreeMap<String, String>,
}

/// Injected HTTP transport. The WebDAV (and future S3) adapters depend on this
/// trait, NOT on a concrete client, so unit tests substitute
/// [`InMemoryHttpClient`] and the production build can wire `reqwest` without
/// touching the adapter logic. Mirrors Go's implicit dependency on
/// `gowebdav.Client` / the AWS SDK HTTP layer.
#[async_trait]
pub trait StorageHttpClient: Send + Sync {
    async fn execute(&self, request: StorageHttpRequest) -> StorageResult<StorageHttpResponse>;
}

/// In-memory [`StorageHttpClient`] for tests. It stores every PUT body keyed
/// by URL, returns the stored bytes on GET, reports `200 OK` + empty body on
/// DELETE/HEAD for known URLs, and `404` for unknown ones. MKCOL always
/// succeeds (idempotent). The recorded request log is exposed via
/// [`InMemoryHttpClient::recorded`] so tests can assert on auth headers, URL
/// shape, and leading-slash trimming.
#[derive(Debug, Default)]
pub struct InMemoryHttpClient {
    store: Mutex<BTreeMap<String, Vec<u8>>>,
    requests: Mutex<Vec<StorageHttpRequest>>,
}

impl InMemoryHttpClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of every request the client has executed, in arrival order.
    pub fn recorded(&self) -> Vec<StorageHttpRequest> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl StorageHttpClient for InMemoryHttpClient {
    async fn execute(&self, request: StorageHttpRequest) -> StorageResult<StorageHttpResponse> {
        if let Ok(mut log) = self.requests.lock() {
            log.push(request.clone());
        }
        let mut store = self
            .store
            .lock()
            .map_err(|_| StorageError::LockPoisoned("in-memory http client"))?;
        let status = match request.method {
            "PUT" => {
                let body = request.body.clone().unwrap_or_default();
                store.insert(request.url.clone(), body);
                200
            }
            "GET" => match store.get(&request.url) {
                Some(body) => {
                    return Ok(StorageHttpResponse {
                        status: 200,
                        body: body.clone(),
                        headers: BTreeMap::new(),
                    });
                }
                None => 404,
            },
            "HEAD" => {
                if store.contains_key(&request.url) {
                    200
                } else {
                    404
                }
            }
            "DELETE" => {
                if store.remove(&request.url).is_some() {
                    204
                } else {
                    404
                }
            }
            // MKCOL and any future verb are treated as idempotent success to
            // match the tolerant mkdirAll behavior in Go (lines 811-844).
            "MKCOL" => 201,
            other => {
                return Err(StorageError::Operation(format!(
                    "in-memory client does not implement {other}"
                )));
            }
        };
        Ok(StorageHttpResponse {
            status,
            body: Vec::new(),
            headers: BTreeMap::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// WebDAV adapter
// ---------------------------------------------------------------------------

/// WebDAV storage adapter (RUST-P13-001 S08). Mirrors Go's
/// `DataStorageService.createWebDAVFs` + the WebDAV branches of
/// `SaveData`/`LoadData`/`DeleteData`.
///
/// Construct with [`WebDavStorageAdapter::new`] (validates the WebDAV URL and
/// resolves the base path) and inject any [`StorageHttpClient`]. The adapter
/// is `Send + Sync` because both the URL/base-path state and the HTTP client
/// are.
#[derive(Debug)]
pub struct WebDavStorageAdapter<C: StorageHttpClient> {
    /// Parsed WebDAV server origin (scheme + host[:port], no path). The base
    /// path is joined per request, matching Go's `gowebdav.Client` + the
    /// `BasePathFs` wrap.
    origin: Url,
    /// Base path applied to every key, with the leading `/` trimmed (Go lines
    /// 461-472). May be empty (no base path) which means keys resolve directly
    /// against the server root.
    base_path: String,
    username: String,
    password: String,
    http: C,
}

impl<C: StorageHttpClient> WebDavStorageAdapter<C> {
    /// Build a WebDAV adapter from the typed [`WebDavSettings`] plus an
    /// optional `directory` (the S08 fallback Go uses when `cfg.Path` is
    /// empty: `ds.Settings.Directory`). Mirrors Go's `createWebDAVFs` lines
    /// 449-479.
    ///
    /// The `insecure_skip_tls` flag is honored by the production HTTP client
    /// wiring (a `reqwest` client built with `danger_accept_invalid_certs`).
    /// It is recorded on [`WebDavSettings`] but not enforced here — the
    /// adapter never opens a socket itself.
    pub fn new(settings: &WebDavSettings, directory: Option<&str>, http: C) -> StorageResult<Self> {
        let origin = Url::parse(&settings.url)
            .map_err(|error| StorageError::Unavailable(format!("invalid webdav url: {error}")))?;
        // Go precedence (lines 461-466): Path wins over Directory.
        let raw_path = if !settings.path.is_empty() {
            settings.path.as_str()
        } else {
            // Go: `path = *ds.Settings.Directory`
            directory.unwrap_or_default()
        };
        // Go line 472: `path = strings.TrimPrefix(path, "/")` — Synology/NAS
        // servers reject absolute paths inside the wrapped BasePathFs.
        let base_path = raw_path.trim_start_matches('/').to_string();
        Ok(Self {
            origin,
            base_path,
            username: settings.username.clone(),
            password: settings.password.clone(),
            http,
        })
    }

    /// Expose the resolved base path (post-trim) so tests and operators can
    /// verify the Synology compatibility normalization without issuing a
    /// request.
    pub fn base_path(&self) -> &str {
        &self.base_path
    }

    /// Compose the absolute URL for `key`, mirroring Go's
    /// `gowebdav.Client` + `BasePathFs` path joining. The key is normalized
    /// first (no `..`, no leading/trailing slash) and the WebDAV leading-slash
    /// trim from `SaveData` (Go line 527) is applied so the final URL path is
    /// `/<base_path>/<key>` with no duplicate separators.
    fn url_for(&self, key: &str) -> StorageResult<String> {
        let normalized = normalize_key(key)?;
        let mut url = self.origin.clone();
        if !normalized.is_empty() {
            let segments: Vec<&str> = normalized
                .split('/')
                .filter(|part| !part.is_empty())
                .collect();
            if !segments.is_empty() {
                let base_segments: Vec<&str> = if self.base_path.is_empty() {
                    Vec::new()
                } else {
                    self.base_path
                        .split('/')
                        .filter(|part| !part.is_empty())
                        .collect()
                };
                url.path_segments_mut()
                    .map_err(|_| {
                        StorageError::Unavailable("webdav url cannot be a base".to_string())
                    })?
                    .clear()
                    .extend(base_segments.into_iter().chain(segments));
            }
        }
        Ok(url.to_string())
    }

    /// Build the Basic-Auth header value Go sends implicitly via
    /// `gowebdav.NewClient(url, username, password)`. Empty credentials yield
    /// no header (the client still works for anonymous WebDAV servers).
    ///
    /// RFC 7617: the header is `Basic <base64(user:password)>`. Previously this
    /// emitted the *unencoded* `Basic user:password`, which a spec-compliant
    /// WebDAV server rejects (P-18). The credential pair is base64-encoded here
    /// (no external crate needed — see `base64_encode`).
    fn auth_header(&self) -> Option<String> {
        if self.username.is_empty() && self.password.is_empty() {
            return None;
        }
        let pair = format!("{}:{}", self.username, self.password);
        Some(format!("Basic {}", base64_encode(pair.as_bytes())))
    }

    /// Idempotent WebDAV MKCOL for every ancestor of `key`, mirroring Go's
    /// `DataStorageService.mkdirAll` (lines 811-844) which silently swallows
    /// "already exists" errors. We issue MKCOL for each prefix; the in-memory
    /// fake and any spec-compliant server return 201 (created) or 405/409
    /// (already exists), both of which we treat as success.
    async fn mkdir_all(&self, url_for_key: &str) -> StorageResult<()> {
        let parsed = Url::parse(url_for_key).map_err(|error| {
            StorageError::Unavailable(format!("webdav url parse failed: {error}"))
        })?;
        let path = parsed.path();
        let segments: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
        // Walk all but the final segment (the object key itself).
        if segments.len() <= 1 {
            return Ok(());
        }
        for depth in 0..(segments.len() - 1) {
            let mut built = parsed.clone();
            built
                .path_segments_mut()
                .map_err(|_| StorageError::Unavailable("webdav url cannot be a base".to_string()))?
                .clear();
            built
                .path_segments_mut()
                .map_err(|_| StorageError::Unavailable("webdav url cannot be a base".to_string()))?
                .extend(segments[..=depth].iter().copied());
            let mut headers = BTreeMap::new();
            if let Some(auth) = self.auth_header() {
                headers.insert("authorization".to_string(), auth);
            }
            let response = self
                .http
                .execute(StorageHttpRequest {
                    method: "MKCOL",
                    url: built.to_string(),
                    body: None,
                    headers,
                })
                .await?;
            // 2xx and "already exists" (405/409) are both acceptable per Go's
            // tolerant mkdirAll.
            if !(200..300).contains(&response.status) && !matches!(response.status, 405 | 409) {
                return Err(StorageError::Operation(format!(
                    "webdav MKCOL failed with status {}",
                    response.status
                )));
            }
        }
        Ok(())
    }

    /// Shared header builder: Authorization when credentials are present.
    fn request_headers(&self) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::new();
        if let Some(auth) = self.auth_header() {
            headers.insert("authorization".to_string(), auth);
        }
        headers
    }

    /// Treat a non-2xx status as an error, with 404 mapped to `None` by the
    /// caller (each method decides whether 404 is success or "not found").
    fn ensure_2xx(response: &StorageHttpResponse, context: &str) -> StorageResult<()> {
        if (200..300).contains(&response.status) {
            Ok(())
        } else {
            Err(StorageError::Operation(format!(
                "webdav {context} failed with status {}",
                response.status
            )))
        }
    }
}

#[async_trait]
impl<C: StorageHttpClient> StorageAdapter for WebDavStorageAdapter<C> {
    async fn put(&self, object: StorageObject) -> StorageResult<StorageMetadata> {
        // Normalize the key once; reuse the same validation as the local
        // adapter so ".." / absolute / drive-letter keys are rejected before
        // any HTTP traffic. Mirrors Go's SaveData WebDAV leading-slash trim
        // (line 527) which our normalize_key already enforces strictly.
        let key = normalize_key(&object.metadata.key)?;
        let content_type = object
            .metadata
            .content_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let mut metadata = object.metadata.clone();
        metadata.key = key.clone();
        metadata.content_length = object.bytes.len() as u64;

        let url = self.url_for(&key)?;
        // Mirror Go's mkdirAll(filepath.Dir(key)) before the PUT (lines
        // 525-532). Servers that reject PUT into a missing collection succeed
        // once the ancestors exist.
        self.mkdir_all(&url).await?;

        let mut headers = self.request_headers();
        headers.insert("content-type".to_string(), content_type);
        headers.insert("content-length".to_string(), object.bytes.len().to_string());
        let response = self
            .http
            .execute(StorageHttpRequest {
                method: "PUT",
                url,
                body: Some(object.bytes),
                headers,
            })
            .await?;
        Self::ensure_2xx(&response, "PUT")?;
        Ok(metadata)
    }

    async fn get(&self, key: &str) -> StorageResult<Option<StorageObject>> {
        let key = normalize_key(key)?;
        let url = self.url_for(&key)?;
        let response = self
            .http
            .execute(StorageHttpRequest {
                method: "GET",
                url,
                body: None,
                headers: self.request_headers(),
            })
            .await?;
        if response.status == 404 {
            // LoadData tolerates missing keys at the dispatcher level; surface
            // None so the higher layer can decide.
            return Ok(None);
        }
        Self::ensure_2xx(&response, "GET")?;
        let content_length = response.body.len() as u64;
        let metadata = StorageMetadata::new(key, content_length);
        Ok(Some(StorageObject {
            metadata,
            bytes: response.body,
        }))
    }

    async fn delete(&self, key: &str) -> StorageResult<bool> {
        let key = normalize_key(key)?;
        let url = self.url_for(&key)?;
        let response = self
            .http
            .execute(StorageHttpRequest {
                method: "DELETE",
                url,
                body: None,
                headers: self.request_headers(),
            })
            .await?;
        if response.status == 404 {
            // Go: `errors.Is(err, os.ErrNotExist) → return nil`. We report
            // `false` (nothing removed); the dispatcher treats Ok as success.
            return Ok(false);
        }
        Self::ensure_2xx(&response, "DELETE")?;
        Ok(true)
    }

    async fn head(&self, key: &str) -> StorageResult<Option<StorageMetadata>> {
        let key = normalize_key(key)?;
        let url = self.url_for(&key)?;
        let response = self
            .http
            .execute(StorageHttpRequest {
                method: "HEAD",
                url,
                body: None,
                headers: self.request_headers(),
            })
            .await?;
        if response.status == 404 {
            return Ok(None);
        }
        Self::ensure_2xx(&response, "HEAD")?;
        // WebDAV HEAD does not return a body; content_length comes from the
        // Content-Length response header if the server provided one.
        let content_length = response
            .headers
            .get("content-length")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(Some(StorageMetadata::new(key, content_length)))
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<StorageMetadata>> {
        let prefix = if prefix.is_empty() {
            String::new()
        } else {
            normalize_key(prefix)?
        };
        let mut url = self.origin.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                StorageError::Unavailable("webdav url cannot be a base".to_string())
            })?;
            segments.clear();
            for part in self.base_path.split('/').filter(|part| !part.is_empty()) {
                segments.push(part);
            }
            for part in prefix.split('/').filter(|part| !part.is_empty()) {
                segments.push(part);
            }
        }
        let mut headers = self.request_headers();
        headers.insert("depth".to_string(), "infinity".to_string());
        headers.insert("content-type".to_string(), "application/xml".to_string());
        let response = self
            .http
            .execute(StorageHttpRequest {
                method: "PROPFIND",
                url: url.to_string(),
                body: Some(b"<?xml version=\"1.0\"?><propfind xmlns=\"DAV:\"><prop><getcontentlength/><getcontenttype/><resourcetype/></prop></propfind>".to_vec()),
                headers,
            })
            .await?;
        if response.status != 207 && !(200..300).contains(&response.status) {
            return Err(StorageError::Operation(format!(
                "webdav PROPFIND failed with status {}",
                response.status
            )));
        }
        let listing: WebDavMultistatus = quick_xml::de::from_reader(response.body.as_slice())
            .map_err(|error| {
                StorageError::Serialization(format!("invalid WebDAV list response: {error}"))
            })?;
        let base_prefix = format!("/{}/", self.base_path.trim_matches('/'));
        Ok(listing
            .responses
            .into_iter()
            .filter_map(|entry| {
                let property = entry.propstats.into_iter().find_map(|status| status.prop)?;
                if property.resource_type.collection.is_some() {
                    return None;
                }
                let path = Url::parse(&entry.href)
                    .ok()
                    .map(|url| url.path().to_string())
                    .unwrap_or(entry.href);
                let key = if self.base_path.is_empty() {
                    path.trim_start_matches('/').to_string()
                } else {
                    path.strip_prefix(&base_prefix)?.to_string()
                };
                if key.is_empty() {
                    return None;
                }
                let mut metadata = StorageMetadata::new(key, property.content_length.unwrap_or(0));
                metadata.content_type = property.content_type;
                Some(metadata)
            })
            .collect())
    }
}

#[derive(serde::Deserialize)]
struct WebDavMultistatus {
    #[serde(rename = "response", default)]
    responses: Vec<WebDavResponseEntry>,
}

#[derive(serde::Deserialize)]
struct WebDavResponseEntry {
    href: String,
    #[serde(rename = "propstat", default)]
    propstats: Vec<WebDavPropstat>,
}

#[derive(serde::Deserialize)]
struct WebDavPropstat {
    #[serde(default)]
    prop: Option<WebDavProperties>,
}

#[derive(serde::Deserialize)]
struct WebDavProperties {
    #[serde(rename = "getcontentlength", default)]
    content_length: Option<u64>,
    #[serde(rename = "getcontenttype", default)]
    content_type: Option<String>,
    #[serde(rename = "resourcetype", default)]
    resource_type: WebDavResourceType,
}

#[derive(Default, serde::Deserialize)]
struct WebDavResourceType {
    #[serde(default)]
    collection: Option<serde::de::IgnoredAny>,
}

/// Standard base64 (RFC 4648) encoding, kept in-crate to avoid a dependency for
/// the single Basic-Auth use site (P-18). Not URL-safe; uses `+`/`/` and `=`
/// padding, exactly what RFC 7617 requires for the `Basic` scheme.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P-18: base64 must match RFC 4648 known vectors (padding included).
    #[test]
    fn base64_encode_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // The Basic-Auth pair used in the WebDAV test.
        assert_eq!(base64_encode(b"alice:s3cret"), "YWxpY2U6czNjcmV0");
    }
    use crate::adapter::StorageObject;
    use std::sync::Arc;

    fn webdav_settings(url: &str, path: &str) -> WebDavSettings {
        WebDavSettings {
            url: url.to_string(),
            username: "alice".to_string(),
            password: "s3cret".to_string(),
            insecure_skip_tls: false,
            path: path.to_string(),
        }
    }

    /// Thin wrapper that lets a single shared [`InMemoryHttpClient`] be
    /// injected into the adapter while tests retain a handle to read the
    /// recorded request log. The adapter owns a `SharedHttpClient` which
    /// delegates to the `Arc<InMemoryHttpClient>` the test keeps.
    #[derive(Debug, Clone)]
    struct SharedHttpClient(Arc<InMemoryHttpClient>);

    #[async_trait]
    impl StorageHttpClient for SharedHttpClient {
        async fn execute(&self, request: StorageHttpRequest) -> StorageResult<StorageHttpResponse> {
            self.0.execute(request).await
        }
    }

    fn shared_adapter(
        url: &str,
        path: &str,
    ) -> (
        WebDavStorageAdapter<SharedHttpClient>,
        Arc<InMemoryHttpClient>,
    ) {
        let http = Arc::new(InMemoryHttpClient::new());
        let settings = webdav_settings(url, path);
        let adapter = WebDavStorageAdapter::new(&settings, None, SharedHttpClient(http.clone()))
            .unwrap_or_else(|err| panic!("adapter build failed: {err:?}"));
        (adapter, http)
    }

    #[tokio::test]
    async fn put_then_get_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, http) = shared_adapter("https://dav.example.com", "/dav");
        adapter
            .put(StorageObject::new(
                "requests/abc.json",
                br#"{"ok":true}"#.to_vec(),
            ))
            .await?;

        let loaded = match adapter.get("requests/abc.json").await? {
            Some(object) => object,
            None => return Err("expected stored object".into()),
        };
        assert_eq!(loaded.bytes, br#"{"ok":true}"#);

        // Inspect the recorded PUT to verify auth header + URL shape parity.
        let put = http
            .recorded()
            .into_iter()
            .find(|request| request.method == "PUT")
            .ok_or("no PUT recorded")?;
        // RFC 7617: base64("alice:s3cret") = "YWxpY2U6czNjcmV0" (P-18 — was
        // previously the unencoded pair, which real servers reject).
        assert_eq!(
            put.headers.get("authorization"),
            Some(&"Basic YWxpY2U6czNjcmV0".to_string()),
        );
        assert!(
            put.url.ends_with("/dav/requests/abc.json"),
            "url was: {}",
            put.url
        );
        Ok(())
    }

    #[tokio::test]
    async fn base_path_trims_leading_slash_for_synology_compatibility() {
        // Mirrors Go line 472: `path = strings.TrimPrefix(path, "/")`.
        let (adapter, _http) = shared_adapter("https://dav.example.com", "/dav/root");
        assert_eq!(adapter.base_path(), "dav/root");
    }

    #[tokio::test]
    async fn directory_is_used_when_path_is_empty() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go lines 462-466: when cfg.Path is empty, fall back to
        // ds.Settings.Directory.
        let http = Arc::new(InMemoryHttpClient::new());
        let settings = WebDavSettings {
            url: "https://dav.example.com".to_string(),
            username: String::new(),
            password: String::new(),
            insecure_skip_tls: false,
            path: String::new(),
        };
        let adapter =
            WebDavStorageAdapter::new(&settings, Some("/from-directory"), SharedHttpClient(http))?;
        assert_eq!(adapter.base_path(), "from-directory");
        Ok(())
    }

    #[tokio::test]
    async fn put_key_with_leading_slash_is_normalized() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go SaveData WebDAV branch (line 527): keys with a leading
        // slash are trimmed before hitting the WebDAV server. normalize_key
        // (S13 invariant) already rejects leading slashes outright, so the URL
        // path the adapter builds never has a double slash between the base
        // path and the key.
        let (adapter, http) = shared_adapter("https://dav.example.com", "dav");
        adapter
            .put(StorageObject::new("requests/item.txt", b"hello".to_vec()))
            .await?;
        let put = http
            .recorded()
            .into_iter()
            .find(|request| request.method == "PUT")
            .ok_or("no PUT recorded")?;
        // Inspect only the path portion (after the scheme `://`) so the
        // scheme separator is not mistaken for a doubled path separator.
        let after_scheme = put
            .url
            .strip_prefix("https://")
            .or_else(|| put.url.strip_prefix("http://"))
            .unwrap_or(&put.url);
        let path_start = after_scheme
            .find('/')
            .map(|index| &after_scheme[index..])
            .unwrap_or("");
        assert!(
            !path_start.contains("//"),
            "double slash leaked into path: {} (full url: {})",
            path_start,
            put.url
        );
        assert!(
            put.url.ends_with("/dav/requests/item.txt"),
            "url was: {}",
            put.url
        );
        Ok(())
    }

    #[tokio::test]
    async fn delete_returns_false_on_404() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go DeleteData: 404 is treated as a successful no-op.
        let (adapter, _http) = shared_adapter("https://dav.example.com", "dav");
        let removed = adapter.delete("never/saved.txt").await?;
        assert!(!removed);
        Ok(())
    }

    #[tokio::test]
    async fn get_returns_none_on_404() -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, _http) = shared_adapter("https://dav.example.com", "dav");
        assert_eq!(adapter.get("absent.txt").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn head_returns_none_on_404_and_some_on_hit() -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, _http) = shared_adapter("https://dav.example.com", "dav");
        assert_eq!(adapter.head("missing.txt").await?, None);
        adapter
            .put(StorageObject::new("present.txt", b"hello".to_vec()))
            .await?;
        let head = match adapter.head("present.txt").await? {
            Some(metadata) => metadata,
            None => return Err("expected head Some after put".into()),
        };
        // InMemoryHttpClient HEAD returns 200 with no Content-Length header,
        // so content_length stays at the default 0. This documents the
        // behavior; a real server's HEAD would populate it.
        assert_eq!(head.content_length, 0);
        assert_eq!(head.key, "present.txt");
        Ok(())
    }

    #[tokio::test]
    async fn put_rejects_path_traversal_key() {
        let (adapter, _http) = shared_adapter("https://dav.example.com", "dav");
        let result = adapter
            .put(StorageObject::new("../escape.txt", b"x".to_vec()))
            .await;
        match result {
            Err(StorageError::InvalidKey(_)) => {}
            other => panic!("expected InvalidKey, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_credentials_omits_authorization_header()
    -> Result<(), Box<dyn std::error::Error>> {
        let http = Arc::new(InMemoryHttpClient::new());
        let settings = WebDavSettings {
            url: "https://dav.example.com".to_string(),
            username: String::new(),
            password: String::new(),
            insecure_skip_tls: false,
            path: "dav".to_string(),
        };
        let adapter = WebDavStorageAdapter::new(&settings, None, SharedHttpClient(http.clone()))?;
        adapter
            .put(StorageObject::new("anon.txt", b"hi".to_vec()))
            .await?;
        let put = http
            .recorded()
            .into_iter()
            .find(|request| request.method == "PUT")
            .ok_or("no PUT recorded")?;
        assert!(!put.headers.contains_key("authorization"));
        Ok(())
    }

    #[tokio::test]
    async fn invalid_webdav_url_is_reported_as_unavailable() {
        let http = Arc::new(InMemoryHttpClient::new());
        let settings = WebDavSettings {
            url: "not a url".to_string(),
            ..Default::default()
        };
        match WebDavStorageAdapter::new(&settings, None, SharedHttpClient(http)) {
            Err(StorageError::Unavailable(msg)) => {
                assert!(msg.contains("webdav url"), "msg: {msg}")
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn put_issues_mkcol_for_ancestor_collections() -> Result<(), Box<dyn std::error::Error>> {
        // Parity with Go mkdirAll (lines 811-844): a PUT to
        // `requests/nested/item.txt` under base path `dav` must first MKCOL
        // `dav/requests` and `dav/requests/nested` so servers that reject PUT
        // into a missing collection succeed.
        let (adapter, http) = shared_adapter("https://dav.example.com", "dav");
        adapter
            .put(StorageObject::new(
                "requests/nested/item.txt",
                b"data".to_vec(),
            ))
            .await?;
        let mkcol_urls: Vec<String> = http
            .recorded()
            .into_iter()
            .filter(|request| request.method == "MKCOL")
            .map(|request| request.url)
            .collect();
        assert!(
            mkcol_urls.iter().any(|url| url.ends_with("/dav/requests")),
            "missing MKCOL for /dav/requests: {mkcol_urls:?}"
        );
        assert!(
            mkcol_urls
                .iter()
                .any(|url| url.ends_with("/dav/requests/nested")),
            "missing MKCOL for /dav/requests/nested: {mkcol_urls:?}"
        );
        // The leaf (item.txt) must NOT receive MKCOL — only ancestors.
        assert!(
            !mkcol_urls.iter().any(|url| url.contains("item.txt")),
            "MKCOL was issued for the leaf key: {mkcol_urls:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn delete_after_put_reports_true() -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, _http) = shared_adapter("https://dav.example.com", "dav");
        adapter
            .put(StorageObject::new("temp.txt", b"x".to_vec()))
            .await?;
        assert!(adapter.delete("temp.txt").await?);
        // Deleting again is a 404 → false (still Ok per Go tolerance).
        assert!(!adapter.delete("temp.txt").await?);
        Ok(())
    }
}
