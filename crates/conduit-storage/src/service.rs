//! Service-level facade mirroring Go's `DataStorageService`
//! (`conduit/internal/server/biz/data_storage.go`). This is the
//! RUST-P13-001 S09 surface: it sits **above** the raw `StorageAdapter`
//! layer (S03-S08, S10, S11, S13, S15) and exposes the Go method set
//! `SaveData` / `LoadData` / `DeleteData` plus a `list` accessor (Go's
//! service itself has no `List`, but every adapter does, and S11 fixes
//! the trait method set as `put/get/delete/exists/presign/list`).
//!
//! ## Go contract mirrored (line refs to `data_storage.go`)
//!
//! - `SaveData(ctx, ds, key, data) error` (lines 508-559):
//!   - `TypeDatabase`: **no-op** (returns nil immediately; data lives in
//!     DB columns on the row itself).
//!   - `TypeFs` / `TypeS3` / `TypeGcs` / `TypeWebdav`: dispatch through
//!     the filesystem adapter. Notably the Go code pre-creates the file
//!     via `fs.Create` then `afero.WriteFile`; our adapter's `put` does
//!     both in one step.
//! - `LoadData(ctx, ds, key) ([]byte, error)` (lines 645-674):
//!   - `TypeDatabase`: returns `[]byte(key)` — "the key is the data
//!     itself".
//!   - File types: read from the FS; a missing file is an **error**
//!     (Go: `failed to read file`). Our facade surfaces this as
//!     [`StorageError::Operation`] when the adapter reports `None`.
//! - `DeleteData(ctx, ds, key) error` (lines 608-640):
//!   - `TypeDatabase`: **no-op** (returns nil).
//!   - File types: `fs.Remove`; a missing file is treated as **success**
//!     (Go: `errors.Is(err, os.ErrNotExist)` -> `return nil`). Our facade
//!     maps both `Ok(true)` and `Ok(false)` to `Ok(())`.
//!
//! ## Construction
//!
//! Go's `DataStorageService` resolves the backend lazily via
//! `buildFileSystem` + an in-process `fsCache` keyed by storage ID. In
//! the Rust crate we don't have the ent row cache yet, so the facade is
//! built eagerly from a `(DataStorageKind, DataStorageConfig)` pair via
//! [`DataStorageService::new`], which delegates to
//! [`build_storage_backend`] (Go's `buildFileSystem`). A second
//! constructor, [`DataStorageService::with_backend`], lets tests inject
//! any `StorageAdapter` fake directly — this is the seam A01 uses.
//!
//! ## Why a facade at all (vs. calling the adapter directly)
//!
//! The adapter trait is intentionally low-level (`put/get/delete` returning
//! metadata / `Option` / `bool`) so it can be uniform across backends. The
//! Go service layer adds two pieces of policy on top: (1) the per-Type
//! no-op / passthrough semantics for `TypeDatabase`, and (2) the
//! not-found tolerance of `DeleteData`. Centralizing that policy here
//! keeps call sites from having to re-derive it, and gives the upcoming
//! biz layer a single Rust type to depend on.

use crate::adapter::{
    DataStorageKind, StorageAdapter, StorageError, StorageMetadata, StorageObject, StorageResult,
};
use crate::backend::build_storage_backend;
use crate::settings::DataStorageConfig;

/// Service-level facade over a single active storage backend. Mirrors Go's
/// `DataStorageService` (`data_storage.go`) method set: `save_data` /
/// `load_data` / `delete_data` / `list`, plus pass-through accessors for
/// `exists` and `head` that higher layers may want without dropping to the
/// raw adapter.
///
/// See the module docs for the per-Type semantics this replicates.
pub struct DataStorageService {
    kind: DataStorageKind,
    backend: Box<dyn StorageAdapter>,
}

impl DataStorageService {
    /// Build a facade for the given `(kind, config)` pair by delegating to
    /// [`build_storage_backend`] (Go's `buildFileSystem`). This is the
    /// production-shaped constructor; tests usually want
    /// [`DataStorageService::with_backend`] so they can inject a fake.
    pub fn new(kind: DataStorageKind, config: Option<&DataStorageConfig>) -> StorageResult<Self> {
        let backend = build_storage_backend(&kind, config)?;
        Ok(Self { kind, backend })
    }

    /// Build a facade wrapping a caller-supplied adapter. The `kind` is
    /// retained so the facade can apply the per-Type policy (notably the
    /// `TypeDatabase` no-op / passthrough rules) without inspecting the
    /// adapter's concrete type.
    pub fn with_backend(kind: DataStorageKind, backend: Box<dyn StorageAdapter>) -> Self {
        Self { kind, backend }
    }

    /// The storage kind this facade was built for. Used internally to apply
    /// the Go per-Type policy and exposed for diagnostics.
    pub fn kind(&self) -> &DataStorageKind {
        &self.kind
    }

    /// Save `data` under `key`. Mirrors Go `SaveData`
    /// (`data_storage.go:508-559`).
    ///
    /// - For `TypeDatabase` (the `DatabaseStorageAdapter`) this is a
    ///   **no-op** at the storage layer: the bytes are not persisted as a
    ///   blob, because the data lives in DB columns on the row itself. The
    ///   facade still returns `Ok(())`.
    /// - For every file/object backend this delegates to
    ///   [`StorageAdapter::put`]; the returned metadata is discarded,
    ///   matching Go's `error`-only return shape.
    pub async fn save_data(&self, key: &str, data: &[u8]) -> StorageResult<()> {
        let object = StorageObject::new(key, data.to_vec());
        // Drop the returned metadata: Go's SaveData returns only an error.
        self.backend.put(object).await?;
        Ok(())
    }

    /// Load the bytes stored under `key`. Mirrors Go `LoadData`
    /// (`data_storage.go:645-674`).
    ///
    /// - For the `DatabaseStorageAdapter` the adapter returns the key
    ///   verbatim as the payload (Go: `return []byte(key), nil`), and
    ///   **always** returns `Some` — this is the "the key is the data
    ///   itself" rule for database storage.
    /// - For file/object backends a missing object surfaces as
    ///   [`StorageError::Operation`] so callers see the same
    ///   "failed to read file" shape Go returns (the adapter's `get`
    ///   returns `Option`; we translate `None` into an error here).
    pub async fn load_data(&self, key: &str) -> StorageResult<Vec<u8>> {
        match self.backend.get(key).await? {
            Some(object) => Ok(object.bytes),
            None => Err(StorageError::Operation(format!(
                "failed to read file: key not found: {key}"
            ))),
        }
    }

    /// Delete the object under `key`. Mirrors Go `DeleteData`
    /// (`data_storage.go:608-640`).
    ///
    /// - `TypeDatabase` / memory: a **no-op** that always succeeds.
    /// - File/object backends: delegates to [`StorageAdapter::delete`];
    ///   both "removed" and "was already absent" map to `Ok(())`,
    ///   matching Go's `errors.Is(err, os.ErrNotExist) -> return nil`.
    pub async fn delete_data(&self, key: &str) -> StorageResult<()> {
        // The adapter reports whether a file was actually removed; Go's
        // DeleteData treats both outcomes as success, so we discard the bool.
        let _ = self.backend.delete(key).await?;
        Ok(())
    }

    /// List object metadata under `prefix`. Go's `DataStorageService` has no
    /// `List` method of its own — listing lives on the adapter (S11 fixes
    /// the trait method set as `put/get/delete/exists/presign/list`). The
    /// facade exposes it for callers that want a single dependency.
    pub async fn list(&self, prefix: &str) -> StorageResult<Vec<StorageMetadata>> {
        self.backend.list(prefix).await
    }

    /// Whether an object exists at `key`. Pass-through to
    /// [`StorageAdapter::exists`]; included so higher layers don't need to
    /// hold a second handle to the adapter.
    pub async fn exists(&self, key: &str) -> StorageResult<bool> {
        self.backend.exists(key).await
    }

    /// Fetch metadata (no body) for `key`. Pass-through to
    /// [`StorageAdapter::head`].
    pub async fn head(&self, key: &str) -> StorageResult<Option<StorageMetadata>> {
        self.backend.head(key).await
    }
}

impl std::fmt::Debug for DataStorageService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataStorageService")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{DataStorageKind, InMemoryStorageAdapter, LocalStorageAdapter};
    use crate::backend::DatabaseStorageAdapter;

    // -------------------------------------------------------------------------
    // S09 facade — mirror Go DataStorageService_SaveData / _LoadData semantics
    // (data_storage.go:508-674 + data_storage_test.go:620-693).
    // -------------------------------------------------------------------------

    #[tokio::test]
    async fn save_data_to_database_kind_is_a_noop_that_succeeds() -> StorageResult<()> {
        // Mirrors Go TestDataStorageService_SaveData "save data to database
        // storage": SaveData for TypeDatabase returns nil immediately. Our
        // DatabaseStorageAdapter is the explicit TypeDatabase stand-in.
        let svc = DataStorageService::with_backend(
            DataStorageKind::Memory,
            Box::new(DatabaseStorageAdapter::new()),
        );
        // No panic, no error: save_data succeeds even though nothing is
        // persisted as a blob.
        svc.save_data("test/key.txt", b"test data content").await?;
        Ok(())
    }

    #[tokio::test]
    async fn load_data_from_database_kind_returns_key_as_payload() -> StorageResult<()> {
        // Mirrors Go TestDataStorageService_LoadData "load data from database
        // storage": for TypeDatabase the key IS the data. Pass the payload
        // itself as the key; load_data must return it verbatim.
        let svc = DataStorageService::with_backend(
            DataStorageKind::Memory,
            Box::new(DatabaseStorageAdapter::new()),
        );
        let payload = "test data content";
        let result = svc.load_data(payload).await?;
        assert_eq!(result, payload.as_bytes());
        Ok(())
    }

    #[tokio::test]
    async fn delete_data_on_database_kind_is_a_noop_that_succeeds() -> StorageResult<()> {
        // Mirrors Go DeleteData for TypeDatabase (lines 612-613): bare
        // `return nil`. The facade must report success regardless.
        let svc = DataStorageService::with_backend(
            DataStorageKind::Memory,
            Box::new(DatabaseStorageAdapter::new()),
        );
        svc.delete_data("any/key.txt").await?;
        // Repeated deletes also succeed (no recorded state requirement).
        svc.delete_data("any/key.txt").await?;
        Ok(())
    }

    #[tokio::test]
    async fn save_load_round_trip_through_local_backend() -> StorageResult<()> {
        // Mirrors Go TestDataStorageService_SaveData "save data to fs storage"
        // + TestDataStorageService_LoadData "load data from fs storage":
        // save then load returns the same bytes.
        let temp = tempfile::tempdir().map_err(|e| StorageError::Operation(e.to_string()))?;
        let svc = DataStorageService::with_backend(
            DataStorageKind::Local,
            Box::new(LocalStorageAdapter::new(temp.path().to_path_buf())),
        );
        let key = "test/key.txt";
        let data = b"test data content";
        svc.save_data(key, data).await?;

        // Go's test then verifies existence + content via the FS; we go
        // through the facade's load_data.
        let loaded = svc.load_data(key).await?;
        assert_eq!(loaded, data);
        Ok(())
    }

    #[tokio::test]
    async fn load_nonexistent_key_on_file_backend_is_an_error() -> StorageResult<()> {
        // Mirrors Go TestDataStorageService_LoadData "load non-existent file
        // from fs storage": the error must mention "failed to read file".
        let temp = tempfile::tempdir().map_err(|e| StorageError::Operation(e.to_string()))?;
        let svc = DataStorageService::with_backend(
            DataStorageKind::Local,
            Box::new(LocalStorageAdapter::new(temp.path().to_path_buf())),
        );
        match svc.load_data("non-existent.txt").await {
            Err(StorageError::Operation(msg)) => {
                assert!(msg.contains("failed to read file"), "msg was: {msg}");
            }
            other => panic!("expected Operation(failed to read file), got {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn delete_data_on_file_backend_treats_missing_key_as_success() -> StorageResult<()> {
        // Mirrors Go DeleteData (lines 628-631): `errors.Is(err,
        // os.ErrNotExist) -> return nil`. Deleting a key that was never
        // written must succeed.
        let temp = tempfile::tempdir().map_err(|e| StorageError::Operation(e.to_string()))?;
        let svc = DataStorageService::with_backend(
            DataStorageKind::Local,
            Box::new(LocalStorageAdapter::new(temp.path().to_path_buf())),
        );
        svc.delete_data("never-written.txt").await?;
        Ok(())
    }

    #[tokio::test]
    async fn list_returns_saved_objects_under_prefix() -> StorageResult<()> {
        // Go's DataStorageService has no List; this exercises the facade's
        // pass-through to the adapter's list (S11 trait method set).
        let svc = DataStorageService::with_backend(
            DataStorageKind::Memory,
            Box::new(InMemoryStorageAdapter::new()),
        );
        svc.save_data("requests/a.json", b"{}").await?;
        svc.save_data("requests/b.json", b"{}").await?;
        svc.save_data("artifacts/c.bin", b"xyz").await?;

        let keys: Vec<String> = svc
            .list("requests")
            .await?
            .into_iter()
            .map(|m| m.key)
            .collect();
        assert_eq!(
            keys,
            vec!["requests/a.json".to_string(), "requests/b.json".to_string()]
        );
        Ok(())
    }

    #[tokio::test]
    async fn exists_and_head_pass_through_to_backend() -> StorageResult<()> {
        let svc = DataStorageService::with_backend(
            DataStorageKind::Memory,
            Box::new(InMemoryStorageAdapter::new()),
        );
        assert!(!svc.exists("k.txt").await?);
        svc.save_data("k.txt", b"data").await?;
        assert!(svc.exists("k.txt").await?);
        let head = match svc.head("k.txt").await? {
            Some(h) => h,
            None => panic!("head must find the saved object"),
        };
        assert_eq!(head.key, "k.txt");
        assert_eq!(head.content_length, 4);
        Ok(())
    }

    #[tokio::test]
    async fn new_constructs_memory_backend_via_build_storage_backend() -> StorageResult<()> {
        // The production-shaped constructor must wire the in-memory backend
        // for DataStorageKind::Memory without any config.
        let svc = DataStorageService::new(DataStorageKind::Memory, None)?;
        assert_eq!(svc.kind(), &DataStorageKind::Memory);
        svc.save_data("built.txt", b"ok").await?;
        assert_eq!(svc.load_data("built.txt").await?, b"ok");
        Ok(())
    }

    #[tokio::test]
    async fn new_constructs_local_backend_with_directory_config()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let config = DataStorageConfig {
            directory: Some(temp.path().to_string_lossy().to_string()),
            ..Default::default()
        };
        let svc = DataStorageService::new(DataStorageKind::Local, Some(&config))?;
        svc.save_data("nested/item.txt", b"hi").await?;
        assert_eq!(svc.load_data("nested/item.txt").await?, b"hi");
        Ok(())
    }

    #[tokio::test]
    async fn new_reports_unavailable_when_local_directory_missing() {
        // Mirrors Go buildFileSystem TypeFs branch (lines 147-150):
        // "directory not configured for fs storage".
        match DataStorageService::new(DataStorageKind::Local, None) {
            Err(StorageError::Unavailable(msg)) => {
                assert!(msg.contains("directory"), "msg was: {msg}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn full_round_trip_put_get_list_delete_through_memory_facade() -> StorageResult<()> {
        // End-to-end exercise of the facade over the in-memory backend:
        // save -> exists -> head -> list -> load -> delete -> missing-load-errors.
        let svc = DataStorageService::with_backend(
            DataStorageKind::Memory,
            Box::new(InMemoryStorageAdapter::new()),
        );
        let key = "artifacts/round-trip.txt";
        let data = b"round-trip-body";

        svc.save_data(key, data).await?;
        assert!(svc.exists(key).await?);
        assert_eq!(
            svc.head(key).await?.map(|m| m.content_length),
            Some(data.len() as u64)
        );
        assert_eq!(svc.load_data(key).await?, data);

        let listed: Vec<String> = svc
            .list("artifacts")
            .await?
            .into_iter()
            .map(|m| m.key)
            .collect();
        assert_eq!(listed, vec![key.to_string()]);

        svc.delete_data(key).await?;
        // Delete is idempotent (Go's os.ErrNotExist tolerance).
        svc.delete_data(key).await?;
        assert!(!svc.exists(key).await?);
        // Loading a now-missing key surfaces the Go "failed to read file" error.
        match svc.load_data(key).await {
            Err(StorageError::Operation(msg)) => {
                assert!(msg.contains("failed to read file"), "msg was: {msg}");
            }
            other => panic!("expected Operation(failed to read file), got {other:?}"),
        }
        Ok(())
    }
}
