use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;

pub type StorageResult<T> = Result<T, StorageError>;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("invalid storage key: {0}")]
    InvalidKey(String),
    #[error("storage backend unavailable: {0}")]
    Unavailable(String),
    #[error("storage operation failed: {0}")]
    Operation(String),
    #[error("storage lock poisoned: {0}")]
    LockPoisoned(&'static str),
    #[error("storage metadata serialization failed: {0}")]
    Serialization(String),
    /// Returned by [`StorageAdapter::presign`] when the backend does not
    /// support pre-signed URLs (e.g. local filesystem). Mirrors the "optional"
    /// nature of presign called out in RUST-P13-001 S11.
    #[error("storage backend does not support presigned URLs")]
    Unsupported,
    /// Returned by [`validate_single_primary`] when more than one storage row
    /// is marked primary. Mirrors the S15 invariant ("primary storage 只能有一个").
    #[error("multiple primary data storages detected: {0}")]
    MultiplePrimary(String),
}

impl From<std::io::Error> for StorageError {
    fn from(error: std::io::Error) -> Self {
        Self::Operation(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageMetadata {
    pub key: String,
    pub content_length: u64,
    pub content_type: Option<String>,
    #[serde(default)]
    pub custom: BTreeMap<String, String>,
}

impl StorageMetadata {
    pub fn new(key: impl Into<String>, content_length: u64) -> Self {
        Self {
            key: key.into(),
            content_length,
            content_type: None,
            custom: BTreeMap::new(),
        }
    }

    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    fn normalized_for(mut self, key: &str, content_length: usize) -> StorageResult<Self> {
        self.key = normalize_key(key)?;
        self.content_length = content_length as u64;
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageObject {
    pub metadata: StorageMetadata,
    pub bytes: Vec<u8>,
}

impl StorageObject {
    pub fn new(key: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        let bytes = bytes.into();
        let metadata = StorageMetadata::new(key, bytes.len() as u64);
        Self { metadata, bytes }
    }

    pub fn with_metadata(mut self, metadata: StorageMetadata) -> Self {
        self.metadata = metadata;
        self
    }

    fn normalized(self) -> StorageResult<Self> {
        let key = self.metadata.key.clone();
        let metadata = self.metadata.normalized_for(&key, self.bytes.len())?;
        Ok(Self {
            metadata,
            bytes: self.bytes,
        })
    }
}

/// Unified storage adapter trait. Mirrors the operations exposed by Go's
/// `DataStorageService` (`SaveData`/`LoadData`/`DeleteData`) plus the listing
/// and pre-signing surface required by RUST-P13-001 S11. Every backend
/// (`memory`/`local`/`s3`/`gcs`/`webdav`) shares this single trait and the
/// unified [`StorageError`] model, so that higher layers can swap backends
/// without touching call sites.
///
/// Method set per S11: `put` / `get` / `delete` / `exists` / `presign` /
/// `list`. `head` is retained as a primary method because the local backend
/// persists sidecar metadata that `exists` alone cannot surface; `exists` has
/// a default delegation to `head` so most backends only implement five
/// methods.
#[async_trait]
pub trait StorageAdapter: Send + Sync {
    async fn put(&self, object: StorageObject) -> StorageResult<StorageMetadata>;

    async fn get(&self, key: &str) -> StorageResult<Option<StorageObject>>;

    async fn delete(&self, key: &str) -> StorageResult<bool>;

    async fn head(&self, key: &str) -> StorageResult<Option<StorageMetadata>>;

    async fn list(&self, prefix: &str) -> StorageResult<Vec<StorageMetadata>>;

    /// Whether an object exists at `key`. Default delegates to [`head`]; backends
    /// with a cheaper existence probe (e.g. S3 `HeadObject`) may override.
    async fn exists(&self, key: &str) -> StorageResult<bool> {
        Ok(self.head(key).await?.is_some())
    }

    /// Produce a pre-signed URL for `key` valid for `ttl` seconds. Backends
    /// that cannot pre-sign (memory/local) keep the default which returns
    /// [`StorageError::Unsupported`], matching the "optional presign" wording
    /// of RUST-P13-001 S11.
    async fn presign(&self, _key: &str, _ttl: u64) -> StorageResult<String> {
        Err(StorageError::Unsupported)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataStorageKind {
    Memory,
    Local,
    S3,
    Gcs,
    WebDav,
    Unknown(String),
}

impl DataStorageKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Memory => "memory",
            Self::Local => "local",
            Self::S3 => "s3",
            Self::Gcs => "gcs",
            Self::WebDav => "webdav",
            Self::Unknown(kind) => kind,
        }
    }
}

impl Serialize for DataStorageKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DataStorageKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let kind = String::deserialize(deserializer)?;
        Ok(match kind.trim().to_ascii_lowercase().as_str() {
            "memory" | "fake" | "in_memory" | "in-memory" => Self::Memory,
            "local" | "filesystem" | "fs" => Self::Local,
            "s3" => Self::S3,
            "gcs" | "google_cloud_storage" => Self::Gcs,
            "webdav" | "web_dav" => Self::WebDav,
            _ if kind.trim().is_empty() => {
                return Err(de::Error::custom("storage kind cannot be empty"));
            }
            _ => Self::Unknown(kind),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataStorageSettings {
    pub kind: DataStorageKind,
    #[serde(default, flatten)]
    pub settings: BTreeMap<String, Value>,
}

impl DataStorageSettings {
    pub fn new(kind: DataStorageKind) -> Self {
        Self {
            kind,
            settings: BTreeMap::new(),
        }
    }
}

/// Resolve an object `key` against a local filesystem `root`, returning the
/// absolute path the local adapter should read or write. **Security-critical**
/// (RUST-P13-001 S13): the resolved path is guaranteed to stay inside `root`
/// — any `..`, absolute path, Windows drive prefix, or other escape attempt
/// returns [`StorageError::InvalidKey`] before any filesystem touch.
///
/// This is the standalone guard behind [`LocalStorageAdapter`]'s path
/// computation; exposing it lets higher layers (and tests) verify the
/// containment invariant without going through the adapter.
///
/// Containment is enforced lexically rather than via `canonicalize`: the
/// underlying [`normalize_key`] rejects every component that could escape
/// (`..`, `.`, absolute prefixes, drive letters, embedded separators), so the
/// joined path is provably a descendant of `root` by construction. We still
/// run a final `starts_with` check on the assembled path as defense in depth
/// against future refactors of `normalize_key`. We deliberately avoid
/// `Path::canonicalize` here because it (a) requires filesystem I/O on every
/// resolution and (b) on Windows mutates the path with a `\\?\` UNC prefix
/// that would make the containment check spuriously fail when `root` and the
/// constructed path differ in canonical form.
///
/// **Symlink caveat:** lexical containment cannot detect a symlink placed
/// *inside* `root` that points outside it. [`LocalStorageAdapter`] therefore
/// layers the runtime guard [`assert_no_symlink_escape`] on top of this
/// resolver before every filesystem touch.
pub fn resolve_local_object_key(root: &Path, key: &str) -> StorageResult<PathBuf> {
    let normalized = normalize_key(key)?;
    let mut resolved = root.to_path_buf();
    for part in normalized.split('/') {
        resolved.push(part);
    }

    // Defense in depth: `normalize_key` already makes escape impossible, but
    // we re-assert containment lexically so a future refactor cannot silently
    // reopen a traversal hole. Use `Path::components` comparison so prefix
    // mismatches (e.g. `\\?\` vs raw) don't cause false rejections.
    if !is_lexically_within(&resolved, root) {
        return Err(StorageError::InvalidKey(key.to_string()));
    }
    Ok(resolved)
}

/// Runtime symlink-escape guard (P-19): assert that `target` — after resolving
/// every symlink the filesystem currently contains — still lives under `root`.
///
/// [`resolve_local_object_key`] proves lexical containment, but a symlink
/// planted *inside* the storage root (e.g. `objects/evil -> /etc`) redirects
/// I/O outside the sandbox while passing every lexical check. This guard
/// closes that hole: it canonicalizes `root` and the deepest **existing**
/// ancestor of `target` (`canonicalize` requires existence, and for
/// "create new file" the leaf does not exist yet — the not-yet-existing tail
/// components cannot contain symlinks precisely because they do not exist)
/// and rejects with [`StorageError::InvalidKey`] when the resolved prefix
/// escapes the resolved root.
///
/// A non-existent `root` is trivially safe (nothing under it can exist, so no
/// symlink can redirect) and returns `Ok`. There is an unavoidable
/// check-then-use race with a concurrent symlink swap; this is defense in
/// depth on top of the lexical guard, matching the threat model of an
/// attacker who pre-plants symlinks rather than one who races the runtime.
///
/// Go parity note: the Go side (`afero.NewBasePathFs` in
/// `conduit/internal/server/biz/data_storage.go`) is lexical-only and does
/// **not** resolve symlinks — this is a deliberate Rust-side hardening.
async fn assert_no_symlink_escape(root: &Path, target: &Path, key: &str) -> StorageResult<()> {
    let canonical_root = match tokio::fs::canonicalize(root).await {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    // Walk up from `target` to the deepest existing ancestor. The loop
    // terminates because `target` is lexically under `root` and `root` itself
    // exists (its canonicalize succeeded above).
    let mut probe = target.to_path_buf();
    let canonical_probe = loop {
        match tokio::fs::canonicalize(&probe).await {
            Ok(path) => break path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => match probe.parent() {
                Some(parent) => probe = parent.to_path_buf(),
                None => return Err(StorageError::InvalidKey(key.to_string())),
            },
            Err(error) => return Err(error.into()),
        }
    };

    if !is_lexically_within(&canonical_probe, &canonical_root) {
        return Err(StorageError::InvalidKey(key.to_string()));
    }
    Ok(())
}

/// Lexical containment check: true iff `path` is `root` itself or a descendant
/// of `root`, comparing path components without touching the filesystem.
fn is_lexically_within(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components().peekable();
    let mut root_components = root.components().peekable();
    loop {
        match (path_components.next(), root_components.next()) {
            (_, None) => return true,
            (Some(pc), Some(rc)) => {
                if pc != rc {
                    return false;
                }
            }
            (None, Some(_)) => return false,
        }
    }
}

/// Mask sensitive fields inside a `DataStorage` settings JSON blob (S10 /
/// RUST-P13-001 S14 logging). Mirrors the Go credential fields in
/// `conduit/internal/objects/data_stograge.go`:
/// `S3.accessKey`, `S3.secretKey`, `GCS.credential`, `WebDAV.password`,
/// `DSN` (database connection string), plus the generic
/// `secret`/`password`/`token` keys for forward compatibility. Each redacted
/// value is replaced with the literal string `"***"`; non-string values and
/// unknown fields are passed through untouched so the structure round-trips.
///
/// Operates on `serde_json::Value` so callers can apply it to either the
/// flattened `DataStorageSettings` JSON or any nested backend subtree without
/// deserializing into a typed struct.
pub fn mask_storage_credentials(settings: &Value) -> Value {
    mask_value(settings)
}

fn mask_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let masked: serde_json::Map<String, Value> = map
                .iter()
                .map(|(k, v)| {
                    let masked_v = if is_sensitive_key(k) && v.is_string() {
                        Value::String((*STARS).to_string())
                    } else {
                        mask_value(v)
                    };
                    (k.clone(), masked_v)
                })
                .collect();
            Value::Object(masked)
        }
        Value::Array(items) => Value::Array(items.iter().map(mask_value).collect()),
        other => other.clone(),
    }
}

/// Names treated as credentials regardless of camelCase / snake_case spelling.
/// A key is sensitive when EITHER:
///   - its lowercase form exactly matches a sensitive stem
///     (`secret`, `password`, `token`, `credential`, `dsn`, ...) — catches the
///     Go json tags `password`, `credential`, `dsn`, `token`; OR
///   - its lowercase form with separators (`_`, `-`, camel boundaries) stripped
///     matches a known compound credential name (`accesskey`, `secretkey`,
///     `apikey`, `privatekey`, `accesstoken`, `refreshtoken`, `clientsecret`).
///
/// Deliberately narrow to avoid false-positive redaction of unrelated fields:
/// `non_secret` / `secret_count` / `public_key` are NOT redacted.
fn is_sensitive_key(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if SENSITIVE_KEY_STEMS.iter().any(|stem| lower == *stem) {
        return true;
    }
    let compact: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
    SENSITIVE_COMPOUND_NAMES.iter().any(|stem| compact == *stem)
}

const STARS: &str = "***";
/// Exact-match sensitive stems (lowercase). Covers every credential field in
/// `conduit/internal/objects/data_stograge.go` plus common standalone names.
const SENSITIVE_KEY_STEMS: &[&str] = &[
    "secret",
    "secrets",
    "password",
    "passwd",
    "pwd",
    "token",
    "tokens",
    "credential",
    "credentials",
    "dsn",
];
/// Compound credential names matched against the separator-stripped lowercase
/// key. Adding entries here is safe; adding a stem to `SENSITIVE_KEY_STEMS`
/// risks false positives on unrelated keys that merely contain that word.
const SENSITIVE_COMPOUND_NAMES: &[&str] = &[
    "accesskey",
    "secretkey",
    "secretaccesskey",
    "apikey",
    "apisecret",
    "clientsecret",
    "privatekey",
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "authtoken",
    "bearertoken",
];

/// Lightweight view of a `DataStorage` row used by [`validate_single_primary`].
/// Only the fields the invariant needs are required, so callers can build this
/// from any concrete storage type without a hard dependency on the ent layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataStorageRow {
    pub id: i64,
    pub name: String,
    pub primary: bool,
}

/// Enforce the S15 invariant: **at most one** data storage may carry
/// `primary = true`. Mirrors the Go semantics where `GetPrimaryDataStorage`
/// does `Where(datastorage.Primary(true)).First(...)` and would otherwise
/// silently return an arbitrary row when multiple exist. Returning the names
/// of the offending rows in the error keeps the failure actionable.
pub fn validate_single_primary<'a, I>(storages: I) -> StorageResult<()>
where
    I: IntoIterator<Item = &'a DataStorageRow>,
{
    let primaries: Vec<&DataStorageRow> = storages.into_iter().filter(|s| s.primary).collect();
    if primaries.len() > 1 {
        let names: Vec<&str> = primaries.iter().map(|s| s.name.as_str()).collect();
        return Err(StorageError::MultiplePrimary(names.join(", ")));
    }
    Ok(())
}

#[derive(Debug, Default)]
pub struct InMemoryStorageAdapter {
    objects: Mutex<BTreeMap<String, StorageObject>>,
}

impl InMemoryStorageAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl StorageAdapter for InMemoryStorageAdapter {
    async fn put(&self, object: StorageObject) -> StorageResult<StorageMetadata> {
        let object = object.normalized()?;
        let metadata = object.metadata.clone();
        self.objects
            .lock()
            .map_err(|_| StorageError::LockPoisoned("in-memory storage"))?
            .insert(metadata.key.clone(), object);
        Ok(metadata)
    }

    async fn get(&self, key: &str) -> StorageResult<Option<StorageObject>> {
        let key = normalize_key(key)?;
        Ok(self
            .objects
            .lock()
            .map_err(|_| StorageError::LockPoisoned("in-memory storage"))?
            .get(&key)
            .cloned())
    }

    async fn delete(&self, key: &str) -> StorageResult<bool> {
        let key = normalize_key(key)?;
        Ok(self
            .objects
            .lock()
            .map_err(|_| StorageError::LockPoisoned("in-memory storage"))?
            .remove(&key)
            .is_some())
    }

    async fn head(&self, key: &str) -> StorageResult<Option<StorageMetadata>> {
        let key = normalize_key(key)?;
        Ok(self
            .objects
            .lock()
            .map_err(|_| StorageError::LockPoisoned("in-memory storage"))?
            .get(&key)
            .map(|object| object.metadata.clone()))
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<StorageMetadata>> {
        let prefix = normalize_prefix(prefix)?;
        Ok(self
            .objects
            .lock()
            .map_err(|_| StorageError::LockPoisoned("in-memory storage"))?
            .iter()
            .filter(|(key, _object)| key.starts_with(&prefix))
            .map(|(_key, object)| object.metadata.clone())
            .collect())
    }
}

#[derive(Debug, Clone)]
pub struct LocalStorageAdapter {
    root: PathBuf,
}

impl LocalStorageAdapter {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn object_root(&self) -> PathBuf {
        self.root.join("objects")
    }

    fn metadata_root(&self) -> PathBuf {
        self.root.join("metadata")
    }

    fn object_path(&self, key: &str) -> StorageResult<PathBuf> {
        resolve_local_object_key(&self.object_root(), key)
    }

    fn metadata_path(&self, key: &str) -> StorageResult<PathBuf> {
        let mut path = resolve_local_object_key(&self.metadata_root(), key)?;
        let file_name = path
            .file_name()
            .ok_or_else(|| StorageError::InvalidKey(key.to_string()))?
            .to_string_lossy();
        path.set_file_name(format!("{file_name}.json"));
        Ok(path)
    }

    /// Resolve `key` under the given per-kind root and run the P-19 symlink
    /// guard so the returned path is safe to hand to the filesystem.
    async fn guarded_path(&self, path: PathBuf, key: &str) -> StorageResult<PathBuf> {
        assert_no_symlink_escape(&self.root, &path, key).await?;
        Ok(path)
    }

    async fn read_metadata(&self, key: &str) -> StorageResult<Option<StorageMetadata>> {
        let path = self.guarded_path(self.metadata_path(key)?, key).await?;
        match tokio::fs::read(path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| StorageError::Serialization(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

#[async_trait]
impl StorageAdapter for LocalStorageAdapter {
    async fn put(&self, object: StorageObject) -> StorageResult<StorageMetadata> {
        let object = object.normalized()?;
        // Guard BEFORE create_dir_all: a pre-planted symlink in an existing
        // ancestor must be caught before any directory is created through it.
        let object_path = self
            .guarded_path(
                self.object_path(&object.metadata.key)?,
                &object.metadata.key,
            )
            .await?;
        let metadata_path = self
            .guarded_path(
                self.metadata_path(&object.metadata.key)?,
                &object.metadata.key,
            )
            .await?;

        if let Some(parent) = object_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if let Some(parent) = metadata_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::write(object_path, &object.bytes).await?;
        let metadata_bytes = serde_json::to_vec_pretty(&object.metadata)
            .map_err(|error| StorageError::Serialization(error.to_string()))?;
        tokio::fs::write(metadata_path, metadata_bytes).await?;
        Ok(object.metadata)
    }

    async fn get(&self, key: &str) -> StorageResult<Option<StorageObject>> {
        let key = normalize_key(key)?;
        let object_path = self.guarded_path(self.object_path(&key)?, &key).await?;
        let bytes = match tokio::fs::read(object_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };

        let metadata = match self.read_metadata(&key).await? {
            Some(metadata) => metadata.normalized_for(&key, bytes.len())?,
            None => StorageMetadata::new(key, bytes.len() as u64),
        };

        Ok(Some(StorageObject { metadata, bytes }))
    }

    async fn delete(&self, key: &str) -> StorageResult<bool> {
        let key = normalize_key(key)?;
        let object_path = self.guarded_path(self.object_path(&key)?, &key).await?;
        let metadata_path = self.guarded_path(self.metadata_path(&key)?, &key).await?;

        let removed_object = remove_file_if_exists(&object_path).await?;
        let _removed_metadata = remove_file_if_exists(&metadata_path).await?;
        Ok(removed_object)
    }

    async fn head(&self, key: &str) -> StorageResult<Option<StorageMetadata>> {
        let key = normalize_key(key)?;
        if let Some(metadata) = self.read_metadata(&key).await? {
            return Ok(Some(metadata));
        }

        let object_path = self.guarded_path(self.object_path(&key)?, &key).await?;
        match tokio::fs::metadata(object_path).await {
            Ok(metadata) if metadata.is_file() => {
                Ok(Some(StorageMetadata::new(key, metadata.len())))
            }
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn list(&self, prefix: &str) -> StorageResult<Vec<StorageMetadata>> {
        let prefix = normalize_prefix(prefix)?;
        let mut result = Vec::new();
        let object_root = self.object_root();
        match tokio::fs::metadata(&object_root).await {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
            Err(error) => return Err(error.into()),
        }

        for path in object_files(&object_root).await? {
            let key = key_from_path(&object_root, &path)?;
            if key.starts_with(&prefix) {
                // Reuse head so missing sidecar metadata still produces a sane entry.
                if let Some(metadata) = self.head(&key).await? {
                    result.push(metadata);
                }
            }
        }

        result.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(result)
    }
}

pub(crate) fn normalize_key(key: &str) -> StorageResult<String> {
    if key.is_empty() {
        return Err(StorageError::InvalidKey(key.to_string()));
    }

    let mut parts = Vec::new();
    for component in Path::new(key).components() {
        match component {
            Component::Normal(part) => parts.push(os_str_to_string(part)?),
            _ => return Err(StorageError::InvalidKey(key.to_string())),
        }
    }

    validate_object_key_parts(key, &parts)?;
    Ok(parts.join("/"))
}

fn validate_object_key_parts(original: &str, parts: &[String]) -> StorageResult<()> {
    if parts.is_empty()
        || original.starts_with('/')
        || original.starts_with('\\')
        || original.ends_with('/')
        || original.ends_with('\\')
        || original.contains('\\')
        || has_windows_drive_prefix(original)
    {
        return Err(StorageError::InvalidKey(original.to_string()));
    }

    if original
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(StorageError::InvalidKey(original.to_string()));
    }

    for part in parts {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.contains('/')
            || part.contains('\\')
        {
            return Err(StorageError::InvalidKey(original.to_string()));
        }
    }

    Ok(())
}

fn has_windows_drive_prefix(key: &str) -> bool {
    let bytes = key.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn normalize_prefix(prefix: &str) -> StorageResult<String> {
    if prefix.is_empty() {
        return Ok(String::new());
    }
    normalize_key(prefix)
}

async fn remove_file_if_exists(path: &Path) -> StorageResult<bool> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn object_files(root: &Path) -> StorageResult<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let file_type = entry.file_type().await?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                files.push(path);
            }
        }
    }

    Ok(files)
}

fn key_from_path(root: &Path, path: &Path) -> StorageResult<String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|error| StorageError::Operation(error.to_string()))?;
    let parts = relative
        .components()
        .map(|component| match component {
            Component::Normal(part) => os_str_to_string(part),
            _ => Err(StorageError::InvalidKey(path.display().to_string())),
        })
        .collect::<StorageResult<Vec<_>>>()?;
    normalize_key(&parts.join("/"))
}

fn os_str_to_string(value: &OsStr) -> StorageResult<String> {
    value
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| StorageError::InvalidKey(value.to_string_lossy().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn in_memory_adapter_put_get_delete_head_and_list() -> StorageResult<()> {
        let adapter = InMemoryStorageAdapter::new();
        let metadata =
            StorageMetadata::new("requests/a.json", 0).with_content_type("application/json");
        let object = StorageObject::new("requests/a.json", br#"{"ok":true}"#.to_vec())
            .with_metadata(metadata);

        let saved = adapter.put(object).await?;
        assert_eq!(saved.content_length, 11);
        assert_eq!(adapter.head("requests/a.json").await?, Some(saved.clone()));

        let loaded = match adapter.get("requests/a.json").await? {
            Some(loaded) => loaded,
            None => panic!("expected stored in-memory object"),
        };
        assert_eq!(loaded.bytes, br#"{"ok":true}"#);
        assert_eq!(
            loaded.metadata.content_type.as_deref(),
            Some("application/json")
        );

        let listed = adapter.list("requests").await?;
        assert_eq!(listed, vec![saved]);
        assert!(adapter.delete("requests/a.json").await?);
        assert_eq!(adapter.head("requests/a.json").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn local_adapter_put_get_delete_head_and_list() -> StorageResult<()> {
        let temp_dir =
            tempfile::tempdir().map_err(|error| StorageError::Operation(error.to_string()))?;
        let adapter = LocalStorageAdapter::new(temp_dir.path());
        let mut metadata =
            StorageMetadata::new("artifacts/item.txt", 999).with_content_type("text/plain");
        metadata
            .custom
            .insert("request_id".to_string(), "req-1".to_string());

        let saved = adapter
            .put(
                StorageObject::new("artifacts/item.txt", b"hello".to_vec()).with_metadata(metadata),
            )
            .await?;
        assert_eq!(saved.content_length, 5);
        assert_eq!(
            adapter.head("artifacts/item.txt").await?,
            Some(saved.clone())
        );

        let loaded = match adapter.get("artifacts/item.txt").await? {
            Some(loaded) => loaded,
            None => panic!("expected stored local object"),
        };
        assert_eq!(loaded.bytes, b"hello");
        assert_eq!(
            loaded.metadata.custom.get("request_id"),
            Some(&"req-1".to_string())
        );
        assert_eq!(adapter.list("artifacts").await?, vec![saved]);

        assert!(adapter.delete("artifacts/item.txt").await?);
        assert_eq!(adapter.get("artifacts/item.txt").await?, None);
        assert_eq!(adapter.head("artifacts/item.txt").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn local_adapter_accepts_safe_nested_object_key() -> StorageResult<()> {
        let temp_dir =
            tempfile::tempdir().map_err(|error| StorageError::Operation(error.to_string()))?;
        let adapter = LocalStorageAdapter::new(temp_dir.path());

        let saved = adapter
            .put(StorageObject::new(
                "safe/nested/item.txt",
                b"hello".to_vec(),
            ))
            .await?;

        assert_eq!(saved.key, "safe/nested/item.txt");
        assert!(adapter.object_path("safe/nested/item.txt")?.is_file());
        assert!(adapter.metadata_path("safe/nested/item.txt")?.is_file());
        Ok(())
    }

    #[tokio::test]
    async fn local_adapter_rejects_path_traversal_key() -> StorageResult<()> {
        let temp_dir =
            tempfile::tempdir().map_err(|error| StorageError::Operation(error.to_string()))?;
        let adapter = LocalStorageAdapter::new(temp_dir.path());

        let res = adapter
            .put(StorageObject::new("../escape.txt", b"escape".to_vec()))
            .await;
        match res {
            Err(error) => assert!(matches!(error, StorageError::InvalidKey(_))),
            Ok(v) => panic!("expected error, got {v:?}"),
        }

        assert!(!temp_dir.path().join("escape.txt").exists());
        Ok(())
    }

    #[tokio::test]
    async fn local_adapter_rejects_absolute_object_key() -> StorageResult<()> {
        let temp_dir =
            tempfile::tempdir().map_err(|error| StorageError::Operation(error.to_string()))?;
        let adapter = LocalStorageAdapter::new(temp_dir.path());

        for key in ["/escape.txt", r"\escape.txt"] {
            let res = adapter
                .put(StorageObject::new(key, b"escape".to_vec()))
                .await;
            match res {
                Err(error) => assert!(matches!(error, StorageError::InvalidKey(_))),
                Ok(v) => panic!("expected error, got {v:?}"),
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn local_adapter_rejects_windows_drive_object_key() -> StorageResult<()> {
        let temp_dir =
            tempfile::tempdir().map_err(|error| StorageError::Operation(error.to_string()))?;
        let adapter = LocalStorageAdapter::new(temp_dir.path());

        let res = adapter
            .put(StorageObject::new(r"C:\escape.txt", b"escape".to_vec()))
            .await;
        match res {
            Err(error) => assert!(matches!(error, StorageError::InvalidKey(_))),
            Ok(v) => panic!("expected error, got {v:?}"),
        }

        Ok(())
    }

    #[tokio::test]
    async fn local_adapter_rejects_keys_requiring_normalization() -> StorageResult<()> {
        let temp_dir =
            tempfile::tempdir().map_err(|error| StorageError::Operation(error.to_string()))?;
        let adapter = LocalStorageAdapter::new(temp_dir.path());

        for key in ["safe//item.txt", "safe/./item.txt", "safe/item.txt/"] {
            let res = adapter
                .put(StorageObject::new(key, b"normalized".to_vec()))
                .await;
            match res {
                Err(error) => assert!(matches!(error, StorageError::InvalidKey(_))),
                Ok(v) => panic!("expected error, got {v:?}"),
            }
        }
        Ok(())
    }

    #[test]
    fn data_storage_settings_round_trip_preserves_unknown_settings() -> Result<(), serde_json::Error>
    {
        let input = json!({
            "kind": "new_backend",
            "endpoint": "https://storage.example.test",
            "headers": {"x-test": "1"},
            "future_flag": true
        });

        let settings: DataStorageSettings = serde_json::from_value(input.clone())?;
        assert_eq!(
            settings.kind,
            DataStorageKind::Unknown("new_backend".to_string())
        );

        let output = serde_json::to_value(settings)?;
        assert_eq!(output, input);
        Ok(())
    }

    // ---------------------------------------------------------------------------
    // RUST-P13-001 S10 — credential mask
    // ---------------------------------------------------------------------------

    #[test]
    fn mask_storage_credentials_redacts_known_backend_secrets() {
        let input = json!({
            "dsn": "postgres://user:hunter2@db:5432/app",
            "directory": "/var/data",
            "s3": {
                "bucketName": "logs",
                "endpoint": "s3.example.com",
                "region": "us-east-1",
                "accessKey": "AKIAEXAMPLE",
                "secretKey": "s3kr3t",
                "pathStyle": true
            },
            "gcs": {
                "bucketName": "gcs-logs",
                "credential": "{\"type\":\"service_account\"}"
            },
            "webdav": {
                "url": "https://dav.example.com",
                "username": "alice",
                "password": "letmein",
                "path": "/dav"
            }
        });

        let masked = mask_storage_credentials(&input);

        // Non-sensitive fields preserved verbatim.
        assert_eq!(
            masked.get("directory").and_then(|v| v.as_str()),
            Some("/var/data")
        );
        assert_eq!(
            masked
                .get("s3")
                .and_then(|v| v.get("bucketName"))
                .and_then(|v| v.as_str()),
            Some("logs")
        );
        assert_eq!(
            masked
                .get("s3")
                .and_then(|v| v.get("pathStyle"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );

        // Each Go-defined credential field is redacted.
        assert_eq!(masked.get("dsn").and_then(|v| v.as_str()), Some("***"));
        assert_eq!(
            masked
                .get("s3")
                .and_then(|v| v.get("accessKey"))
                .and_then(|v| v.as_str()),
            Some("***")
        );
        assert_eq!(
            masked
                .get("s3")
                .and_then(|v| v.get("secretKey"))
                .and_then(|v| v.as_str()),
            Some("***")
        );
        assert_eq!(
            masked
                .get("gcs")
                .and_then(|v| v.get("credential"))
                .and_then(|v| v.as_str()),
            Some("***")
        );
        assert_eq!(
            masked
                .get("webdav")
                .and_then(|v| v.get("password"))
                .and_then(|v| v.as_str()),
            Some("***")
        );
        // webdav.username is NOT a credential and must survive.
        assert_eq!(
            masked
                .get("webdav")
                .and_then(|v| v.get("username"))
                .and_then(|v| v.as_str()),
            Some("alice")
        );
    }

    #[test]
    fn mask_storage_credentials_handles_generic_secret_names_and_arrays() {
        let input = json!({
            "items": [
                {"secret": "s", "label": "keep"},
                {"token": "t", "count": 7}
            ],
            "APIKey": "ABCDEF",
            "access_token": "atok",
            "non_secret": "plain"
        });

        let masked = mask_storage_credentials(&input);
        let items = match masked.get("items").and_then(|v| v.as_array()) {
            Some(v) => v,
            None => panic!("items array should be preserved"),
        };
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get("secret").and_then(|v| v.as_str()), Some("***"));
        assert_eq!(items[0].get("label").and_then(|v| v.as_str()), Some("keep"));
        assert_eq!(items[1].get("token").and_then(|v| v.as_str()), Some("***"));
        assert_eq!(items[1].get("count").and_then(|v| v.as_i64()), Some(7));

        // Case-insensitive on the key name; non-string values pass through.
        assert_eq!(masked.get("APIKey").and_then(|v| v.as_str()), Some("***"));
        assert_eq!(
            masked.get("access_token").and_then(|v| v.as_str()),
            Some("***")
        );
        assert_eq!(
            masked.get("non_secret").and_then(|v| v.as_str()),
            Some("plain")
        );
    }

    #[test]
    fn mask_storage_credentials_passes_non_object_through() {
        assert_eq!(mask_storage_credentials(&json!("string")), json!("string"));
        assert_eq!(mask_storage_credentials(&json!(42)), json!(42));
        assert_eq!(mask_storage_credentials(&json!(null)), json!(null));
        assert_eq!(mask_storage_credentials(&json!([1, 2])), json!([1, 2]));
    }

    // ---------------------------------------------------------------------------
    // RUST-P13-001 S13 — path traversal guard (standalone)
    // ---------------------------------------------------------------------------

    #[test]
    fn resolve_local_object_key_accepts_safe_nested_key() -> StorageResult<()> {
        let temp_dir = tempfile::tempdir().map_err(|e| StorageError::Operation(e.to_string()))?;
        let resolved = resolve_local_object_key(temp_dir.path(), "requests/abc/def.json")?;
        assert!(resolved.starts_with(temp_dir.path()));
        assert!(resolved.ends_with("requests/abc/def.json"));
        Ok(())
    }

    #[test]
    fn resolve_local_object_key_rejects_traversal_attempts() -> StorageResult<()> {
        let temp_dir = tempfile::tempdir().map_err(|e| StorageError::Operation(e.to_string()))?;
        for bad in [
            "../escape.txt",
            "safe/../../escape.txt",
            "/etc/passwd",
            r"C:\Windows\system32\config\sam",
            "safe/../other",
            "./here",
            "a//b",
            "trailing/",
        ] {
            match resolve_local_object_key(temp_dir.path(), bad) {
                Err(StorageError::InvalidKey(_)) => {}
                other => panic!("expected InvalidKey for {bad:?}, got {other:?}"),
            }
        }
        Ok(())
    }

    /// P-19: a symlink pre-planted inside the storage root that points outside
    /// it must not let put/get/delete/head escape the sandbox. Unix-only —
    /// creating symlinks on Windows requires elevated privileges.
    #[cfg(unix)]
    #[tokio::test]
    async fn local_adapter_rejects_symlink_escape() -> Result<(), Box<dyn std::error::Error>> {
        let outside_dir = tempfile::tempdir()?;
        let temp_dir = tempfile::tempdir()?;
        let adapter = LocalStorageAdapter::new(temp_dir.path());

        // Plant `objects/evil -> <outside_dir>` inside the storage root.
        std::fs::create_dir_all(temp_dir.path().join("objects"))?;
        std::os::unix::fs::symlink(outside_dir.path(), temp_dir.path().join("objects/evil"))?;

        // put through the symlink must be rejected and write nothing outside.
        match adapter
            .put(StorageObject::new("evil/escape.txt", b"pwned".to_vec()))
            .await
        {
            Err(StorageError::InvalidKey(_)) => {}
            other => panic!("expected InvalidKey for symlinked put, got {other:?}"),
        }
        assert!(!outside_dir.path().join("escape.txt").exists());

        // get through the symlink must be rejected, not leak outside content.
        std::fs::write(outside_dir.path().join("secret.txt"), b"secret")?;
        match adapter.get("evil/secret.txt").await {
            Err(StorageError::InvalidKey(_)) => {}
            other => panic!("expected InvalidKey for symlinked get, got {other:?}"),
        }

        // delete and head through the symlink must be rejected as well.
        match adapter.delete("evil/secret.txt").await {
            Err(StorageError::InvalidKey(_)) => {}
            other => panic!("expected InvalidKey for symlinked delete, got {other:?}"),
        }
        assert!(outside_dir.path().join("secret.txt").exists());
        match adapter.head("evil/secret.txt").await {
            Err(StorageError::InvalidKey(_)) => {}
            other => panic!("expected InvalidKey for symlinked head, got {other:?}"),
        }
        Ok(())
    }

    /// P-20 regression: the local adapter's put -> get -> delete round trip
    /// must work end-to-end on the async (tokio::fs) implementation.
    #[tokio::test]
    async fn local_adapter_async_roundtrip_put_get_delete() -> StorageResult<()> {
        let temp_dir =
            tempfile::tempdir().map_err(|error| StorageError::Operation(error.to_string()))?;
        let adapter = LocalStorageAdapter::new(temp_dir.path());

        adapter
            .put(StorageObject::new("round/trip.bin", b"async".to_vec()))
            .await?;
        let loaded = match adapter.get("round/trip.bin").await? {
            Some(loaded) => loaded,
            None => panic!("expected round-trip object to exist"),
        };
        assert_eq!(loaded.bytes, b"async");
        assert!(adapter.delete("round/trip.bin").await?);
        assert_eq!(adapter.get("round/trip.bin").await?, None);
        Ok(())
    }

    #[test]
    fn resolve_local_object_key_cannot_escape_root_via_symlink_friendly_input() -> StorageResult<()>
    {
        // The guard must reject any input that would canonicalize outside root.
        let temp_dir = tempfile::tempdir().map_err(|e| StorageError::Operation(e.to_string()))?;
        let root = temp_dir
            .path()
            .canonicalize()
            .map_err(|e| StorageError::Operation(e.to_string()))?;
        // Even with an existing root, ".." is rejected at normalize_key time.
        match resolve_local_object_key(&root, "../outside.txt") {
            Err(StorageError::InvalidKey(_)) => Ok(()),
            other => panic!("expected InvalidKey, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------
    // RUST-P13-001 S11 — unified trait surface (exists / presign)
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn in_memory_adapter_exists_and_presign_defaults() -> StorageResult<()> {
        let adapter = InMemoryStorageAdapter::new();
        // Nothing there yet.
        assert!(!adapter.exists("k.txt").await?);
        // Put and re-check via the default `exists` impl (delegates to head).
        adapter
            .put(StorageObject::new("k.txt", b"data".to_vec()))
            .await?;
        assert!(adapter.exists("k.txt").await?);

        // Default presign must surface Unsupported so callers can detect
        // backends that cannot pre-sign.
        match adapter.presign("k.txt", 60).await {
            Err(StorageError::Unsupported) => Ok(()),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------------
    // RUST-P13-001 S15 — single primary invariant
    // ---------------------------------------------------------------------------

    fn row(id: i64, name: &str, primary: bool) -> DataStorageRow {
        DataStorageRow {
            id,
            name: name.to_string(),
            primary,
        }
    }

    #[test]
    fn validate_single_primary_accepts_zero_or_one_primary() {
        assert!(validate_single_primary(&[]).is_ok());
        let one = [row(1, "primary", true), row(2, "replica", false)];
        assert!(validate_single_primary(&one).is_ok());
    }

    #[test]
    fn validate_single_primary_rejects_multiple_primaries() {
        let two = [
            row(1, "primary-a", true),
            row(2, "primary-b", true),
            row(3, "replica", false),
        ];
        match validate_single_primary(&two) {
            Err(StorageError::MultiplePrimary(msg)) => {
                // Names of both offending primaries appear so operators can fix it.
                assert!(msg.contains("primary-a"), "msg was: {msg}");
                assert!(msg.contains("primary-b"), "msg was: {msg}");
            }
            other => panic!("expected MultiplePrimary, got {other:?}"),
        }
    }
}
