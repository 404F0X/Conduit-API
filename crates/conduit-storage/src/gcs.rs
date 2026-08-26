//! GCS storage backend (RUST-P13-001 S07) with a testable signing seam.
//!
//! Mirrors the Go GCS path inside `DataStorageService`:
//!
//! - `createGcsFs` (`conduit/internal/server/biz/data_storage.go` lines
//!   422-446) parses the GCP service-account credential JSON via
//!   `google.CredentialsFromJSON(ctx, []byte(cred), storage.ScopeFullControl)`,
//!   builds a `cloud.google.com/go/storage` client, wraps it with
//!   `gcsfs.NewGcsFSFromClient`, and finally wraps THAT in
//!   `afero.NewBasePathFs(fs, gcsConfig.BucketName)` so the bucket is the base
//!   path prefix for every key.
//! - `SaveData` / `LoadData` / `DeleteData` (lines 508-674) for `TypeGcs` all
//!   fall through the same generic file-system branch as S3/WebDAV: they
//!   `afero.WriteFile` / `afero.ReadFile` / `fs.Remove` through the wrapped Fs.
//!   GCS gets no special key trimming (no `isS3PathStyle` branch), so the key
//!   flows straight through. Our [`normalize_key`] (S13) already rejects
//!   leading slashes and `..`, which is a strict superset of what Go does.
//! - `DeleteData` treats a missing object as success
//!   (`errors.Is(err, os.ErrNotExist) → return nil`, lines 628-633): we map
//!   HTTP 404 on DELETE to `Ok(false)` (no row removed), which the dispatcher
//!   reports as success — matching Go's tolerance.
//!
//! ## URL shape
//!
//! GCS exposes two HTTP APIs: the JSON API
//! (`https://storage.googleapis.com/storage/v1/b/<bucket>/o/<key>`) and the XML
//! / S3-compatible API (`https://storage.googleapis.com/<bucket>/<key>`). The
//! Go `cloud.google.com/go/storage` client uses the JSON API internally, but
//! for our adapter the **XML-style URL** `<host>/<bucket>/<key>` is the right
//! shape because:
//!   1. it matches the S3 path-style URL exactly, so GCS-as-S3-backdoor and
//!      S3-compatible stores share the same URL builder logic;
//!   2. the JSON API requires URL-escaping the object name into the query
//!      path segment (`o/<url-encoded-key>`), which adds a fragile encoding
//!      layer that the Go SDK owns and we would have to mirror byte-for-byte;
//!   3. the load-bearing contract for this slice is the *signing seam* + key
//!      handling + 404 tolerance, all of which transport cleanly through the
//!      XML-style URL.
//!
//! The `host` defaults to `https://storage.googleapis.com` (the GCS public
//! endpoint). Go's `storage.NewClient` targets the same host by default.
//!
//! ## Signing seam
//!
//! Real GCP service-account JWT signing (OAuth 2.0 access token minting via
//! `google.CredentialsFromJSON` + a self-signed JWT) is **deferred** — mirrors
//! exactly how the S3 adapter (S06) deferred real SigV4 signing. The adapter
//! depends on the [`GcsSigner`] trait, NOT on a concrete signer, so URL
//! construction + key handling + HTTP transport are fully unit-testable now
//! via [`InMemoryHttpClient`] + [`StaticSigner`] / [`RecordingSigner`] fakes.
//! A production build wires a real signer that mints an `Authorization: Bearer
//! <access-token>` header from the service-account JSON; until then
//! [`DeferredGcsSigner`] surfaces [`StorageError::Operation`] with a clear
//! "not yet implemented" message so callers can detect the not-yet-wired state
//! instead of silently sending unsigned traffic that GCS would reject with
//! 401.

use crate::adapter::normalize_key;
use crate::adapter::{StorageAdapter, StorageError, StorageMetadata, StorageObject, StorageResult};
use crate::settings::GcsSettings;
use crate::webdav::{StorageHttpClient, StorageHttpRequest, StorageHttpResponse};
use async_trait::async_trait;
use std::collections::BTreeMap;
use url::Url;

// ---------------------------------------------------------------------------
// Signer seam.
// ---------------------------------------------------------------------------

/// Signature input handed to [`GcsSigner::sign`]. GCS OAuth signing is
/// stateless: the same `Authorization: Bearer <token>` header is valid for
/// every verb + URL until the token expires. We still hand the method + URL to
/// the signer so a future implementation that moves to per-request signing
/// (e.g. V4 signed URLs for downloads) has the pieces it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcsSigningRequest<'a> {
    pub method: &'a str,
    pub url: &'a str,
}

/// Input to [`GcsSigner::presign`]: everything needed to build a GOOG4 signed
/// URL for one object.
pub struct GcsPresignRequest<'a> {
    pub method: &'a str,
    /// Scheme + host origin, e.g. `https://storage.googleapis.com` (no path).
    pub origin: &'a str,
    /// Host portion only, e.g. `storage.googleapis.com` (the signed `host`).
    pub host: &'a str,
    pub bucket: &'a str,
    /// Normalized object key (no leading slash).
    pub object: &'a str,
    pub ttl_seconds: u64,
}

/// Injected GCS signing strategy. Production wires a real signer that mints an
/// OAuth 2.0 access token from the service-account JSON (`gcs.credential`);
/// tests inject [`StaticSigner`] (returns a fixed header set) or
/// [`RecordingSigner`] (records the signing request and returns a canned
/// header).
///
/// Like the S3 [`crate::s3::S3Signer`], the signer returns a
/// `BTreeMap<String, String>` (lower-cased header names) rather than a full
/// `http::HeaderMap` so the adapter stays free of an `http` crate dependency.
#[async_trait]
pub trait GcsSigner: Send + Sync {
    /// Produce the auth headers (typically a single
    /// `authorization: Bearer <token>`) for the request. Returning
    /// [`StorageError::Operation`] with a "not yet implemented" message is the
    /// contract for "real GCS signing not yet wired" so the dispatcher can
    /// surface the not-yet-ported state without panicking.
    async fn sign(&self, request: GcsSigningRequest<'_>)
    -> StorageResult<BTreeMap<String, String>>;

    /// Produce a GOOG4-RSA-SHA256 pre-signed URL for the object described by
    /// `request`. Default: [`StorageError::Unsupported`] — the deferred/test
    /// signers cannot sign URLs. [`ServiceAccountGcsSigner`] implements the real
    /// algorithm (P-10).
    async fn presign(&self, _request: GcsPresignRequest<'_>) -> StorageResult<String> {
        Err(StorageError::Unsupported)
    }
}

/// Placeholder signer that always returns [`StorageError::Operation`]. Used by
/// the dispatcher so GCS *configuration* is wired (the adapter builds, the URL
/// shape is computed) but real signed requests fail loudly with a clear "not
/// yet implemented" message rather than silently sending unsigned traffic that
/// GCS would reject with 401.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeferredGcsSigner;

#[async_trait]
impl GcsSigner for DeferredGcsSigner {
    async fn sign(
        &self,
        _request: GcsSigningRequest<'_>,
    ) -> StorageResult<BTreeMap<String, String>> {
        Err(StorageError::Operation(
            "gcs signing not yet implemented (RUST-P13-001 S07 remaining)".to_string(),
        ))
    }
}

/// Test signer that always returns a fixed header set. The headers are merged
/// onto the outgoing request after any adapter-supplied metadata headers, so
/// tests can assert on auth shape without computing a real OAuth token.
#[derive(Debug, Clone)]
pub struct StaticSigner {
    pub headers: BTreeMap<String, String>,
}

impl StaticSigner {
    pub fn new(headers: BTreeMap<String, String>) -> Self {
        Self { headers }
    }
}

#[async_trait]
impl GcsSigner for StaticSigner {
    async fn sign(
        &self,
        _request: GcsSigningRequest<'_>,
    ) -> StorageResult<BTreeMap<String, String>> {
        Ok(self.headers.clone())
    }
}

/// Test signer that records every [`GcsSigningRequest`] it sees and forwards
/// it to an inner signer (defaulting to [`StaticSigner`] with a single
/// `authorization` header). Tests use this to assert the "signer-is-invoked"
/// contract: method + URL both flow through unchanged.
#[derive(Debug)]
pub struct RecordingSigner {
    inner: StaticSigner,
    requests: std::sync::Mutex<Vec<RecordingEntry>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingEntry {
    pub method: String,
    pub url: String,
}

impl Default for RecordingSigner {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingSigner {
    /// Build a recorder whose canned response is a single
    /// `authorization: Bearer ya29.test-token` header so the request still
    /// flows through the in-memory HTTP client cleanly.
    pub fn new() -> Self {
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "Bearer ya29.test-token".to_string(),
        );
        Self {
            inner: StaticSigner::new(headers),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Snapshot of every signing call, in arrival order.
    pub fn recorded(&self) -> Vec<RecordingEntry> {
        self.requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }
}

#[async_trait]
impl GcsSigner for RecordingSigner {
    async fn sign(
        &self,
        request: GcsSigningRequest<'_>,
    ) -> StorageResult<BTreeMap<String, String>> {
        if let Ok(mut log) = self.requests.lock() {
            log.push(RecordingEntry {
                method: request.method.to_string(),
                url: request.url.to_string(),
            });
        }
        self.inner.sign(request).await
    }
}

#[derive(Debug, serde::Deserialize)]
struct ServiceAccountCredential {
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

#[derive(Debug, serde::Serialize)]
struct ServiceAccountClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

#[derive(Debug, serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default = "default_expires_in")]
    expires_in: i64,
}

fn default_expires_in() -> i64 {
    3600
}

#[derive(Debug, Clone)]
struct CachedToken {
    value: String,
    expires_at: i64,
}

/// OAuth 2.0 service-account signer used by the production GCS backend.
#[derive(Debug)]
pub struct ServiceAccountGcsSigner {
    credential: ServiceAccountCredential,
    client: reqwest::Client,
    cached: tokio::sync::Mutex<Option<CachedToken>>,
}

impl ServiceAccountGcsSigner {
    pub fn new(settings: &GcsSettings) -> StorageResult<Self> {
        let credential: ServiceAccountCredential = serde_json::from_str(&settings.credential)
            .map_err(|error| {
                StorageError::Unavailable(format!(
                    "invalid gcs service-account credential: {error}"
                ))
            })?;
        if credential.client_email.is_empty() || credential.private_key.is_empty() {
            return Err(StorageError::Unavailable(
                "gcs credential requires client_email and private_key".to_string(),
            ));
        }
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| StorageError::Unavailable(format!("gcs token client: {error}")))?;
        Ok(Self {
            credential,
            client,
            cached: tokio::sync::Mutex::new(None),
        })
    }

    async fn access_token(&self) -> StorageResult<String> {
        let now = chrono::Utc::now().timestamp();
        let mut cached = self.cached.lock().await;
        if let Some(token) = cached.as_ref()
            && token.expires_at > now + 60
        {
            return Ok(token.value.clone());
        }

        let claims = ServiceAccountClaims {
            iss: &self.credential.client_email,
            scope: "https://www.googleapis.com/auth/devstorage.read_write",
            aud: &self.credential.token_uri,
            iat: now,
            exp: now + 3600,
        };
        let assertion = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_rsa_pem(self.credential.private_key.as_bytes())
                .map_err(|error| {
                    StorageError::Unavailable(format!("invalid gcs private key: {error}"))
                })?,
        )
        .map_err(|error| StorageError::Operation(format!("gcs JWT signing failed: {error}")))?;
        let response = self
            .client
            .post(&self.credential.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|error| {
                StorageError::Operation(format!("gcs token exchange failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(StorageError::Operation(format!(
                "gcs token exchange failed with status {status}: {body}"
            )));
        }
        let token: TokenResponse = response.json().await.map_err(|error| {
            StorageError::Operation(format!("invalid gcs token response: {error}"))
        })?;
        *cached = Some(CachedToken {
            value: token.access_token.clone(),
            expires_at: now + token.expires_in,
        });
        Ok(token.access_token)
    }
}

#[async_trait]
impl GcsSigner for ServiceAccountGcsSigner {
    async fn sign(
        &self,
        _request: GcsSigningRequest<'_>,
    ) -> StorageResult<BTreeMap<String, String>> {
        let token = self.access_token().await?;
        Ok(BTreeMap::from([(
            "authorization".to_string(),
            format!("Bearer {token}"),
        )]))
    }

    async fn presign(&self, request: GcsPresignRequest<'_>) -> StorageResult<String> {
        goog4_signed_url(
            &self.credential.client_email,
            &self.credential.private_key,
            request,
            chrono::Utc::now(),
        )
    }
}

// ---------------------------------------------------------------------------
// GOOG4-RSA-SHA256 pre-signed URLs (Google's V4 signing scheme, the GCS
// analogue of AWS SigV4). Spec:
// <https://cloud.google.com/storage/docs/authentication/signatures>.
//
// The deterministic URL construction (canonical request, string-to-sign,
// percent-encoding, hashing) is unit-tested below. The RSA signature itself is
// produced by `jsonwebtoken` (the same primitive the OAuth path already uses)
// and is round-trip verifiable, but the emitted URL has NOT been exercised
// against a live GCS endpoint — there is no wired caller yet (P-10). Kept
// spec-faithful so wiring a caller later is a drop-in.
// ---------------------------------------------------------------------------

/// Lower-case hex encoding.
fn goog4_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// SHA-256 of `data` as lower-case hex (the canonical-request digest).
fn goog4_sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    goog4_hex(&hasher.finalize())
}

/// RFC 3986 percent-encoding. Unreserved = `A-Za-z0-9-._~`. When `keep_slash`
/// is set, `/` is left literal (canonical URI path); otherwise it is `%2F`
/// (query-parameter values, e.g. the credential's `/` separators).
fn goog4_encode(input: &str, keep_slash: bool) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        let unreserved = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~');
        if unreserved || (keep_slash && b == b'/') {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Decode URL-safe base64 without padding (the shape `jsonwebtoken::crypto::sign`
/// returns) into raw bytes.
fn goog4_base64url_decode(input: &str) -> StorageResult<Vec<u8>> {
    fn sextet(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &c in input.as_bytes() {
        let v = sextet(c)
            .ok_or_else(|| StorageError::Operation("invalid base64url in signature".to_string()))?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// The five sorted `X-Goog-*` query parameters (signature excluded). Keys are
/// already in ASCII-sorted order; values are RFC-3986 encoded.
fn goog4_canonical_query(credential: &str, x_goog_date: &str, ttl_seconds: u64) -> String {
    format!(
        "X-Goog-Algorithm=GOOG4-RSA-SHA256\
         &X-Goog-Credential={cred}\
         &X-Goog-Date={date}\
         &X-Goog-Expires={ttl}\
         &X-Goog-SignedHeaders=host",
        cred = goog4_encode(credential, false),
        date = x_goog_date,
        ttl = ttl_seconds,
    )
}

/// The canonical request (SigV4-shaped): method, URI, query, the single
/// `host` header, signed-headers list, then the `UNSIGNED-PAYLOAD` marker.
fn goog4_canonical_request(
    method: &str,
    canonical_uri: &str,
    canonical_query: &str,
    host: &str,
) -> String {
    format!("{method}\n{canonical_uri}\n{canonical_query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD")
}

/// Build the full GOOG4 pre-signed URL. `now` is injected for deterministic
/// tests (production passes `Utc::now()`).
fn goog4_signed_url(
    client_email: &str,
    private_key_pem: &str,
    request: GcsPresignRequest<'_>,
    now: chrono::DateTime<chrono::Utc>,
) -> StorageResult<String> {
    let x_goog_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let credential_scope = format!("{date_stamp}/auto/storage/goog4_request");
    let credential = format!("{client_email}/{credential_scope}");

    let canonical_query = goog4_canonical_query(&credential, &x_goog_date, request.ttl_seconds);
    let canonical_uri = format!("/{}/{}", request.bucket, goog4_encode(request.object, true));
    let canonical_request = goog4_canonical_request(
        request.method,
        &canonical_uri,
        &canonical_query,
        request.host,
    );
    let string_to_sign = format!(
        "GOOG4-RSA-SHA256\n{x_goog_date}\n{credential_scope}\n{hash}",
        hash = goog4_sha256_hex(canonical_request.as_bytes()),
    );

    let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .map_err(|error| StorageError::Operation(format!("invalid gcs private key: {error}")))?;
    let signature_b64 = jsonwebtoken::crypto::sign(
        string_to_sign.as_bytes(),
        &key,
        jsonwebtoken::Algorithm::RS256,
    )
    .map_err(|error| StorageError::Operation(format!("gcs presign RSA signing failed: {error}")))?;
    let signature = goog4_hex(&goog4_base64url_decode(&signature_b64)?);

    Ok(format!(
        "{origin}{canonical_uri}?{canonical_query}&X-Goog-Signature={signature}",
        origin = request.origin.trim_end_matches('/'),
    ))
}

// ---------------------------------------------------------------------------
// Adapter.
// ---------------------------------------------------------------------------

/// Default GCS public endpoint. `cloud.google.com/go/storage.NewClient`
/// targets this host when no custom endpoint is configured.
const DEFAULT_GCS_HOST: &str = "https://storage.googleapis.com";

/// GCS storage adapter (RUST-P13-001 S07). Mirrors Go's `createGcsFs`
/// (`data_storage.go` lines 422-446) plus the GCS branches of
/// `SaveData`/`LoadData`/`DeleteData` (lines 508-674). GCS shares the same
/// generic file-system I/O branch as S3/WebDAV, so the key flows through
/// unchanged (Go applies no special trimming for GCS).
///
/// Construct with [`GcsStorageAdapter::new`] (validates the bucket + host
/// origin). Inject any [`StorageHttpClient`] and [`GcsSigner`]; the adapter is
/// `Send + Sync` because all of its state is.
///
/// The adapter does NOT implement real GCP OAuth/JWT signing — that is the
/// remaining S07 work. Until a production signer lands, the dispatcher wires
/// [`DeferredGcsSigner`] so requests fail with a clear message instead of
/// sending unsigned traffic that GCS would reject with 401.
#[derive(Debug)]
pub struct GcsStorageAdapter<C: StorageHttpClient, S: GcsSigner> {
    bucket: String,
    /// Parsed host origin (scheme + host[:port], no path). Defaults to
    /// `https://storage.googleapis.com`. Used as the base for URL construction
    /// (`<origin>/<bucket>/<key>`).
    origin: Url,
    http: C,
    signer: S,
}

impl<C: StorageHttpClient, S: GcsSigner> GcsStorageAdapter<C, S> {
    /// Build a GCS adapter from the typed [`GcsSettings`] plus injected HTTP
    /// transport and signer. Mirrors Go's `createGcsFs` (lines 422-446).
    ///
    /// The origin host is always `https://storage.googleapis.com` because Go's
    /// `storage.NewClient` uses that default and `objects.GCS` has no
    /// `endpoint` field to override it. We still parse it once here (rather
    /// than hard-coding a `Url` constant) so the [`url_for`] path-building
    /// logic has a stable base to clone from, and so a future custom-endpoint
    /// extension (e.g. for a GCS-emulator) only needs to widen this resolution.
    pub fn new(settings: &GcsSettings, http: C, signer: S) -> StorageResult<Self> {
        if settings.bucket_name.is_empty() {
            return Err(StorageError::Unavailable(
                "gcs bucketName not configured".to_string(),
            ));
        }

        let origin = Url::parse(DEFAULT_GCS_HOST).map_err(|error| {
            StorageError::Unavailable(format!("invalid gcs default host: {error}"))
        })?;

        // Defensive: the origin must carry a host so URL composition is well
        // defined. `Url::parse` rejects host-less inputs, but we assert again
        // so a future refactor cannot silently regress.
        if origin.host().is_none() {
            return Err(StorageError::Unavailable(format!(
                "gcs origin has no host: {origin}"
            )));
        }

        Ok(Self {
            bucket: settings.bucket_name.clone(),
            origin,
            http,
            signer,
        })
    }

    /// The bucket name the adapter writes to. Exposed for parity assertions.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Compose the fully-qualified GCS URL for `key`. GCS uses the XML-style
    /// path layout `<host>/<bucket>/<key>` (see module docs for why the JSON
    /// API URL shape is NOT used here). This matches the S3 path-style URL
    /// exactly and is the shape the Go `cloud.google.com/go/storage` client
    /// effectively resolves to for media download/upload.
    ///
    /// The key is normalized first via [`normalize_key`] (no `..`, no
    /// leading/trailing slash, no backslashes), which is a strict superset of
    /// the S13 containment invariant. Go applies no special trimming for GCS
    /// keys (no `isS3PathStyle` branch), so the normalized key is the exact
    /// object name GCS sees.
    fn url_for(&self, key: &str) -> StorageResult<String> {
        let normalized = normalize_key(key)?;
        let mut url = self.origin.clone();
        let endpoint_segments: Vec<String> = collect_path_segments(&url);
        {
            let mut path_builder = url.path_segments_mut().map_err(|_| {
                StorageError::Unavailable("gcs origin cannot be a base".to_string())
            })?;
            path_builder.clear();
            for segment in &endpoint_segments {
                path_builder.push(segment);
            }
            path_builder.push(&self.bucket);
            for part in normalized.split('/').filter(|segment| !segment.is_empty()) {
                path_builder.push(part);
            }
        }
        Ok(url.to_string())
    }

    /// Sign `request` then merge the resulting auth headers with the metadata
    /// headers the adapter already prepared. Auth headers win on conflict so
    /// the signer can stamp `authorization` over any default.
    async fn sign_and_merge(
        &self,
        method: &str,
        url: &str,
        mut headers: BTreeMap<String, String>,
    ) -> StorageResult<BTreeMap<String, String>> {
        let signed = self.signer.sign(GcsSigningRequest { method, url }).await?;
        for (name, value) in signed {
            headers.insert(name, value);
        }
        Ok(headers)
    }

    /// Treat a non-2xx status as an error. Callers decide whether 404 is
    /// success (delete/get) or "not found".
    fn ensure_2xx(response: &StorageHttpResponse, context: &str) -> StorageResult<()> {
        if (200..300).contains(&response.status) {
            Ok(())
        } else {
            Err(StorageError::Operation(format!(
                "gcs {context} failed with status {}",
                response.status
            )))
        }
    }
}

#[async_trait]
impl<C: StorageHttpClient, S: GcsSigner> StorageAdapter for GcsStorageAdapter<C, S> {
    /// GOOG4 pre-signed GET URL for `key`, valid for `ttl` seconds. Delegates to
    /// the injected signer; deferred/test signers return
    /// [`StorageError::Unsupported`]. Uses the origin's scheme+host (path-style
    /// `/{bucket}/{object}`), matching the public GCS endpoint (P-10).
    async fn presign(&self, key: &str, ttl: u64) -> StorageResult<String> {
        let normalized = normalize_key(key)?;
        let host = self
            .origin
            .host_str()
            .ok_or_else(|| StorageError::Unavailable("gcs origin has no host".to_string()))?
            .to_string();
        let origin = self.origin.origin().ascii_serialization();
        self.signer
            .presign(GcsPresignRequest {
                method: "GET",
                origin: &origin,
                host: &host,
                bucket: &self.bucket,
                object: &normalized,
                ttl_seconds: ttl,
            })
            .await
    }

    async fn put(&self, object: StorageObject) -> StorageResult<StorageMetadata> {
        // Normalize the key once via the S13 invariant. Go applies no special
        // trimming for GCS keys (no isS3PathStyle branch), so the normalized
        // key is the exact object name GCS sees.
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

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), content_type);
        headers.insert("content-length".to_string(), object.bytes.len().to_string());
        headers.insert("host".to_string(), host_header(&url)?);
        headers = self.sign_and_merge("PUT", &url, headers).await?;

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
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host_header(&url)?);
        headers = self.sign_and_merge("GET", &url, headers).await?;

        let response = self
            .http
            .execute(StorageHttpRequest {
                method: "GET",
                url,
                body: None,
                headers,
            })
            .await?;
        if response.status == 404 {
            // LoadData tolerates missing keys at the dispatcher level; surface
            // None so the higher layer can decide. (Go surfaces a wrapped
            // error, but the unified Rust trait uses Option.)
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
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host_header(&url)?);
        headers = self.sign_and_merge("DELETE", &url, headers).await?;

        let response = self
            .http
            .execute(StorageHttpRequest {
                method: "DELETE",
                url,
                body: None,
                headers,
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
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host_header(&url)?);
        headers = self.sign_and_merge("HEAD", &url, headers).await?;

        let response = self
            .http
            .execute(StorageHttpRequest {
                method: "HEAD",
                url,
                body: None,
                headers,
            })
            .await?;
        if response.status == 404 {
            return Ok(None);
        }
        Self::ensure_2xx(&response, "HEAD")?;
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
        let mut page_token: Option<String> = None;
        let mut objects = Vec::new();
        loop {
            let mut url = Url::parse(&format!(
                "https://storage.googleapis.com/storage/v1/b/{}/o",
                self.bucket
            ))
            .map_err(|error| StorageError::Unavailable(error.to_string()))?;
            {
                let mut query = url.query_pairs_mut();
                if !prefix.is_empty() {
                    query.append_pair("prefix", &prefix);
                }
                if let Some(token) = page_token.as_deref() {
                    query.append_pair("pageToken", token);
                }
            }
            let url = url.to_string();
            let mut headers = BTreeMap::new();
            headers.insert("host".to_string(), host_header(&url)?);
            headers = self.sign_and_merge("GET", &url, headers).await?;
            let response = self
                .http
                .execute(StorageHttpRequest {
                    method: "GET",
                    url,
                    body: None,
                    headers,
                })
                .await?;
            Self::ensure_2xx(&response, "LIST")?;
            let body = response.body;
            let page: GcsListResponse = serde_json::from_slice(&body).map_err(|error| {
                StorageError::Serialization(format!("invalid GCS list response: {error}"))
            })?;
            objects.extend(page.items.into_iter().map(|item| {
                let size = item.size.parse::<u64>().unwrap_or(0);
                let mut metadata = StorageMetadata::new(item.name, size);
                metadata.content_type = item.content_type;
                metadata
            }));
            page_token = page.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(objects)
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GcsListResponse {
    #[serde(default)]
    items: Vec<GcsListItem>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GcsListItem {
    name: String,
    #[serde(default)]
    size: String,
    #[serde(default)]
    content_type: Option<String>,
}

/// Collect the non-empty path segments of `url` in order. Returns an empty
/// vector when `url` has no path segments (cannot-be-a-base URLs). Used by
/// [`GcsStorageAdapter::url_for`] to preserve the endpoint's own path prefix
/// (rare for GCS, but kept for parity with the S3 adapter's URL builder).
fn collect_path_segments(url: &Url) -> Vec<String> {
    match url.path_segments() {
        Some(segments) => segments
            .filter(|segment| !segment.is_empty())
            .map(|segment| segment.to_string())
            .collect(),
        None => Vec::new(),
    }
}

/// Extract the `host[:port]` authority from a URL as the value to stamp into
/// the HTTP `host` header (RFC 7230 section 5.4).
fn host_header(url_str: &str) -> StorageResult<String> {
    let parsed = Url::parse(url_str)
        .map_err(|error| StorageError::Unavailable(format!("gcs url parse failed: {error}")))?;
    parsed
        .host_str()
        .map(|host| match parsed.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        })
        .ok_or_else(|| StorageError::Unavailable(format!("gcs url has no host: {url_str}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::StorageObject;
    use crate::webdav::InMemoryHttpClient;
    use std::sync::Arc;

    fn gcs_settings(bucket: &str) -> GcsSettings {
        GcsSettings {
            bucket_name: bucket.to_string(),
            credential: "{\"type\":\"service_account\"}".to_string(),
        }
    }

    /// Wrapper so a single shared [`InMemoryHttpClient`] can be injected into
    /// the adapter while tests keep a handle to read the recorded request log.
    #[derive(Debug, Clone)]
    struct SharedHttp(Arc<InMemoryHttpClient>);

    #[async_trait]
    impl StorageHttpClient for SharedHttp {
        async fn execute(&self, request: StorageHttpRequest) -> StorageResult<StorageHttpResponse> {
            self.0.execute(request).await
        }
    }

    fn adapter(
        bucket: &str,
        signer: impl GcsSigner,
    ) -> (
        GcsStorageAdapter<SharedHttp, impl GcsSigner>,
        Arc<InMemoryHttpClient>,
    ) {
        let http = Arc::new(InMemoryHttpClient::new());
        let adapter =
            GcsStorageAdapter::new(&gcs_settings(bucket), SharedHttp(http.clone()), signer)
                .unwrap_or_else(|error| panic!("gcs adapter build failed: {error:?}"));
        (adapter, http)
    }

    // -------------------------------------------------------------------------
    // Adapter construction + URL shape parity.
    // -------------------------------------------------------------------------

    #[test]
    fn new_rejects_empty_bucket() {
        let result = GcsStorageAdapter::new(
            &gcs_settings(""),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredGcsSigner,
        );
        match result {
            Err(StorageError::Unavailable(msg)) => assert!(msg.contains("bucket")),
            other => panic!("expected Unavailable for empty bucket, got {other:?}"),
        }
    }

    #[test]
    fn bucket_is_preserved_from_settings() -> StorageResult<()> {
        let adapter = GcsStorageAdapter::new(
            &gcs_settings("gcs-logs"),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredGcsSigner,
        )?;
        assert_eq!(adapter.bucket(), "gcs-logs");
        Ok(())
    }

    // -------------------------------------------------------------------------
    // P-10: GOOG4-RSA-SHA256 pre-signed URLs. These cover the deterministic,
    // bug-prone parts (encoding, canonical request/query, hashing); the RSA
    // signature itself is `jsonwebtoken`'s job (same primitive as the OAuth
    // path) and is exercised via the invalid-key error path.
    // -------------------------------------------------------------------------

    #[test]
    fn goog4_encode_follows_rfc3986() {
        assert_eq!(goog4_encode("aZ0-._~", false), "aZ0-._~");
        assert_eq!(
            goog4_encode("a/b", false),
            "a%2Fb",
            "slash encoded in values"
        );
        assert_eq!(goog4_encode("a/b", true), "a/b", "slash kept in URI path");
        assert_eq!(goog4_encode("svc@p.iam", false), "svc%40p.iam");
    }

    #[test]
    fn goog4_sha256_hex_matches_empty_vector() {
        assert_eq!(
            goog4_sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn goog4_base64url_decode_round_trips() -> StorageResult<()> {
        assert_eq!(goog4_base64url_decode("Zm9vYmFy")?, b"foobar");
        Ok(())
    }

    #[test]
    fn goog4_canonical_query_is_sorted_and_encoded() {
        let query = goog4_canonical_query(
            "svc@p.iam.gserviceaccount.com/20260101/auto/storage/goog4_request",
            "20260101T000000Z",
            3600,
        );
        assert_eq!(
            query,
            "X-Goog-Algorithm=GOOG4-RSA-SHA256\
             &X-Goog-Credential=svc%40p.iam.gserviceaccount.com%2F20260101%2Fauto%2Fstorage%2Fgoog4_request\
             &X-Goog-Date=20260101T000000Z\
             &X-Goog-Expires=3600\
             &X-Goog-SignedHeaders=host"
        );
    }

    #[test]
    fn goog4_canonical_request_has_sigv4_shape() {
        assert_eq!(
            goog4_canonical_request(
                "GET",
                "/bucket/obj.txt",
                "X-Goog-Algorithm=GOOG4-RSA-SHA256",
                "storage.googleapis.com",
            ),
            "GET\n/bucket/obj.txt\nX-Goog-Algorithm=GOOG4-RSA-SHA256\nhost:storage.googleapis.com\n\nhost\nUNSIGNED-PAYLOAD"
        );
    }

    #[test]
    fn goog4_signed_url_surfaces_invalid_key() {
        let Some(now) = chrono::DateTime::from_timestamp(1_767_225_600, 0) else {
            return;
        };
        let request = GcsPresignRequest {
            method: "GET",
            origin: "https://storage.googleapis.com",
            host: "storage.googleapis.com",
            bucket: "b",
            object: "o",
            ttl_seconds: 900,
        };
        // A non-PEM key must fail loudly (proves the RSA path is wired + errors
        // propagate; never a silent unsigned URL).
        let result = goog4_signed_url("svc@p.iam", "not-a-pem", request, now);
        assert!(matches!(result, Err(StorageError::Operation(_))));
    }

    #[tokio::test]
    async fn adapter_presign_is_unsupported_for_deferred_signer() -> StorageResult<()> {
        // Default seam: an adapter wired with the deferred signer reports
        // Unsupported rather than panicking or emitting an unsigned URL.
        let adapter = GcsStorageAdapter::new(
            &gcs_settings("bucket"),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredGcsSigner,
        )?;
        match adapter.presign("dir/obj.txt", 900).await {
            Err(StorageError::Unsupported) => Ok(()),
            other => panic!("expected Unsupported for deferred signer, got {other:?}"),
        }
    }

    #[test]
    fn url_for_targets_storage_googleapis_com_with_bucket_and_key() -> StorageResult<()> {
        // The GCS XML-style URL shape: https://storage.googleapis.com/<bucket>/<key>.
        let adapter = GcsStorageAdapter::new(
            &gcs_settings("logs"),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredGcsSigner,
        )?;
        let url = adapter.url_for("requests/abc.json")?;
        assert!(
            url.starts_with("https://storage.googleapis.com/"),
            "url was: {url}"
        );
        assert!(url.ends_with("/logs/requests/abc.json"), "url was: {url}");
        Ok(())
    }

    #[test]
    fn url_for_handles_nested_keys() -> StorageResult<()> {
        let adapter = GcsStorageAdapter::new(
            &gcs_settings("artifacts"),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredGcsSigner,
        )?;
        let url = adapter.url_for("deep/nested/path/item.bin")?;
        assert!(
            url.ends_with("/artifacts/deep/nested/path/item.bin"),
            "url was: {url}"
        );
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Key normalization / path-traversal rejection.
    // -------------------------------------------------------------------------

    #[test]
    fn url_for_rejects_path_traversal_key() {
        let adapter = GcsStorageAdapter::new(
            &gcs_settings("logs"),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredGcsSigner,
        )
        .unwrap_or_else(|error| panic!("adapter build failed: {error:?}"));
        match adapter.url_for("../escape.json") {
            Err(StorageError::InvalidKey(_)) => {}
            other => panic!("expected InvalidKey, got {other:?}"),
        }
    }

    #[test]
    fn url_for_rejects_leading_slash_key() {
        // Go applies no special trimming for GCS keys, but our normalize_key
        // (S13 invariant) rejects leading slashes outright — a strict superset
        // of what Go tolerates.
        let adapter = GcsStorageAdapter::new(
            &gcs_settings("logs"),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredGcsSigner,
        )
        .unwrap_or_else(|error| panic!("adapter build failed: {error:?}"));
        assert!(adapter.url_for("/leading.json").is_err());
    }

    #[test]
    fn url_for_rejects_redundant_separators() {
        // `normalize_key` rejects `a//b` outright; the URL must never carry a
        // double slash between bucket and key.
        let adapter = GcsStorageAdapter::new(
            &gcs_settings("logs"),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredGcsSigner,
        )
        .unwrap_or_else(|error| panic!("adapter build failed: {error:?}"));
        assert!(adapter.url_for("requests//double.json").is_err());
    }

    // -------------------------------------------------------------------------
    // Signer-is-invoked contract + HTTP round-trip via the fake client.
    // -------------------------------------------------------------------------

    /// Test-only signer that delegates to a shared `Arc<RecordingSigner>` so
    /// the test can read back every signing call after the fact. Mirrors the
    /// S3 ObservingSigner pattern.
    #[derive(Debug, Clone)]
    struct ObservingSigner {
        inner: Arc<RecordingSigner>,
    }

    #[async_trait]
    impl GcsSigner for ObservingSigner {
        async fn sign(
            &self,
            request: GcsSigningRequest<'_>,
        ) -> StorageResult<BTreeMap<String, String>> {
            self.inner.sign(request).await
        }
    }

    #[tokio::test]
    async fn put_signs_request_and_round_trips_through_in_memory_client()
    -> Result<(), Box<dyn std::error::Error>> {
        let signer = RecordingSigner::new();
        let recorded_signer = Arc::new(signer);
        let observing = ObservingSigner {
            inner: recorded_signer.clone(),
        };
        let http = Arc::new(InMemoryHttpClient::new());
        let adapter =
            GcsStorageAdapter::new(&gcs_settings("logs"), SharedHttp(http.clone()), observing)?;
        adapter
            .put(StorageObject::new(
                "requests/abc.json",
                br#"{"ok":true}"#.to_vec(),
            ))
            .await?;

        // The signer saw exactly one PUT call with the expected URL shape.
        let put = http
            .recorded()
            .into_iter()
            .find(|request| request.method == "PUT")
            .ok_or("no PUT recorded")?;
        assert!(
            put.url.ends_with("/logs/requests/abc.json"),
            "put url was: {}",
            put.url
        );
        // The signer's canned authorization header landed on the outgoing
        // request.
        assert_eq!(
            put.headers.get("authorization"),
            Some(&"Bearer ya29.test-token".to_string())
        );

        // GET round-trips through the same client.
        let loaded = match adapter.get("requests/abc.json").await? {
            Some(object) => object,
            None => return Err("expected stored object".into()),
        };
        assert_eq!(loaded.bytes, br#"{"ok":true}"#);
        Ok(())
    }

    #[tokio::test]
    async fn signer_is_invoked_with_method_and_url() -> Result<(), Box<dyn std::error::Error>> {
        // The load-bearing S07 contract: every HTTP verb the adapter issues
        // must flow through the signer with the canonical request pieces.
        let signer = RecordingSigner::new();
        let recorded_signer = Arc::new(signer);
        let observing = ObservingSigner {
            inner: recorded_signer.clone(),
        };
        let http = Arc::new(InMemoryHttpClient::new());
        let adapter =
            GcsStorageAdapter::new(&gcs_settings("logs"), SharedHttp(http.clone()), observing)?;

        adapter
            .put(StorageObject::new("a/b.json", b"payload".to_vec()))
            .await?;

        let signing_calls = recorded_signer.recorded();
        let put_sign = signing_calls
            .iter()
            .find(|entry| entry.method == "PUT")
            .ok_or("no PUT signing call recorded")?;
        assert!(put_sign.url.contains("/logs/a/b.json"));

        // GET signs as well.
        let _ = adapter.get("a/b.json").await?;
        assert!(
            recorded_signer
                .recorded()
                .into_iter()
                .any(|entry| entry.method == "GET"),
            "no GET signing call recorded"
        );

        // DELETE signs as well.
        let _ = adapter.delete("a/b.json").await?;
        assert!(
            recorded_signer
                .recorded()
                .into_iter()
                .any(|entry| entry.method == "DELETE"),
            "no DELETE signing call recorded"
        );
        Ok(())
    }

    // -------------------------------------------------------------------------
    // 404 tolerance.
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn get_returns_none_on_404() -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, _http) = adapter("logs", RecordingSigner::new());
        assert_eq!(adapter.get("never/saved.json").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn delete_returns_false_on_404() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go DeleteData: 404 is a successful no-op.
        let (adapter, _http) = adapter("logs", RecordingSigner::new());
        assert!(!adapter.delete("never/saved.json").await?);
        Ok(())
    }

    #[tokio::test]
    async fn head_returns_none_on_404() -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, _http) = adapter("logs", RecordingSigner::new());
        assert_eq!(adapter.head("missing.json").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn delete_after_put_reports_true_then_false() -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, _http) = adapter("logs", RecordingSigner::new());
        adapter
            .put(StorageObject::new("temp.json", b"x".to_vec()))
            .await?;
        assert!(adapter.delete("temp.json").await?);
        // Second delete is a 404 → false (still Ok per Go tolerance).
        assert!(!adapter.delete("temp.json").await?);
        Ok(())
    }

    // -------------------------------------------------------------------------
    // StaticSigner / DeferredGcsSigner behavior.
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn static_signer_merges_provided_headers_onto_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "Bearer ya29.real-token".to_string(),
        );
        let http = Arc::new(InMemoryHttpClient::new());
        let adapter = GcsStorageAdapter::new(
            &gcs_settings("logs"),
            SharedHttp(http.clone()),
            StaticSigner::new(headers),
        )?;
        adapter
            .put(StorageObject::new("k.json", b"v".to_vec()))
            .await?;
        let put = http
            .recorded()
            .into_iter()
            .find(|request| request.method == "PUT")
            .ok_or("no PUT recorded")?;
        assert_eq!(
            put.headers.get("authorization"),
            Some(&"Bearer ya29.real-token".to_string())
        );
        Ok(())
    }

    #[tokio::test]
    async fn deferred_signer_surfaces_operation_error_on_put() {
        // Production wiring path until real GCS signing lands: the adapter
        // builds (URL construction works) but every operation fails with a
        // clear "not yet implemented" message instead of sending unsigned
        // traffic.
        let (adapter, _http) = adapter("logs", DeferredGcsSigner);
        match adapter
            .put(StorageObject::new("k.json", b"v".to_vec()))
            .await
        {
            Err(StorageError::Operation(msg)) => {
                assert!(
                    msg.contains("signing not yet implemented"),
                    "msg was: {msg}"
                )
            }
            other => panic!("expected Operation(signing not yet implemented), got {other:?}"),
        }
    }
}
