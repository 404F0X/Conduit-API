//! S3 storage backend (RUST-P13-001 S06) with a testable signing seam.
//!
//! Mirrors the Go S3 path inside `DataStorageService`:
//!
//! - `createS3Fs` (`conduit/internal/server/biz/data_storage.go` lines 387-420)
//!   builds an `awss3.NewFromConfig` client with `BaseEndpoint =
//!   s3Config.Endpoint` (when non-empty) and `o.UsePathStyle =
//!   s3Config.PathStyle`. The SDK then issues standard S3 REST calls
//!   (PUT/GET/DELETE/HEAD) against either a virtual-hosted-style URL
//!   (`https://<bucket>.<host>/<key>`) or, when `PathStyle` is on, a
//!   path-style URL (`https://<host>/<bucket>/<key>`).
//! - `SaveData` / `LoadData` / `DeleteData` (lines 512-674) for
//!   `TypeS3` call `fs.Create` / `afero.ReadFile` / `fs.Remove`. When
//!   `isS3PathStyle(ds)` is true, the key is first normalized with
//!   `strings.TrimPrefix(key, "/")` (lines 538-540, 585-587, 622-626,
//!   659-663) so S3-compatible stores (MinIO, Ceph RGW) do not reject
//!   the request with `InvalidArgument`. Our [`normalize_key`] (S13)
//!   already rejects leading slashes outright, so the same key shape
//!   flows to both URL styles here.
//! - `DeleteData` treats a missing object as success
//!   (`errors.Is(err, os.ErrNotExist) → return nil`, lines 628-633):
//!   we map HTTP 404 on DELETE to `Ok(false)` (no row removed), which
//!   the dispatcher reports as success — matching Go's tolerance.
//!
//! ## Signing seam
//!
//! Real AWS SigV4 signing is **deferred** (mirrors how the WebDAV
//! adapter deferred the real `reqwest` client in S08). The adapter
//! depends on the [`S3Signer`] trait, NOT on a concrete signer, so the
//! URL construction + key handling + HTTP transport are fully unit-
//! testable now via [`InMemoryHttpClient`] + [`StaticSigner`] /
//! [`RecordingSigner`] fakes. A production build wires a real SigV4
//! signer (either the `aws-sigv4` crate or a hand-rolled one); until
//! then [`DeferredSigV4Signer`] surfaces [`StorageError::Unsupported`]
//! so callers can detect the not-yet-wired state.
//!
//! The SHA-256 of the payload is computed inline (a small pure-Rust
//! implementation in [`sha256`] below) so the adapter does not pull a
//! `sha2` dependency into the workspace; the hash is fed to the signer
//! because SigV4 mandates an `x-amz-content-sha256` header whose value
//! is exactly this hex digest.

use crate::adapter::normalize_key;
use crate::adapter::{StorageAdapter, StorageError, StorageMetadata, StorageObject, StorageResult};
use crate::settings::S3Settings;
use crate::webdav::{StorageHttpClient, StorageHttpRequest, StorageHttpResponse};
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::BTreeMap;
use url::{Host, Url};

fn current_sigv4_datetime() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

// ---------------------------------------------------------------------------
// Minimal SHA-256 (pure Rust, no `unsafe`, no `sha2` dep).
// ---------------------------------------------------------------------------

mod sha256 {
    //! Compact SHA-256 implementation. Pure Rust, no `unsafe`. Exists only so
    //! the S3 adapter can compute the `x-amz-content-sha256` digest without
    //! pulling the `sha2` crate into the workspace. Verified against the
    //! well-known empty-string and "abc" test vectors in the tests below.

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    /// Compute the SHA-256 digest of `data` as a lower-case hex string.
    pub fn hex_digest(data: &[u8]) -> String {
        let mut h = H0;
        let mut buffer: Vec<u8> = Vec::with_capacity(data.len() + 72);
        buffer.extend_from_slice(data);
        let bit_len: u64 = (data.len() as u64).wrapping_mul(8);
        buffer.push(0x80);
        while buffer.len() % 64 != 56 {
            buffer.push(0);
        }
        buffer.extend_from_slice(&bit_len.to_be_bytes());

        for chunk in buffer.chunks_exact(64) {
            let mut w = [0u32; 64];
            for (i, word) in chunk.chunks_exact(4).enumerate() {
                w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let temp1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let temp2 = s0.wrapping_add(maj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(temp1);
                d = c;
                c = b;
                b = a;
                a = temp1.wrapping_add(temp2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }

        let mut out = String::with_capacity(64);
        for word in h {
            for byte in word.to_be_bytes() {
                let _ = std::fmt::write(&mut out, format_args!("{byte:02x}"));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Signer seam.
// ---------------------------------------------------------------------------

/// Signature input handed to [`S3Signer::sign`]. The digest is computed by
/// the adapter (via the inline [`sha256`] module) so the signer does not need
/// a SHA-2 dependency of its own, and so tests can assert on the exact digest
/// value the adapter fed through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningRequest<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub payload_sha256: &'a str,
    /// ISO 8601 basic date-time (e.g. `20260701T120000Z`) SigV4 stamps into
    /// `x-amz-date`. The date portion also feeds the credential scope.
    pub datetime: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresigningRequest<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub datetime: &'a str,
    pub expires_seconds: u64,
}

/// Injected SigV4 signing strategy. Production wires a real signer (either
/// the `aws-sigv4` crate or a hand-rolled implementation); tests inject
/// [`StaticSigner`] (returns a fixed header set) or [`RecordingSigner`]
/// (records the signing request and returns a canned header).
///
/// The signer returns a `BTreeMap<String, String>` rather than a full
/// `http::HeaderMap` so the adapter stays free of an `http` crate dependency.
/// Lower-cased header names are used for consistency with
/// [`StorageHttpResponse`] and with how the in-memory HTTP client stores
/// recorded requests.
pub trait S3Signer: Send + Sync {
    /// Produce the auth + `x-amz-*` headers for the request. Returning
    /// [`StorageError::Unsupported`] is the contract for "real SigV4 not yet
    /// wired" so the dispatcher can surface the not-yet-ported state without
    /// panicking.
    fn sign(&self, request: SigningRequest<'_>) -> StorageResult<BTreeMap<String, String>>;

    fn presign(&self, _request: PresigningRequest<'_>) -> StorageResult<String> {
        Err(StorageError::Unsupported)
    }
}

/// AWS Signature Version 4 signer for S3 requests.
#[derive(Debug, Clone)]
pub struct AwsSigV4Signer {
    access_key: String,
    secret_key: String,
    region: String,
}

impl AwsSigV4Signer {
    pub fn new(settings: &S3Settings) -> StorageResult<Self> {
        if settings.access_key.is_empty() || settings.secret_key.is_empty() {
            return Err(StorageError::Unavailable(
                "s3 accessKey and secretKey must be configured".to_string(),
            ));
        }
        Ok(Self {
            access_key: settings.access_key.clone(),
            secret_key: settings.secret_key.clone(),
            region: if settings.region.is_empty() {
                "us-east-1".to_string()
            } else {
                settings.region.clone()
            },
        })
    }

    fn hmac(key: &[u8], data: &str) -> StorageResult<Vec<u8>> {
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|_| StorageError::Operation("invalid SigV4 HMAC key".to_string()))?;
        mac.update(data.as_bytes());
        Ok(mac.finalize().into_bytes().to_vec())
    }

    fn hex(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = std::fmt::write(&mut out, format_args!("{byte:02x}"));
        }
        out
    }
}

impl S3Signer for AwsSigV4Signer {
    fn sign(&self, request: SigningRequest<'_>) -> StorageResult<BTreeMap<String, String>> {
        if request.datetime.len() < 8 {
            return Err(StorageError::Operation(
                "invalid SigV4 datetime".to_string(),
            ));
        }
        let url = Url::parse(request.url)
            .map_err(|error| StorageError::Operation(format!("invalid SigV4 URL: {error}")))?;
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        };
        if host.is_empty() {
            return Err(StorageError::Operation("SigV4 URL has no host".to_string()));
        }
        let canonical_uri = if url.path().is_empty() {
            "/"
        } else {
            url.path()
        };
        let mut query: Vec<&str> = url
            .query()
            .unwrap_or_default()
            .split('&')
            .filter(|v| !v.is_empty())
            .collect();
        query.sort_unstable();
        let canonical_query = query.join("&");
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical_headers = format!(
            "host:{host}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
            request.payload_sha256, request.datetime
        );
        let canonical_request = format!(
            "{}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{}",
            request.method, request.payload_sha256
        );
        let date = &request.datetime[..8];
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
            request.datetime,
            sha256::hex_digest(canonical_request.as_bytes())
        );
        let date_key = Self::hmac(format!("AWS4{}", self.secret_key).as_bytes(), date)?;
        let region_key = Self::hmac(&date_key, &self.region)?;
        let service_key = Self::hmac(&region_key, "s3")?;
        let signing_key = Self::hmac(&service_key, "aws4_request")?;
        let signature = Self::hex(&Self::hmac(&signing_key, &string_to_sign)?);

        Ok(BTreeMap::from([
            (
                "authorization".to_string(),
                format!(
                    "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
                    self.access_key
                ),
            ),
            ("host".to_string(), host),
            ("x-amz-date".to_string(), request.datetime.to_string()),
            (
                "x-amz-content-sha256".to_string(),
                request.payload_sha256.to_string(),
            ),
        ]))
    }

    fn presign(&self, request: PresigningRequest<'_>) -> StorageResult<String> {
        if request.datetime.len() < 8 {
            return Err(StorageError::Operation(
                "invalid SigV4 datetime".to_string(),
            ));
        }
        if request.expires_seconds == 0 || request.expires_seconds > 604_800 {
            return Err(StorageError::Operation(
                "S3 presign expiry must be between 1 and 604800 seconds".to_string(),
            ));
        }

        let mut url = Url::parse(request.url)
            .map_err(|error| StorageError::Operation(format!("invalid SigV4 URL: {error}")))?;
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_string(),
        };
        if host.is_empty() {
            return Err(StorageError::Operation("SigV4 URL has no host".to_string()));
        }

        let date = &request.datetime[..8];
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("X-Amz-Algorithm", "AWS4-HMAC-SHA256");
            query.append_pair("X-Amz-Credential", &format!("{}/{scope}", self.access_key));
            query.append_pair("X-Amz-Date", request.datetime);
            query.append_pair("X-Amz-Expires", &request.expires_seconds.to_string());
            query.append_pair("X-Amz-SignedHeaders", "host");
        }

        let canonical_uri = if url.path().is_empty() {
            "/"
        } else {
            url.path()
        };
        let mut query: Vec<&str> = url
            .query()
            .unwrap_or_default()
            .split('&')
            .filter(|value| !value.is_empty())
            .collect();
        query.sort_unstable();
        let canonical_query = query.join("&");
        let canonical_request = format!(
            "{}\n{canonical_uri}\n{canonical_query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD",
            request.method
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
            request.datetime,
            sha256::hex_digest(canonical_request.as_bytes())
        );
        let date_key = Self::hmac(format!("AWS4{}", self.secret_key).as_bytes(), date)?;
        let region_key = Self::hmac(&date_key, &self.region)?;
        let service_key = Self::hmac(&region_key, "s3")?;
        let signing_key = Self::hmac(&service_key, "aws4_request")?;
        let signature = Self::hex(&Self::hmac(&signing_key, &string_to_sign)?);
        url.query_pairs_mut()
            .append_pair("X-Amz-Signature", &signature);
        Ok(url.to_string())
    }
}

/// Placeholder signer that always returns [`StorageError::Unsupported`]. Used
/// by the dispatcher so S3 *configuration* is wired (the adapter builds, the
/// URL shape is computed) but real signed requests fail loudly with a clear
/// "not yet implemented" message rather than silently sending unsigned
/// traffic.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeferredSigV4Signer;

impl S3Signer for DeferredSigV4Signer {
    fn sign(&self, _request: SigningRequest<'_>) -> StorageResult<BTreeMap<String, String>> {
        Err(StorageError::Operation(
            "s3 signing not yet implemented (RUST-P13-001 S06 remaining)".to_string(),
        ))
    }
}

/// Test signer that always returns a fixed header set. The headers are merged
/// onto the outgoing request after any adapter-supplied metadata headers, so
/// tests can assert on auth shape without computing a real signature.
#[derive(Debug, Clone)]
pub struct StaticSigner {
    pub headers: BTreeMap<String, String>,
}

impl StaticSigner {
    pub fn new(headers: BTreeMap<String, String>) -> Self {
        Self { headers }
    }
}

impl S3Signer for StaticSigner {
    fn sign(&self, _request: SigningRequest<'_>) -> StorageResult<BTreeMap<String, String>> {
        Ok(self.headers.clone())
    }
}

/// Test signer that records every [`SigningRequest`] it sees and forwards it
/// to an inner signer (defaulting to [`StaticSigner`] with a single
/// `authorization` header). Tests use this to assert the "signer-is-invoked"
/// contract: method, URL, payload digest, and datetime all flow through
/// unchanged.
#[derive(Debug)]
pub struct RecordingSigner {
    inner: StaticSigner,
    requests: std::sync::Mutex<Vec<RecordingEntry>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingEntry {
    pub method: String,
    pub url: String,
    pub payload_sha256: String,
    pub datetime: String,
}

impl Default for RecordingSigner {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingSigner {
    /// Build a recorder whose canned response is a single
    /// `authorization: AWS4-HMAC-SHA256 ...` header so the request still
    /// flows through the in-memory HTTP client cleanly.
    pub fn new() -> Self {
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "AWS4-HMAC-SHA256 Credential=AKIA.../signed".to_string(),
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

impl S3Signer for RecordingSigner {
    fn sign(&self, request: SigningRequest<'_>) -> StorageResult<BTreeMap<String, String>> {
        if let Ok(mut log) = self.requests.lock() {
            log.push(RecordingEntry {
                method: request.method.to_string(),
                url: request.url.to_string(),
                payload_sha256: request.payload_sha256.to_string(),
                datetime: request.datetime.to_string(),
            });
        }
        self.inner.sign(request)
    }
}

// ---------------------------------------------------------------------------
// Adapter.
// ---------------------------------------------------------------------------

/// S3 storage adapter (RUST-P13-001 S06). Mirrors Go's `createS3Fs`
/// (`data_storage.go` lines 387-420) plus the S3 branches of
/// `SaveData`/`LoadData`/`DeleteData` (lines 512-674).
///
/// Construct with [`S3StorageAdapter::new`] (validates the bucket + endpoint
/// and resolves the URL style from `path_style`). Inject any
/// [`StorageHttpClient`] and [`S3Signer`]; the adapter is `Send + Sync`
/// because all of its state is.
///
/// The adapter does NOT implement real SigV4 — that is the remaining S06
/// work. Until a production signer lands, the dispatcher wires
/// [`DeferredSigV4Signer`] so requests fail with a clear message instead of
/// sending unsigned traffic that AWS would reject with `403`.
#[derive(Debug)]
pub struct S3StorageAdapter<C: StorageHttpClient, S: S3Signer> {
    bucket: String,
    /// Parsed host origin (scheme + host[:port], no path). Used as the base
    /// for both virtual-host and path-style URL construction. When `endpoint`
    /// was empty this is the regional AWS URL (`https://s3.<region>.amazonaws.com`).
    origin: Url,
    /// `true` mirrors Go's `o.UsePathStyle = s3Config.PathStyle` (line 410).
    /// Path-style puts the bucket in the URL path; virtual-host style puts it
    /// in a sub-domain.
    path_style: bool,
    region: String,
    http: C,
    signer: S,
}

impl<C: StorageHttpClient, S: S3Signer> S3StorageAdapter<C, S> {
    /// Build an S3 adapter from the typed [`S3Settings`] plus injected HTTP
    /// transport and signer. Mirrors Go's `createS3Fs` (lines 387-420).
    ///
    /// URL origin resolution mirrors the AWS SDK v2:
    /// - When `endpoint` is non-empty it is used verbatim (Go sets
    ///   `o.BaseEndpoint = lo.ToPtr(s3Config.Endpoint)`).
    /// - Otherwise the regional AWS URL `https://s3.<region>.amazonaws.com`
    ///   is synthesized. When `region` is also empty we fall back to
    ///   `https://s3.amazonaws.com` (the SDK default), matching the Go SDK
    ///   behavior when neither field is set.
    ///
    /// `path_style` mirrors `o.UsePathStyle` (line 410): when true the bucket
    /// is part of the URL path; when false it is a sub-domain (virtual-host
    /// style). The leading-slash trim Go performs inside `SaveData` /
    /// `LoadData` / `DeleteData` (lines 538-540, 585-587, 622-626, 659-663)
    /// is subsumed by [`normalize_key`], which already rejects leading
    /// slashes, so both URL styles receive the same key shape.
    pub fn new(settings: &S3Settings, http: C, signer: S) -> StorageResult<Self> {
        if settings.bucket_name.is_empty() {
            return Err(StorageError::Unavailable(
                "s3 bucketName not configured".to_string(),
            ));
        }

        let origin = if !settings.endpoint.is_empty() {
            Url::parse(&settings.endpoint).map_err(|error| {
                StorageError::Unavailable(format!("invalid s3 endpoint: {error}"))
            })?
        } else if !settings.region.is_empty() {
            Url::parse(&format!("https://s3.{}.amazonaws.com", settings.region))
                .map_err(|error| StorageError::Unavailable(format!("invalid s3 region: {error}")))?
        } else {
            // AWS SDK v2 default when neither endpoint nor region is set.
            Url::parse("https://s3.amazonaws.com").map_err(|error| {
                StorageError::Unavailable(format!("invalid s3 fallback url: {error}"))
            })?
        };

        // Defensive: the origin must carry a host so URL composition is well
        // defined. `Url::parse` rejects host-less inputs, but we assert again
        // so a future refactor cannot silently regress.
        if origin.host().is_none() {
            return Err(StorageError::Unavailable(format!(
                "s3 origin has no host: {origin}"
            )));
        }

        Ok(Self {
            bucket: settings.bucket_name.clone(),
            origin,
            path_style: settings.path_style,
            region: settings.region.clone(),
            http,
            signer,
        })
    }

    /// `true` when this adapter was constructed with `PathStyle = true`.
    /// Exposed so tests and operators can verify the URL-style decision
    /// without issuing a request.
    pub fn path_style(&self) -> bool {
        self.path_style
    }

    /// The bucket name the adapter writes to. Exposed for parity assertions.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// The AWS region (possibly empty when an explicit endpoint was given).
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Compose the fully-qualified S3 URL for `key`, mirroring the AWS SDK v2
    /// URL construction that Go's `awss3.NewFromConfig` performs.
    ///
    /// - **Path style** (`UsePathStyle = true`): `<origin>/<bucket>/<key>`.
    /// - **Virtual-host style**: `https://<bucket>.<host>[:port]/<key>`.
    ///   Virtual-host style is only valid when the bucket name is DNS-safe
    ///   (no underscores, no uppercase, length <= 63); we let the URL crate
    ///   validate the host and surface a parse failure as
    ///   [`StorageError::Unavailable`]. The Go SDK applies the same rule.
    ///
    /// The key is normalized first (no `..`, no leading/trailing slash, no
    /// backslashes), matching the S13 containment invariant that the Go
    /// `strings.TrimPrefix(key, "/")` step (lines 538-540) is a strict
    /// subset of.
    fn url_for(&self, key: &str) -> StorageResult<String> {
        let normalized = normalize_key(key)?;
        if self.path_style {
            // `<origin>/<bucket>/<key>`. Reuse the origin's scheme + host and
            // rebuild the path from scratch so a stray path on the endpoint
            // (e.g. `https://minio.local:9000/store`) does not corrupt the
            // object URL.
            let mut url = self.origin.clone();
            // The AWS SDK keeps the endpoint's own path as a prefix (the
            // `BaseEndpoint` path is treated as a leading segment), so collect
            // it before clearing. Push endpoint segments first, then bucket,
            // then key.
            let endpoint_segments: Vec<String> = collect_path_segments(&url);
            {
                let mut path_builder = url.path_segments_mut().map_err(|_| {
                    StorageError::Unavailable("s3 origin cannot be a base".to_string())
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
            return Ok(url.to_string());
        }

        // Virtual-host style: `<bucket>.<host>[:port]/<key>`. Build a fresh
        // URL so a host-bearing endpoint path (rare) does not interfere.
        let host = self.origin.host().ok_or_else(|| {
            StorageError::Unavailable("s3 origin has no host for virtual-host style".to_string())
        })?;
        let host_str = match host {
            Host::Domain(domain) => domain.to_string(),
            Host::Ipv4(addr) => addr.to_string(),
            Host::Ipv6(addr) => format!("[{addr}]"),
        };
        let port = self.origin.port();
        let credentials = self.origin.username();
        let virtual_host = match port {
            Some(port) => format!("{}.{}:{}", self.bucket, host_str, port),
            None => format!("{}.{}", self.bucket, host_str),
        };
        let mut url = Url::parse(&format!("{}://{}", self.origin.scheme(), virtual_host)).map_err(
            |error| {
                StorageError::Unavailable(format!(
                    "invalid virtual-host url for bucket {}: {error}",
                    self.bucket
                ))
            },
        )?;
        if !credentials.is_empty() {
            // `set_username` returns `Result<(), ()>`; the unit error only
            // occurs for cannot-be-a-base URLs, which `Url::parse` above has
            // already ruled out. We surface a stable message in that case.
            if url.set_username(credentials).is_err() {
                return Err(StorageError::Unavailable(
                    "s3 virtual-host url cannot accept a username".to_string(),
                ));
            }
        }
        // Preserve the endpoint's path prefix (if any) for parity with the AWS
        // SDK, then append the key segments.
        let endpoint_path: Vec<String> = collect_path_segments(&self.origin);
        {
            let mut path_builder = url.path_segments_mut().map_err(|_| {
                StorageError::Unavailable("s3 virtual-host url cannot be a base".to_string())
            })?;
            path_builder.clear();
            for segment in &endpoint_path {
                path_builder.push(segment);
            }
            for part in normalized.split('/').filter(|segment| !segment.is_empty()) {
                path_builder.push(part);
            }
        }
        Ok(url.to_string())
    }

    /// Sign `request` then merge the resulting auth headers with the metadata
    /// headers the adapter already prepared. Auth headers win on conflict so
    /// the signer can stamp `x-amz-content-sha256` over any default.
    fn sign_and_merge(
        &self,
        method: &str,
        url: &str,
        payload_sha256: &str,
        datetime: &str,
        mut headers: BTreeMap<String, String>,
    ) -> StorageResult<BTreeMap<String, String>> {
        let signed = self.signer.sign(SigningRequest {
            method,
            url,
            payload_sha256,
            datetime,
        })?;
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
                "s3 {context} failed with status {}",
                response.status
            )))
        }
    }
}

#[async_trait]
impl<C: StorageHttpClient, S: S3Signer> StorageAdapter for S3StorageAdapter<C, S> {
    async fn put(&self, object: StorageObject) -> StorageResult<StorageMetadata> {
        // Normalize the key once via the S13 invariant (stricter-than-Go
        // superset of the leading-slash trim at lines 538-540).
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
        let payload_sha256 = sha256::hex_digest(&object.bytes);
        // Fixed stamp: SigV4 is date-driven and the real signer would derive
        // this from `chrono::Utc::now()`. We do not pull `chrono` here; the
        // signer owns time. Tests inject a `StaticSigner` so the value does
        // not matter, and the deferred production signer stamps the real now.
        let datetime = current_sigv4_datetime();

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), content_type);
        headers.insert("content-length".to_string(), object.bytes.len().to_string());
        headers.insert("host".to_string(), host_header(&url)?);
        headers = self.sign_and_merge("PUT", &url, &payload_sha256, &datetime, headers)?;

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
        let payload_sha256 = sha256::hex_digest(&[]);
        let datetime = current_sigv4_datetime();
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host_header(&url)?);
        headers = self.sign_and_merge("GET", &url, &payload_sha256, &datetime, headers)?;

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
        let payload_sha256 = sha256::hex_digest(&[]);
        let datetime = current_sigv4_datetime();
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host_header(&url)?);
        headers = self.sign_and_merge("DELETE", &url, &payload_sha256, &datetime, headers)?;

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
        let payload_sha256 = sha256::hex_digest(&[]);
        let datetime = current_sigv4_datetime();
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host_header(&url)?);
        headers = self.sign_and_merge("HEAD", &url, &payload_sha256, &datetime, headers)?;

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

    async fn presign(&self, key: &str, ttl: u64) -> StorageResult<String> {
        let key = normalize_key(key)?;
        let url = self.url_for(&key)?;
        let datetime = current_sigv4_datetime();
        self.signer.presign(PresigningRequest {
            method: "GET",
            url: &url,
            datetime: &datetime,
            expires_seconds: ttl,
        })
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<StorageMetadata>> {
        let prefix = if prefix.is_empty() {
            String::new()
        } else {
            normalize_key(prefix)?
        };
        let mut continuation: Option<String> = None;
        let mut objects = Vec::new();
        loop {
            let mut url = Url::parse(&self.url_for("__list__")?)
                .map_err(|error| StorageError::Unavailable(error.to_string()))?;
            url.path_segments_mut()
                .map_err(|_| StorageError::Unavailable("invalid S3 bucket URL".to_string()))?
                .pop();
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("list-type", "2");
                if !prefix.is_empty() {
                    query.append_pair("prefix", &prefix);
                }
                if let Some(token) = continuation.as_deref() {
                    query.append_pair("continuation-token", token);
                }
            }
            let url = url.to_string();
            let payload_sha256 = sha256::hex_digest(&[]);
            let datetime = current_sigv4_datetime();
            let mut headers = BTreeMap::new();
            headers.insert("host".to_string(), host_header(&url)?);
            headers = self.sign_and_merge("GET", &url, &payload_sha256, &datetime, headers)?;
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
            let page: S3ListBucketResult = quick_xml::de::from_reader(response.body.as_slice())
                .map_err(|error| {
                    StorageError::Serialization(format!("invalid S3 list response: {error}"))
                })?;
            objects.extend(
                page.contents
                    .into_iter()
                    .map(|item| StorageMetadata::new(item.key, item.size)),
            );
            continuation = page.next_continuation_token;
            if !page.is_truncated || continuation.is_none() {
                break;
            }
        }
        Ok(objects)
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct S3ListBucketResult {
    #[serde(default)]
    contents: Vec<S3ListItem>,
    #[serde(default)]
    is_truncated: bool,
    #[serde(default)]
    next_continuation_token: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct S3ListItem {
    key: String,
    #[serde(default)]
    size: u64,
}

/// Collect the non-empty path segments of `url` in order. Returns an empty
/// vector when `url` has no path segments (cannot-be-a-base URLs). Used by
/// [`S3StorageAdapter::url_for`] to preserve the endpoint's own path prefix
/// (e.g. `https://minio.local:9000/store`) for both URL styles, matching the
/// AWS SDK v2 behavior where `BaseEndpoint`'s path is treated as a leading
/// prefix.
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
/// the SigV4 `host` header (RFC 7230 §5.4 + SigV4 host requirement).
fn host_header(url_str: &str) -> StorageResult<String> {
    let parsed = Url::parse(url_str)
        .map_err(|error| StorageError::Unavailable(format!("s3 url parse failed: {error}")))?;
    parsed
        .host_str()
        .map(|host| match parsed.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        })
        .ok_or_else(|| StorageError::Unavailable(format!("s3 url has no host: {url_str}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::StorageObject;
    use crate::webdav::InMemoryHttpClient;
    use std::sync::Arc;

    fn s3_settings(bucket: &str, endpoint: &str, region: &str, path_style: bool) -> S3Settings {
        S3Settings {
            bucket_name: bucket.to_string(),
            endpoint: endpoint.to_string(),
            region: region.to_string(),
            access_key: "AKIAEXAMPLE".to_string(),
            secret_key: "s3kr3t".to_string(),
            path_style,
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

    fn path_style_adapter(
        bucket: &str,
        endpoint: &str,
        signer: impl S3Signer,
    ) -> (
        S3StorageAdapter<SharedHttp, impl S3Signer>,
        Arc<InMemoryHttpClient>,
    ) {
        let http = Arc::new(InMemoryHttpClient::new());
        let adapter = S3StorageAdapter::new(
            &s3_settings(bucket, endpoint, "us-east-1", true),
            SharedHttp(http.clone()),
            signer,
        )
        .unwrap_or_else(|error| panic!("s3 adapter build failed: {error:?}"));
        (adapter, http)
    }

    // -------------------------------------------------------------------------
    // sha256 module self-check — well-known NIST test vectors.
    // -------------------------------------------------------------------------

    #[test]
    fn sha256_empty_string_is_known_constant() {
        // RFC 6234 / NIST FIPS 180-4 empty-input digest.
        assert_eq!(
            sha256::hex_digest(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_abc_is_known_constant() {
        // The classic "abc" test vector.
        assert_eq!(
            sha256::hex_digest(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sigv4_signer_matches_independent_known_vector() -> StorageResult<()> {
        let settings = S3Settings {
            bucket_name: "examplebucket".to_string(),
            region: "us-east-1".to_string(),
            access_key: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            ..Default::default()
        };
        let signer = AwsSigV4Signer::new(&settings)?;
        let payload = sha256::hex_digest(b"");
        let headers = signer.sign(SigningRequest {
            method: "GET",
            url: "https://examplebucket.s3.amazonaws.com/test.txt",
            payload_sha256: &payload,
            datetime: "20130524T000000Z",
        })?;

        assert_eq!(
            headers.get("host").map(String::as_str),
            Some("examplebucket.s3.amazonaws.com")
        );
        assert_eq!(headers.get("x-amz-content-sha256"), Some(&payload));
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=df548e2ce037944d03f3e68682813b093763996d597cf890ca3d9037fd231eb4"
            )
        );
        Ok(())
    }

    #[test]
    fn sigv4_presign_emits_required_query_auth_fields() -> StorageResult<()> {
        let signer = AwsSigV4Signer::new(&s3_settings(
            "examplebucket",
            "https://s3.amazonaws.com",
            "us-east-1",
            true,
        ))?;
        let signed = signer.presign(PresigningRequest {
            method: "GET",
            url: "https://s3.amazonaws.com/examplebucket/test.txt?response-content-type=text%2Fplain",
            datetime: "20130524T000000Z",
            expires_seconds: 86400,
        })?;
        let parsed = Url::parse(&signed)
            .map_err(|error| StorageError::Operation(format!("invalid signed URL: {error}")))?;
        let query: BTreeMap<String, String> = parsed.query_pairs().into_owned().collect();
        assert_eq!(
            query.get("X-Amz-Algorithm").map(String::as_str),
            Some("AWS4-HMAC-SHA256")
        );
        assert_eq!(
            query.get("X-Amz-Date").map(String::as_str),
            Some("20130524T000000Z")
        );
        assert_eq!(
            query.get("X-Amz-Expires").map(String::as_str),
            Some("86400")
        );
        assert_eq!(
            query.get("X-Amz-SignedHeaders").map(String::as_str),
            Some("host")
        );
        assert!(
            query
                .get("X-Amz-Credential")
                .is_some_and(|value| value.ends_with("/20130524/us-east-1/s3/aws4_request"))
        );
        assert_eq!(query.get("X-Amz-Signature").map(String::len), Some(64));
        assert_eq!(
            query.get("response-content-type").map(String::as_str),
            Some("text/plain")
        );
        Ok(())
    }

    #[test]
    fn sigv4_presign_rejects_expiry_outside_aws_limits() -> StorageResult<()> {
        let signer = AwsSigV4Signer::new(&s3_settings(
            "bucket",
            "https://s3.amazonaws.com",
            "us-east-1",
            true,
        ))?;
        for expires_seconds in [0, 604_801] {
            let result = signer.presign(PresigningRequest {
                method: "GET",
                url: "https://s3.amazonaws.com/bucket/key",
                datetime: "20260718T000000Z",
                expires_seconds,
            });
            assert!(matches!(result, Err(StorageError::Operation(_))));
        }
        Ok(())
    }

    #[test]
    fn sigv4_signer_rejects_missing_credentials() {
        let result = AwsSigV4Signer::new(&S3Settings::default());
        assert!(matches!(result, Err(StorageError::Unavailable(_))));
    }

    // -------------------------------------------------------------------------
    // Adapter construction + URL shape parity.
    // -------------------------------------------------------------------------

    #[test]
    fn new_rejects_empty_bucket() {
        let result = S3StorageAdapter::new(
            &s3_settings("", "https://s3.example.com", "us-east-1", false),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        );
        match result {
            Err(StorageError::Unavailable(msg)) => assert!(msg.contains("bucket")),
            other => panic!("expected Unavailable for empty bucket, got {other:?}"),
        }
    }

    #[test]
    fn path_style_flag_is_preserved_from_settings() -> StorageResult<()> {
        let adapter = S3StorageAdapter::new(
            &s3_settings("logs", "https://s3.example.com", "us-east-1", true),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )?;
        assert!(adapter.path_style());
        assert_eq!(adapter.bucket(), "logs");
        assert_eq!(adapter.region(), "us-east-1");
        Ok(())
    }

    #[test]
    fn virtual_host_style_flag_is_preserved_from_settings() -> StorageResult<()> {
        let adapter = S3StorageAdapter::new(
            &s3_settings("logs", "https://s3.example.com", "us-east-1", false),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )?;
        assert!(!adapter.path_style());
        Ok(())
    }

    #[test]
    fn endpoint_empty_region_set_synthesizes_regional_aws_url() -> StorageResult<()> {
        // Mirrors AWS SDK v2 default: when only region is given, base URL is
        // https://s3.<region>.amazonaws.com.
        let adapter = S3StorageAdapter::new(
            &S3Settings {
                bucket_name: "b".to_string(),
                endpoint: String::new(),
                region: "eu-west-1".to_string(),
                access_key: String::new(),
                secret_key: String::new(),
                path_style: true,
            },
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )?;
        let url = adapter.url_for("path/to/key.json")?;
        assert!(
            url.starts_with("https://s3.eu-west-1.amazonaws.com/"),
            "url was: {url}"
        );
        Ok(())
    }

    #[test]
    fn endpoint_and_region_both_empty_falls_back_to_global_aws_url() -> StorageResult<()> {
        let adapter = S3StorageAdapter::new(
            &S3Settings {
                bucket_name: "b".to_string(),
                endpoint: String::new(),
                region: String::new(),
                access_key: String::new(),
                secret_key: String::new(),
                path_style: true,
            },
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )?;
        let url = adapter.url_for("k")?;
        assert!(
            url.starts_with("https://s3.amazonaws.com/"),
            "url was: {url}"
        );
        Ok(())
    }

    #[test]
    fn path_style_url_puts_bucket_in_path() -> StorageResult<()> {
        // Mirrors Go `UsePathStyle = true`: `<origin>/<bucket>/<key>`.
        let adapter = S3StorageAdapter::new(
            &s3_settings("logs", "https://s3.example.com", "us-east-1", true),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )?;
        let url = adapter.url_for("requests/abc.json")?;
        assert!(
            url.contains("/logs/requests/abc.json"),
            "path-style url was: {url}"
        );
        assert!(
            !url.contains("logs.s3.example.com"),
            "path-style must NOT use virtual host: {url}"
        );
        Ok(())
    }

    #[test]
    fn virtual_host_style_url_puts_bucket_in_subdomain() -> StorageResult<()> {
        // Mirrors Go `UsePathStyle = false`: `https://<bucket>.<host>/<key>`.
        let adapter = S3StorageAdapter::new(
            &s3_settings("logs", "https://s3.example.com", "us-east-1", false),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )?;
        let url = adapter.url_for("requests/abc.json")?;
        assert!(
            url.starts_with("https://logs.s3.example.com/"),
            "virtual-host url was: {url}"
        );
        assert!(
            url.ends_with("/requests/abc.json"),
            "key must be in path: {url}"
        );
        Ok(())
    }

    #[test]
    fn path_style_preserves_endpoint_port() -> StorageResult<()> {
        // MinIO / Ceph RGW often run on a custom port; the path-style URL
        // must keep it.
        let adapter = S3StorageAdapter::new(
            &s3_settings("data", "https://minio.local:9000", "us-east-1", true),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )?;
        let url = adapter.url_for("nested/item.txt")?;
        assert!(
            url.starts_with("https://minio.local:9000/"),
            "url was: {url}"
        );
        assert!(url.contains("/data/nested/item.txt"), "url was: {url}");
        Ok(())
    }

    #[test]
    fn virtual_host_style_preserves_endpoint_port() -> StorageResult<()> {
        let adapter = S3StorageAdapter::new(
            &s3_settings("data", "https://minio.local:9000", "us-east-1", false),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )?;
        let url = adapter.url_for("item.txt")?;
        assert!(
            url.starts_with("https://data.minio.local:9000/"),
            "url was: {url}"
        );
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Key normalization / path-traversal rejection.
    // -------------------------------------------------------------------------

    #[test]
    fn url_for_rejects_path_traversal_key() {
        let adapter = S3StorageAdapter::new(
            &s3_settings("logs", "https://s3.example.com", "us-east-1", true),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )
        .unwrap_or_else(|error| panic!("adapter build failed: {error:?}"));
        match adapter.url_for("../escape.json") {
            Err(StorageError::InvalidKey(_)) => {}
            other => panic!("expected InvalidKey, got {other:?}"),
        }
    }

    #[test]
    fn url_for_rejects_leading_slash_key() {
        // Mirrors Go's `strings.TrimPrefix(key, "/")` defense — but our
        // `normalize_key` rejects it outright (S13 invariant), which is a
        // strict superset of the Go trim.
        let adapter = S3StorageAdapter::new(
            &s3_settings("logs", "https://s3.example.com", "us-east-1", true),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )
        .unwrap_or_else(|error| panic!("adapter build failed: {error:?}"));
        assert!(adapter.url_for("/leading.json").is_err());
    }

    #[test]
    fn url_for_normalizes_redundant_separators() {
        // `normalize_key` rejects `a//b` outright; the URL must never carry a
        // double slash between bucket and key.
        let adapter = S3StorageAdapter::new(
            &s3_settings("logs", "https://s3.example.com", "us-east-1", true),
            SharedHttp(Arc::new(InMemoryHttpClient::new())),
            DeferredSigV4Signer,
        )
        .unwrap_or_else(|error| panic!("adapter build failed: {error:?}"));
        assert!(adapter.url_for("requests//double.json").is_err());
    }

    // -------------------------------------------------------------------------
    // Signer-is-invoked contract + HTTP round-trip via the fake client.
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn put_signs_request_and_round_trips_through_in_memory_client()
    -> Result<(), Box<dyn std::error::Error>> {
        let signer = RecordingSigner::new();
        let (adapter, http) =
            path_style_adapter("logs", "https://s3.example.com", RecordingSigner::new());
        // Rebuild with the actual signer we want to assert on; the helper
        // above created its own. Use the direct constructor instead.
        let _ = (adapter, http);

        let http = Arc::new(InMemoryHttpClient::new());
        let adapter = S3StorageAdapter::new(
            &s3_settings("logs", "https://s3.example.com", "us-east-1", true),
            SharedHttp(http.clone()),
            signer,
        )?;
        adapter
            .put(StorageObject::new(
                "requests/abc.json",
                br#"{"ok":true}"#.to_vec(),
            ))
            .await?;

        // The signer saw exactly one PUT call with the expected URL shape.
        let recorded = http.recorded();
        let put = recorded
            .iter()
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
            Some(&"AWS4-HMAC-SHA256 Credential=AKIA.../signed".to_string())
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
    async fn signer_is_invoked_with_method_url_digest_and_datetime()
    -> Result<(), Box<dyn std::error::Error>> {
        // The load-bearing S06 contract: every HTTP verb the adapter issues
        // must flow through the signer with the canonical request pieces.
        let signer = RecordingSigner::new();
        let recorded_signer = Arc::new(signer);
        // Adapter needs an owned signer; wrap the recorder so we can observe.
        let observing = ObservingSigner {
            inner: recorded_signer.clone(),
        };
        let http = Arc::new(InMemoryHttpClient::new());
        let adapter = S3StorageAdapter::new(
            &s3_settings("logs", "https://s3.example.com", "us-east-1", true),
            SharedHttp(http.clone()),
            observing,
        )?;

        adapter
            .put(StorageObject::new("a/b.json", b"payload".to_vec()))
            .await?;

        let signing_calls = recorded_signer.recorded();
        let put_sign = signing_calls
            .iter()
            .find(|entry| entry.method == "PUT")
            .ok_or("no PUT signing call recorded")?;
        // Payload digest is the SHA-256 of "payload".
        assert_eq!(put_sign.payload_sha256, sha256::hex_digest(b"payload"),);
        assert!(put_sign.url.contains("/logs/a/b.json"));
        assert!(!put_sign.datetime.is_empty());

        // GET signs with the empty-payload digest.
        let _ = adapter.get("a/b.json").await?;
        let get_sign = recorded_signer
            .recorded()
            .into_iter()
            .find(|entry| entry.method == "GET")
            .ok_or("no GET signing call recorded")?;
        assert_eq!(get_sign.payload_sha256, sha256::hex_digest(b""));

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

    /// Test-only signer that delegates to a shared `Arc<RecordingSigner>` so
    /// the test can read back every signing call after the fact.
    #[derive(Debug, Clone)]
    struct ObservingSigner {
        inner: Arc<RecordingSigner>,
    }

    impl S3Signer for ObservingSigner {
        fn sign(&self, request: SigningRequest<'_>) -> StorageResult<BTreeMap<String, String>> {
            self.inner.sign(request)
        }
    }

    // -------------------------------------------------------------------------
    // 404 tolerance.
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn get_returns_none_on_404() -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, _http) =
            path_style_adapter("logs", "https://s3.example.com", RecordingSigner::new());
        assert_eq!(adapter.get("never/saved.json").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn delete_returns_false_on_404() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go DeleteData: 404 is a successful no-op.
        let (adapter, _http) =
            path_style_adapter("logs", "https://s3.example.com", RecordingSigner::new());
        assert!(!adapter.delete("never/saved.json").await?);
        Ok(())
    }

    #[tokio::test]
    async fn head_returns_none_on_404() -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, _http) =
            path_style_adapter("logs", "https://s3.example.com", RecordingSigner::new());
        assert_eq!(adapter.head("missing.json").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn delete_after_put_reports_true_then_false() -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, _http) =
            path_style_adapter("logs", "https://s3.example.com", RecordingSigner::new());
        adapter
            .put(StorageObject::new("temp.json", b"x".to_vec()))
            .await?;
        assert!(adapter.delete("temp.json").await?);
        // Second delete is a 404 → false (still Ok per Go tolerance).
        assert!(!adapter.delete("temp.json").await?);
        Ok(())
    }

    // -------------------------------------------------------------------------
    // StaticSigner / DeferredSigV4Signer behavior.
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn static_signer_merges_provided_headers_onto_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut headers = BTreeMap::new();
        headers.insert("authorization".to_string(), "Bearer test-token".to_string());
        headers.insert(
            "x-amz-content-sha256".to_string(),
            "UNSIGNED-PAYLOAD".to_string(),
        );
        let http = Arc::new(InMemoryHttpClient::new());
        let adapter = S3StorageAdapter::new(
            &s3_settings("logs", "https://s3.example.com", "us-east-1", true),
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
            Some(&"Bearer test-token".to_string())
        );
        assert_eq!(
            put.headers.get("x-amz-content-sha256"),
            Some(&"UNSIGNED-PAYLOAD".to_string())
        );
        Ok(())
    }

    #[tokio::test]
    async fn deferred_signer_surfaces_unsupported_on_put() {
        // Production wiring path until real SigV4 lands: the adapter builds
        // (URL construction works) but every operation fails with a clear
        // "not yet implemented" message instead of sending unsigned traffic.
        let (adapter, _http) =
            path_style_adapter("logs", "https://s3.example.com", DeferredSigV4Signer);
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
