//! Concrete `StorageAdapter` backends + the high-level dispatcher that mirrors
//! Go's `DataStorageService` (`conduit/internal/server/biz/data_storage.go`):
//! `buildFileSystem` (switch on type) and the data I/O methods `SaveData` /
//! `LoadData` / `DeleteData` (`SaveDataFromReader` is deferred — it requires an
//! `io::Reader` streaming trait which is out of scope for this bounded slice).
//!
//! This module covers RUST-P13-001 S04 (database storage) and wires the
//! existing `LocalStorageAdapter` (S05) into the dispatcher. S3/GCS/WebDAV
//! (S06/S07/S08) are wired with testable signing/HTTP seams: real signing
//! (SigV4 / GCP OAuth) is deferred but the adapter builds + URL construction
//! works, and every operation surfaces a clear "not yet implemented" error
//! until a production signer lands.
//!
//! **Contract parity notes** (Go `data_storage.go`):
//! - `TypeDatabase` `SaveData` is a **no-op** (line 510-511); `LoadData`
//!   returns the `key` argument verbatim as the data bytes (line 648-649);
//!   `DeleteData` is a **no-op** (line 612-613). We replicate this exactly.
//! - `TypeFs` writes via the filesystem; the Go side converts the key with
//!   `filepath.FromSlash(key)` and `MkdirAll(filepath.Dir(key))` before
//!   writing. `LocalStorageAdapter` already does `create_dir_all` on the
//!   parent, and its `normalize_key` rejects `\\` and leading/trailing `/`,
//!   which is a stricter-but-safe superset of the Go behavior.
//! - `DeleteData` for file backends treats "file not found" as success
//!   (Go: `errors.Is(err, os.ErrNotExist)` → `return nil`). Our adapter's
//!   `delete` returns `bool` (true if a file was actually removed); the
//!   dispatcher reports success on `Ok(_)` regardless of the bool, matching
//!   Go's tolerance of missing files.

use crate::adapter::{
    DataStorageKind, LocalStorageAdapter, StorageAdapter, StorageError, StorageMetadata,
    StorageObject, StorageResult,
};
use crate::gcs::{GcsSigner, GcsStorageAdapter, ServiceAccountGcsSigner};
use crate::http::ReqwestStorageHttpClient;
use crate::s3::{AwsSigV4Signer, S3Signer, S3StorageAdapter};
use crate::settings::DataStorageConfig;
use crate::webdav::{StorageHttpClient, WebDavStorageAdapter};
use std::path::PathBuf;
use std::sync::Mutex;

/// `StorageAdapter` implementation mirroring Go's `TypeDatabase` semantics
/// (RUST-P13-001 S04).
///
/// Go contract (`data_storage.go`):
/// - `SaveData`: **no-op** (line 510-511). The data lives in DB columns on the
///   row itself, so storing bytes via the storage facade is a no-op.
/// - `LoadData`: returns the `key` argument verbatim as the data (line 648-649)
///   — "for database storage, the key is the data itself".
/// - `DeleteData`: **no-op** (line 612-613); the row's columns hold the data.
///
/// This adapter carries that contract: `put` records nothing, `get` echoes the
/// key, `delete`/`head`/`list` are inert. The in-memory `seen` set exists ONLY
/// so that `head`/`list`/`exists` behave predictably in tests — it records
/// which keys were "written" so that a caller who probes `exists` after `put`
/// sees `true`. This matches the Go behavior where a `TypeDatabase` row, once
/// created, is queryable; it does NOT match a "store arbitrary blobs" model.
#[derive(Debug, Default)]
pub struct DatabaseStorageAdapter {
    seen: Mutex<std::collections::BTreeSet<String>>,
}

impl DatabaseStorageAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl StorageAdapter for DatabaseStorageAdapter {
    async fn put(&self, object: StorageObject) -> StorageResult<StorageMetadata> {
        // Go: SaveData for TypeDatabase returns nil immediately (no write).
        // We still record the key so head/exists reflect "this row exists".
        let key = object.metadata.key.clone();
        let metadata = object.metadata;
        if let Ok(mut seen) = self.seen.lock() {
            seen.insert(key);
        }
        Ok(metadata)
    }

    async fn get(&self, key: &str) -> StorageResult<Option<StorageObject>> {
        // Go: LoadData for TypeDatabase does `return []byte(key), nil`.
        // The "key" IS the payload. We surface that exactly: the bytes are
        // the UTF-8 of the key string.
        let bytes = key.as_bytes().to_vec();
        let metadata = StorageMetadata::new(key, bytes.len() as u64);
        Ok(Some(StorageObject { metadata, bytes }))
    }

    async fn delete(&self, key: &str) -> StorageResult<bool> {
        // Go: DeleteData for TypeDatabase returns nil (no-op). We report
        // whether the key was previously "seen" so tests can assert state.
        let removed = self
            .seen
            .lock()
            .map(|mut seen| seen.remove(key))
            .unwrap_or(false);
        Ok(removed)
    }

    async fn head(&self, key: &str) -> StorageResult<Option<StorageMetadata>> {
        let present = self
            .seen
            .lock()
            .map(|seen| seen.contains(key))
            .map_err(|_| StorageError::LockPoisoned("database storage"))?;
        if present {
            // content_length is unknown for the DB-as-payload model; report the
            // key byte length to mirror `get`'s metadata shape.
            Ok(Some(StorageMetadata::new(key, key.len() as u64)))
        } else {
            Ok(None)
        }
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<StorageMetadata>> {
        // Database storage has no filesystem-style listing; Go does not list
        // either. We surface the recorded "seen" keys filtered by prefix so
        // the adapter is well-behaved in mixed tests.
        let entries: Vec<String> = self
            .seen
            .lock()
            .map_err(|_| StorageError::LockPoisoned("database storage"))?
            .range::<str, _>(..)
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        Ok(entries
            .into_iter()
            .map(|k| {
                let len = k.len() as u64;
                StorageMetadata::new(k, len)
            })
            .collect())
    }

    // `exists` delegates to `head` via the default impl; that is fine here.
}

/// Dispatch table selecting a concrete [`StorageAdapter`] from a
/// [`DataStorageKind`] + [`DataStorageConfig`]. Mirrors Go's
/// `DataStorageService.buildFileSystem` switch (lines 143-189) and the per-type
/// branches inside `SaveData`/`LoadData`/`DeleteData`.
///
/// Returns a boxed `dyn StorageAdapter` so callers can treat every backend
/// uniformly. S3/GCS/WebDAV are wired with deferred signers (real SigV4 / GCP
/// OAuth is not yet ported); the adapters build + URL construction works, and
/// every signed operation surfaces a clear "signing not yet implemented" error
/// until a production signer lands.
///
/// **WebDAV (S08)** is wired: when the config carries a `webdav` block, the
/// dispatcher constructs a [`WebDavStorageAdapter`] backed by an
/// [`InMemoryHttpClient`] placeholder. The placeholder is NOT production-safe
/// (it stores bytes in-process and never opens a socket); production callers
/// must use [`build_webdav_backend`] to inject a real HTTP client (e.g. a
/// `reqwest`-backed [`StorageHttpClient`]). Until that wiring lands, the
/// dispatcher exists so that downstream code paths can detect "WebDAV is
/// configured" without special-casing the `Unsupported` error.
pub fn build_storage_backend(
    kind: &DataStorageKind,
    config: Option<&DataStorageConfig>,
) -> StorageResult<Box<dyn StorageAdapter>> {
    match kind {
        DataStorageKind::Memory => Ok(Box::new(crate::adapter::InMemoryStorageAdapter::new())),
        DataStorageKind::Local => {
            // Go: `if ds.Settings == nil || ds.Settings.Directory == nil {
            //         return nil, fmt.Errorf("directory not configured for fs storage") }`
            let directory = config.and_then(|c| c.directory.as_deref()).ok_or_else(|| {
                StorageError::Unavailable("directory not configured for fs storage".to_string())
            })?;
            Ok(Box::new(LocalStorageAdapter::new(PathBuf::from(directory))))
        }
        DataStorageKind::WebDav => {
            // Go lines 175-185: `if ds.Settings == nil || ds.Settings.WebDAV == nil {
            //     return nil, fmt.Errorf("webdav settings not configured") }`.
            let webdav = config.and_then(|c| c.webdav.as_ref()).ok_or_else(|| {
                StorageError::Unavailable("webdav settings not configured".to_string())
            })?;
            let directory = config.and_then(|c| c.directory.as_deref());
            // The dispatcher wires an in-memory placeholder HTTP client. The
            // dedicated `build_webdav_backend` constructor accepts a real
            // client for production use; see its docs for why the dispatcher
            // cannot.
            let http = ReqwestStorageHttpClient::new(None, webdav.insecure_skip_tls)?;
            let adapter = WebDavStorageAdapter::new(webdav, directory, http)?;
            Ok(Box::new(adapter))
        }
        DataStorageKind::S3 => {
            // Go line 153-160: `if ds.Settings == nil || ds.Settings.S3 == nil {
            //     return nil, fmt.Errorf("s3 settings not configured") }`.
            let s3 = config.and_then(|c| c.s3.as_ref()).ok_or_else(|| {
                StorageError::Unavailable("s3 settings not configured".to_string())
            })?;
            // The dispatcher wires the deferred SigV4 signer + in-memory
            // placeholder HTTP client. S3 *configuration* is now validated and
            // the adapter builds (URL construction works), but every signed
            // operation surfaces `StorageError::Operation("signing not yet
            // implemented")` until a real signer lands. The dedicated
            // `build_s3_backend` constructor accepts a real signer + HTTP
            // client for production use; see its docs for the rationale.
            let http = ReqwestStorageHttpClient::new(None, false)?;
            let signer = AwsSigV4Signer::new(s3)?;
            let adapter = S3StorageAdapter::new(s3, http, signer)?;
            Ok(Box::new(adapter))
        }
        DataStorageKind::Gcs => {
            // Go lines 164-174: `if ds.Settings == nil || ds.Settings.GCS == nil {
            //     return nil, fmt.Errorf("gcs settings not configured") }`.
            let gcs = config.and_then(|c| c.gcs.as_ref()).ok_or_else(|| {
                StorageError::Unavailable("gcs settings not configured".to_string())
            })?;
            // The dispatcher wires the deferred GCS signer + in-memory
            // placeholder HTTP client. GCS *configuration* is now validated
            // and the adapter builds (URL construction works), but every
            // signed operation surfaces `StorageError::Operation("signing not
            // yet implemented")` until a real signer lands. The dedicated
            // `build_gcs_backend` constructor accepts a real signer + HTTP
            // client for production use; see its docs for the rationale.
            let http = ReqwestStorageHttpClient::new(None, false)?;
            let signer = ServiceAccountGcsSigner::new(gcs)?;
            let adapter = GcsStorageAdapter::new(gcs, http, signer)?;
            Ok(Box::new(adapter))
        }
        DataStorageKind::Unknown(other) => Err(StorageError::Unavailable(format!(
            "unsupported storage type: {other}"
        ))),
    }
}

/// Build a WebDAV adapter with a caller-supplied HTTP client (RUST-P13-001
/// S08). Production code uses this to inject a `reqwest`-backed
/// [`StorageHttpClient`]; the dispatcher's [`build_storage_backend`] cannot
/// because it has no HTTP-client parameter and defaults to the in-memory
/// placeholder.
///
/// Mirrors Go's `createWebDAVFs` (lines 449-479) plus the WebDAV branches of
/// `SaveData`/`LoadData`/`DeleteData`. See [`WebDavStorageAdapter`] for the
/// full parity notes.
pub fn build_webdav_backend<C: StorageHttpClient>(
    settings: &crate::settings::WebDavSettings,
    directory: Option<&str>,
    http: C,
) -> StorageResult<WebDavStorageAdapter<C>> {
    WebDavStorageAdapter::new(settings, directory, http)
}

/// Build an S3 adapter with a caller-supplied HTTP client + signer
/// (RUST-P13-001 S06). Production code uses this to inject a `reqwest`-backed
/// [`StorageHttpClient`] and a real SigV4 [`S3Signer`]; the dispatcher's
/// [`build_storage_backend`] cannot because it has no signer/client parameters
/// and defaults to [`DeferredSigV4Signer`] + the in-memory placeholder client.
///
/// Mirrors Go's `createS3Fs` (`data_storage.go` lines 387-420) plus the S3
/// branches of `SaveData`/`LoadData`/`DeleteData` (lines 512-674). See
/// [`S3StorageAdapter`] for the full parity notes.
pub fn build_s3_backend<C: StorageHttpClient, S: S3Signer>(
    settings: &crate::settings::S3Settings,
    http: C,
    signer: S,
) -> StorageResult<S3StorageAdapter<C, S>> {
    S3StorageAdapter::new(settings, http, signer)
}

/// Build a GCS adapter with a caller-supplied HTTP client + signer
/// (RUST-P13-001 S07). Production code uses this to inject a `reqwest`-backed
/// [`StorageHttpClient`] and a real GCP OAuth [`GcsSigner`]; the dispatcher's
/// [`build_storage_backend`] cannot because it has no signer/client parameters
/// and defaults to [`DeferredGcsSigner`] + the in-memory placeholder client.
///
/// Mirrors Go's `createGcsFs` (`data_storage.go` lines 422-446) plus the GCS
/// branches of `SaveData`/`LoadData`/`DeleteData` (lines 508-674). See
/// [`GcsStorageAdapter`] for the full parity notes.
pub fn build_gcs_backend<C: StorageHttpClient, S: GcsSigner>(
    settings: &crate::settings::GcsSettings,
    http: C,
    signer: S,
) -> StorageResult<GcsStorageAdapter<C, S>> {
    GcsStorageAdapter::new(settings, http, signer)
}

// ---------------------------------------------------------------------------
// Production builders: wire [`ReqwestStorageHttpClient`] + Go's transport
// policy (10-minute timeout, `InsecureSkipTLS` → `danger_accept_invalid_certs`).
// These are thin conveniences over `build_*_backend` so a production caller
// does not have to assemble the reqwest client by hand.
// ---------------------------------------------------------------------------

/// Build a WebDAV adapter backed by [`ReqwestStorageHttpClient`], configured to
/// mirror Go's `createWebDAVFs` HTTP transport (`data_storage.go` lines
/// 449-457): 10-minute timeout unless `timeout` overrides it, and
/// `danger_accept_invalid_certs = settings.insecure_skip_tls`.
///
/// Use this in production instead of [`build_webdav_backend`] when you just
/// have a [`crate::settings::WebDavSettings`] and want the Go-default
/// transport. For S3/GCS, the production builder still needs a real signer
/// (SigV4 / GCP OAuth) — see [`build_s3_production_backend`] /
/// [`build_gcs_production_backend`].
pub fn build_webdav_production_backend(
    settings: &crate::settings::WebDavSettings,
    directory: Option<&str>,
    timeout: Option<std::time::Duration>,
) -> StorageResult<WebDavStorageAdapter<crate::http::ReqwestStorageHttpClient>> {
    let http = crate::http::ReqwestStorageHttpClient::new(timeout, settings.insecure_skip_tls)?;
    WebDavStorageAdapter::new(settings, directory, http)
}

/// Build an S3 adapter backed by a caller-configured
/// [`ReqwestStorageHttpClient`] plus a real SigV4 [`S3Signer`]. The HTTP
/// client honors Go's transport policy (10-minute timeout,
/// `insecure_skip_tls` → `danger_accept_invalid_certs`); the signer must be a
/// real SigV4 implementation (`DeferredSigV4Signer` will reject every
/// operation with "signing not yet implemented").
pub fn build_s3_production_backend<S: S3Signer>(
    settings: &crate::settings::S3Settings,
    signer: S,
    timeout: Option<std::time::Duration>,
    insecure_skip_tls: bool,
) -> StorageResult<S3StorageAdapter<crate::http::ReqwestStorageHttpClient, S>> {
    let http = crate::http::ReqwestStorageHttpClient::new(timeout, insecure_skip_tls)?;
    S3StorageAdapter::new(settings, http, signer)
}

/// Build a GCS adapter backed by a caller-configured
/// [`ReqwestStorageHttpClient`] plus a real GCP OAuth [`GcsSigner`]. The HTTP
/// client honors Go's transport policy (10-minute timeout,
/// `insecure_skip_tls` → `danger_accept_invalid_certs`); the signer must be a
/// real GCP OAuth implementation (`DeferredGcsSigner` will reject every
/// operation with "signing not yet implemented").
pub fn build_gcs_production_backend<S: GcsSigner>(
    settings: &crate::settings::GcsSettings,
    signer: S,
    timeout: Option<std::time::Duration>,
    insecure_skip_tls: bool,
) -> StorageResult<GcsStorageAdapter<crate::http::ReqwestStorageHttpClient, S>> {
    let http = crate::http::ReqwestStorageHttpClient::new(timeout, insecure_skip_tls)?;
    GcsStorageAdapter::new(settings, http, signer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{DataStorageKind, StorageAdapter, StorageObject};

    // -------------------------------------------------------------------------
    // DatabaseStorageAdapter — mirrors Go TypeDatabase behavior
    // (data_storage.go SaveData/LoadData/DeleteData no-op + passthrough).
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn database_put_is_a_noop_that_records_existence() -> StorageResult<()> {
        let adapter = DatabaseStorageAdapter::new();
        let metadata = adapter
            .put(StorageObject::new(
                "the-payload-itself",
                b"ignored".to_vec(),
            ))
            .await?;
        // put returns metadata but, per Go, the bytes are NOT stored.
        assert_eq!(metadata.key, "the-payload-itself");
        assert!(adapter.exists("the-payload-itself").await?);
        Ok(())
    }

    #[tokio::test]
    async fn database_get_returns_key_as_payload() -> StorageResult<()> {
        // Mirrors Go LoadData: `return []byte(key), nil` for TypeDatabase.
        let adapter = DatabaseStorageAdapter::new();
        let payload = "{\"id\":1,\"body\":\"hello\"}";
        let object = match adapter.get(payload).await? {
            Some(object) => object,
            None => panic!("database get must always return the key as bytes"),
        };
        assert_eq!(object.bytes, payload.as_bytes());
        assert_eq!(object.metadata.content_length, payload.len() as u64);
        Ok(())
    }

    #[tokio::test]
    async fn database_get_returns_some_even_when_never_written() -> StorageResult<()> {
        // Go LoadData never consults a "written" set — it always returns the
        // key. Our adapter matches that: get is Some unconditionally.
        let adapter = DatabaseStorageAdapter::new();
        let object = adapter.get("never-put").await?;
        assert!(object.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn database_delete_is_a_noop_reporting_prior_existence() -> StorageResult<()> {
        // Go DeleteData for TypeDatabase is a bare `return nil`. Our adapter
        // additionally reports whether the key was previously recorded, which
        // is a strict superset of the Go contract (success either way).
        let adapter = DatabaseStorageAdapter::new();
        adapter
            .put(StorageObject::new("recorded", b"x".to_vec()))
            .await?;
        // Deleting a recorded key reports true and is a no-op on storage.
        assert!(adapter.delete("recorded").await?);
        // Deleting again reports false (already removed) — still Ok, matching
        // Go's tolerant DeleteData for missing files.
        assert!(!adapter.delete("recorded").await?);
        Ok(())
    }

    #[tokio::test]
    async fn database_head_reports_recorded_keys_only() -> StorageResult<()> {
        let adapter = DatabaseStorageAdapter::new();
        assert_eq!(adapter.head("absent").await?, None);
        adapter
            .put(StorageObject::new("present", b"x".to_vec()))
            .await?;
        let head = match adapter.head("present").await? {
            Some(h) => h,
            None => panic!("head must find the recorded key"),
        };
        assert_eq!(head.key, "present");
        Ok(())
    }

    #[tokio::test]
    async fn database_list_filters_recorded_keys_by_prefix() -> StorageResult<()> {
        let adapter = DatabaseStorageAdapter::new();
        adapter
            .put(StorageObject::new("req/a", b"x".to_vec()))
            .await?;
        adapter
            .put(StorageObject::new("req/b", b"x".to_vec()))
            .await?;
        adapter
            .put(StorageObject::new("other/c", b"x".to_vec()))
            .await?;
        let reqs: Vec<String> = adapter
            .list("req")
            .await?
            .into_iter()
            .map(|m| m.key)
            .collect();
        assert_eq!(reqs, vec!["req/a".to_string(), "req/b".to_string()]);
        Ok(())
    }

    // -------------------------------------------------------------------------
    // build_storage_backend — mirrors Go buildFileSystem dispatch
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn build_backend_memory_returns_in_memory_adapter() -> StorageResult<()> {
        let backend = build_storage_backend(&DataStorageKind::Memory, None)?;
        backend.put(StorageObject::new("k", b"v".to_vec())).await?;
        assert!(backend.exists("k").await?);
        Ok(())
    }

    #[tokio::test]
    async fn build_backend_local_requires_directory() {
        // Go: "directory not configured for fs storage".
        let result = build_storage_backend(&DataStorageKind::Local, None);
        match result {
            Err(StorageError::Unavailable(msg)) => {
                assert!(msg.contains("directory"), "msg was: {msg}");
            }
            Err(other) => panic!("expected Unavailable, got {other:?}"),
            Ok(_) => panic!("expected Unavailable, got Ok"),
        }
    }

    #[tokio::test]
    async fn build_backend_local_with_directory_writes_through()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let config = DataStorageConfig {
            directory: Some(temp.path().to_string_lossy().to_string()),
            ..Default::default()
        };
        let backend = build_storage_backend(&DataStorageKind::Local, Some(&config))?;
        backend
            .put(StorageObject::new("nested/item.txt", b"hi".to_vec()))
            .await?;
        let loaded = match backend.get("nested/item.txt").await? {
            Some(object) => object,
            None => panic!("expected the written object"),
        };
        assert_eq!(loaded.bytes, b"hi");
        Ok(())
    }

    #[tokio::test]
    async fn build_backend_s3_without_config_reports_unavailable() {
        // Go: "s3 settings not configured".
        match build_storage_backend(&DataStorageKind::S3, None) {
            Err(StorageError::Unavailable(msg)) => {
                assert!(msg.contains("s3"), "msg was: {msg}");
            }
            Err(other) => panic!("expected Unavailable, got {other:?}"),
            Ok(_) => panic!("expected Unavailable, got Ok"),
        }
    }

    #[tokio::test]
    async fn build_backend_s3_with_config_builds_real_signed_http_adapter()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = DataStorageConfig {
            s3: Some(crate::settings::S3Settings {
                bucket_name: "logs".to_string(),
                endpoint: "https://s3.example.com".to_string(),
                region: "us-east-1".to_string(),
                access_key: "test-access".to_string(),
                secret_key: "test-secret".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let _backend = build_storage_backend(&DataStorageKind::S3, Some(&config))?;
        Ok(())
    }

    #[tokio::test]
    async fn build_backend_gcs_rejects_incomplete_service_account()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = DataStorageConfig {
            gcs: Some(crate::settings::GcsSettings {
                bucket_name: "gcs-logs".to_string(),
                credential: "{\"type\":\"service_account\"}".to_string(),
            }),
            ..Default::default()
        };
        let result = build_storage_backend(&DataStorageKind::Gcs, Some(&config));
        assert!(matches!(result, Err(StorageError::Unavailable(_))));
        Ok(())
    }

    #[tokio::test]
    async fn build_backend_gcs_without_config_reports_unavailable() {
        // Go line 165-166: "gcs settings not configured".
        match build_storage_backend(&DataStorageKind::Gcs, None) {
            Err(StorageError::Unavailable(msg)) => {
                assert!(msg.contains("gcs"), "msg was: {msg}");
            }
            Err(other) => panic!("expected Unavailable, got {other:?}"),
            Ok(_) => panic!("expected Unavailable, got Ok"),
        }
    }

    #[tokio::test]
    async fn build_backend_webdav_without_config_reports_unavailable() {
        // Go line 176-177: "webdav settings not configured".
        match build_storage_backend(&DataStorageKind::WebDav, None) {
            Err(StorageError::Unavailable(msg)) => {
                assert!(msg.contains("webdav"), "msg was: {msg}");
            }
            Err(other) => panic!("expected Unavailable, got {other:?}"),
            Ok(_) => panic!("expected Unavailable, got Ok"),
        }
    }

    #[tokio::test]
    async fn build_backend_webdav_with_config_constructs_real_http_adapter()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = DataStorageConfig {
            webdav: Some(crate::settings::WebDavSettings {
                url: "https://dav.example.com".to_string(),
                username: "alice".to_string(),
                password: "s3cret".to_string(),
                path: "/dav".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let _backend = build_storage_backend(&DataStorageKind::WebDav, Some(&config))?;
        Ok(())
    }

    #[tokio::test]
    async fn build_webdav_backend_accepts_injected_http_client()
    -> Result<(), Box<dyn std::error::Error>> {
        // The dedicated constructor lets production inject a real HTTP client.
        use crate::webdav::InMemoryHttpClient;
        let settings = crate::settings::WebDavSettings {
            url: "https://dav.example.com".to_string(),
            path: "dav".to_string(),
            ..Default::default()
        };
        let adapter = build_webdav_backend(&settings, None, InMemoryHttpClient::new())?;
        assert_eq!(adapter.base_path(), "dav");
        Ok(())
    }

    #[tokio::test]
    async fn build_s3_backend_accepts_injected_http_client_and_signer()
    -> Result<(), Box<dyn std::error::Error>> {
        // The dedicated constructor lets production inject a real HTTP client
        // + a real SigV4 signer. Here we inject the test fakes to verify the
        // adapter builds + round-trips through the in-memory client.
        use crate::s3::StaticSigner;
        use crate::webdav::InMemoryHttpClient;
        use std::collections::BTreeMap;
        let settings = crate::settings::S3Settings {
            bucket_name: "logs".to_string(),
            endpoint: "https://s3.example.com".to_string(),
            region: "us-east-1".to_string(),
            path_style: true,
            ..Default::default()
        };
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "AWS4-HMAC-SHA256 test".to_string(),
        );
        let adapter = build_s3_backend(
            &settings,
            InMemoryHttpClient::new(),
            StaticSigner::new(headers),
        )?;
        assert!(adapter.path_style());
        assert_eq!(adapter.bucket(), "logs");
        Ok(())
    }

    #[tokio::test]
    async fn build_s3_backend_works_with_recording_signer_for_test_simulations()
    -> Result<(), Box<dyn std::error::Error>> {
        // Verifies the full round-trip with the RecordingSigner fake so the
        // "signer-is-invoked" contract is observable end-to-end via the
        // public constructor.
        use crate::s3::RecordingSigner;
        use crate::webdav::InMemoryHttpClient;
        use std::sync::Arc;
        #[derive(Debug, Clone)]
        struct SharedHttp(Arc<InMemoryHttpClient>);
        #[async_trait::async_trait]
        impl StorageHttpClient for SharedHttp {
            async fn execute(
                &self,
                request: crate::webdav::StorageHttpRequest,
            ) -> StorageResult<crate::webdav::StorageHttpResponse> {
                self.0.execute(request).await
            }
        }
        let settings = crate::settings::S3Settings {
            bucket_name: "data".to_string(),
            endpoint: "https://s3.example.com".to_string(),
            region: "us-east-1".to_string(),
            path_style: true,
            ..Default::default()
        };
        let http = Arc::new(InMemoryHttpClient::new());
        let signer = RecordingSigner::new();
        let signer_arc = Arc::new(signer);
        #[derive(Debug, Clone)]
        struct Observing {
            inner: Arc<RecordingSigner>,
        }
        impl crate::s3::S3Signer for Observing {
            fn sign(
                &self,
                request: crate::s3::SigningRequest<'_>,
            ) -> StorageResult<std::collections::BTreeMap<String, String>> {
                self.inner.sign(request)
            }
        }
        let adapter = build_s3_backend(
            &settings,
            SharedHttp(http.clone()),
            Observing {
                inner: signer_arc.clone(),
            },
        )?;
        adapter
            .put(StorageObject::new("nested/item.txt", b"hello".to_vec()))
            .await?;
        let signing = signer_arc.recorded();
        assert!(
            signing
                .iter()
                .any(|entry| entry.method == "PUT" && entry.url.contains("/data/nested/item.txt")),
            "no PUT signing call with expected url: {signing:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn build_gcs_backend_accepts_injected_http_client_and_signer()
    -> Result<(), Box<dyn std::error::Error>> {
        // The dedicated constructor lets production inject a real HTTP client
        // + a real GCP OAuth signer. Here we inject the test fakes to verify
        // the adapter builds.
        use crate::gcs::StaticSigner;
        use crate::webdav::InMemoryHttpClient;
        use std::collections::BTreeMap;
        let settings = crate::settings::GcsSettings {
            bucket_name: "gcs-logs".to_string(),
            credential: "{\"type\":\"service_account\"}".to_string(),
        };
        let mut headers = BTreeMap::new();
        headers.insert(
            "authorization".to_string(),
            "Bearer ya29.test-token".to_string(),
        );
        let adapter = build_gcs_backend(
            &settings,
            InMemoryHttpClient::new(),
            StaticSigner::new(headers),
        )?;
        assert_eq!(adapter.bucket(), "gcs-logs");
        Ok(())
    }

    #[tokio::test]
    async fn build_gcs_backend_works_with_recording_signer_for_test_simulations()
    -> Result<(), Box<dyn std::error::Error>> {
        // Verifies the full round-trip with the GCS RecordingSigner fake so
        // the "signer-is-invoked" contract is observable end-to-end via the
        // public constructor.
        use crate::gcs::RecordingSigner;
        use crate::webdav::InMemoryHttpClient;
        use std::sync::Arc;
        #[derive(Debug, Clone)]
        struct SharedHttp(Arc<InMemoryHttpClient>);
        #[async_trait::async_trait]
        impl StorageHttpClient for SharedHttp {
            async fn execute(
                &self,
                request: crate::webdav::StorageHttpRequest,
            ) -> StorageResult<crate::webdav::StorageHttpResponse> {
                self.0.execute(request).await
            }
        }
        let settings = crate::settings::GcsSettings {
            bucket_name: "gcs-data".to_string(),
            credential: "{\"type\":\"service_account\"}".to_string(),
        };
        let http = Arc::new(InMemoryHttpClient::new());
        let signer = RecordingSigner::new();
        let signer_arc = Arc::new(signer);
        #[derive(Debug, Clone)]
        struct Observing {
            inner: Arc<RecordingSigner>,
        }
        #[async_trait::async_trait]
        impl crate::gcs::GcsSigner for Observing {
            async fn sign(
                &self,
                request: crate::gcs::GcsSigningRequest<'_>,
            ) -> StorageResult<std::collections::BTreeMap<String, String>> {
                self.inner.sign(request).await
            }
        }
        let adapter = build_gcs_backend(
            &settings,
            SharedHttp(http.clone()),
            Observing {
                inner: signer_arc.clone(),
            },
        )?;
        adapter
            .put(StorageObject::new("nested/item.txt", b"hello".to_vec()))
            .await?;
        let signing = signer_arc.recorded();
        assert!(
            signing.iter().any(|entry| entry.method == "PUT"
                && entry.url.contains("/gcs-data/nested/item.txt")
                && entry.url.starts_with("https://storage.googleapis.com/")),
            "no PUT signing call with expected url: {signing:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn build_backend_unknown_kind_is_unavailable() {
        match build_storage_backend(&DataStorageKind::Unknown("ftp".to_string()), None) {
            Err(StorageError::Unavailable(msg)) => {
                assert!(msg.contains("ftp"), "msg was: {msg}");
            }
            Err(other) => panic!("expected Unavailable, got {other:?}"),
            Ok(_) => panic!("expected Unavailable, got Ok"),
        }
    }

    #[tokio::test]
    async fn build_webdav_production_backend_constructs_adapter_with_reqwest_client()
    -> Result<(), Box<dyn std::error::Error>> {
        // The production builder wires a ReqwestStorageHttpClient honoring the
        // Go transport policy. We only assert it builds + base-path resolves;
        // no network call is made (the test never issues a request).
        let settings = crate::settings::WebDavSettings {
            url: "https://dav.example.com".to_string(),
            username: "alice".to_string(),
            password: "s3cret".to_string(),
            path: "/dav".to_string(),
            insecure_skip_tls: false,
        };
        let adapter = build_webdav_production_backend(&settings, None, None)?;
        assert_eq!(adapter.base_path(), "dav");
        Ok(())
    }

    #[tokio::test]
    async fn build_s3_production_backend_constructs_adapter_with_reqwest_client()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors the S3 production path: reqwest client + a (here, deferred)
        // signer. The deferred signer rejects signed operations, but the
        // adapter still builds — confirming the production wiring compiles and
        // the S3Settings validation runs.
        use crate::s3::DeferredSigV4Signer;
        let settings = crate::settings::S3Settings {
            bucket_name: "data".to_string(),
            endpoint: "https://s3.example.com".to_string(),
            region: "us-east-1".to_string(),
            path_style: true,
            ..Default::default()
        };
        let adapter = build_s3_production_backend(&settings, DeferredSigV4Signer, None, false)?;
        assert_eq!(adapter.bucket(), "data");
        assert!(adapter.path_style());
        Ok(())
    }

    #[tokio::test]
    async fn build_gcs_production_backend_constructs_adapter_with_reqwest_client()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors the GCS production path: reqwest client + a (here, deferred)
        // signer. The deferred signer rejects signed operations, but the
        // adapter still builds — confirming the production wiring compiles and
        // the GcsSettings validation runs.
        use crate::gcs::DeferredGcsSigner;
        let settings = crate::settings::GcsSettings {
            bucket_name: "gcs-data".to_string(),
            credential: "{\"type\":\"service_account\"}".to_string(),
        };
        let adapter = build_gcs_production_backend(&settings, DeferredGcsSigner, None, false)?;
        assert_eq!(adapter.bucket(), "gcs-data");
        Ok(())
    }
}
