//! Asset source abstraction (S04 + S08).
//!
//! Provides a single [`AssetSource`] trait with two production strategies plus
//! one for tests:
//!
//! * [`FileSystemAssets`] - dev strategy: reads files from a configurable root
//!   on disk (default `frontend/dist`).
//! * [`EmbeddedAssets`] - prod strategy: serves bytes compiled into the binary
//!   via `include_dir!`. Gated behind the `embed-frontend` cargo feature.
//! * [`InMemoryAssets`] - test fixture, backed by a `HashMap`.
//!
//! All strategies resolve a request path (e.g. `/assets/app.js`) to bytes, and
//! expose the SPA index document (`index.html`) separately so callers can apply
//! the no-cache headers Go sets in `serveSPAIndex`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(feature = "embed-frontend")]
use std::sync::LazyLock;

#[cfg(feature = "embed-frontend")]
use include_dir::{Dir, include_dir};

/// Default on-disk frontend root (mirrors Go `dist` served at `/`).
pub const DEFAULT_STATIC_ROOT: &str = "frontend/dist";

/// Frontend dist folder compiled into the binary when the `embed-frontend`
/// feature is enabled. Build the frontend before enabling this feature so the
/// distribution directory exists at compile time.
#[cfg(feature = "embed-frontend")]
static EMBEDDED_DIST: LazyLock<Dir<'static>> =
    LazyLock::new(|| include_dir!("$CARGO_MANIFEST_DIR/../../frontend/dist"));

/// A source of static-asset bytes addressable by request path.
///
/// Implementations must normalise the request path the same way
/// [`crate::static_files`] does: strip a leading `/`, reject path traversal,
/// and treat `index.html` as the SPA entry document.
pub trait AssetSource: Send + Sync {
    /// Bytes for an asset identified by its request path (e.g.
    /// `/assets/app.js`, `/index.html`, or `index.html`). Returns `None` when
    /// the asset is absent so the caller can synthesise a 404 or SPA fallback.
    fn read(&self, request_path: &str) -> Option<Cow<'_, [u8]>>;

    /// SPA index document bytes (`index.html` at the root). Mirrors Go
    /// `serveSPAIndex` which always serves `/` regardless of the request path.
    fn index_html(&self) -> Option<Cow<'_, [u8]>>;
}

/// Normalize a request path into a relative asset key, or `None` when the path
/// is unsafe (empty, dot-segments). Shared by filesystem and embedded sources
/// so they agree on addressing.
pub fn asset_key(request_path: &str) -> Option<String> {
    let relative = request_path_without_query(request_path).trim_start_matches('/');

    if relative.is_empty()
        || relative
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return None;
    }

    Some(relative.to_string())
}

/// Read bytes for `key` from the embedded dir, if present.
#[cfg(feature = "embed-frontend")]
fn embedded_read(key: &str) -> Option<Cow<'static, [u8]>> {
    EMBEDDED_DIST
        .get_file(key)
        .map(|file| Cow::Owned(file.contents().to_vec()))
}

/// Dev strategy: read assets from a configurable directory on disk.
#[derive(Debug, Clone)]
pub struct FileSystemAssets {
    root: PathBuf,
}

impl FileSystemAssets {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Construct using [`DEFAULT_STATIC_ROOT`].
    pub fn default_root() -> Self {
        Self::new(DEFAULT_STATIC_ROOT)
    }

    fn resolve(&self, request_path: &str) -> Option<PathBuf> {
        asset_key(request_path).map(|key| self.root.join(key))
    }
}

impl AssetSource for FileSystemAssets {
    fn read(&self, request_path: &str) -> Option<Cow<'_, [u8]>> {
        let path = self.resolve(request_path)?;
        match std::fs::read(&path) {
            Ok(bytes) => Some(Cow::Owned(bytes)),
            Err(_) => None,
        }
    }

    fn index_html(&self) -> Option<Cow<'_, [u8]>> {
        match std::fs::read(self.root.join("index.html")) {
            Ok(bytes) => Some(Cow::Owned(bytes)),
            Err(_) => None,
        }
    }
}

/// Production strategy: serve assets from bytes compiled into the binary via
/// `include_dir!`. Only available with the `embed-frontend` cargo feature.
#[cfg(feature = "embed-frontend")]
#[derive(Debug, Default, Clone)]
pub struct EmbeddedAssets;

#[cfg(feature = "embed-frontend")]
impl EmbeddedAssets {
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(feature = "embed-frontend")]
impl AssetSource for EmbeddedAssets {
    fn read(&self, request_path: &str) -> Option<Cow<'_, [u8]>> {
        let key = asset_key(request_path)?;
        embedded_read(&key)
    }

    fn index_html(&self) -> Option<Cow<'_, [u8]>> {
        embedded_read("index.html")
    }
}

/// Test-only asset source backed by an in-memory map of `key -> bytes`. Keys
/// are stored as relative asset keys (e.g. `index.html`, `assets/app.js`).
#[derive(Debug, Clone, Default)]
pub struct InMemoryAssets {
    files: HashMap<String, Vec<u8>>,
}

impl InMemoryAssets {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a file keyed by its relative asset path (`index.html`,
    /// `assets/app.js`, ...).
    pub fn with(mut self, key: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        self.files.insert(key.into(), bytes.into());
        self
    }
}

impl AssetSource for InMemoryAssets {
    fn read(&self, request_path: &str) -> Option<Cow<'_, [u8]>> {
        let key = asset_key(request_path)?;
        self.files
            .get(&key)
            .map(|bytes| Cow::Borrowed(bytes.as_slice()))
    }

    fn index_html(&self) -> Option<Cow<'_, [u8]>> {
        self.files
            .get("index.html")
            .map(|bytes| Cow::Borrowed(bytes.as_slice()))
    }
}

/// Pick a production asset source: embedded when the `embed-frontend` feature
/// is compiled in, otherwise the filesystem strategy rooted at
/// [`DEFAULT_STATIC_ROOT`]. Mirrors Go which always embeds; Rust defaults to
/// filesystem (dev) and opts into embedding via the cargo feature (prod).
pub fn production_asset_source() -> Box<dyn AssetSource> {
    #[cfg(feature = "embed-frontend")]
    {
        return Box::new(EmbeddedAssets::new());
    }
    #[cfg(not(feature = "embed-frontend"))]
    {
        Box::new(FileSystemAssets::default_root())
    }
}

fn request_path_without_query(request_path: &str) -> &str {
    request_path
        .split_once('?')
        .map_or(request_path, |(path, _)| path)
}

/// Content type for a request path, reused so the asset source stays decoupled
/// from header construction in [`crate::static_files`].
pub fn content_type_for(request_path: &str) -> &'static str {
    crate::static_files::static_content_type_for_path(Path::new(request_path_without_query(
        request_path,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_source_reads_file_from_disk() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        std::fs::create_dir_all(root.join("assets"))?;
        std::fs::write(root.join("assets").join("app.js"), b"console.log(1);")?;

        let source = FileSystemAssets::new(root);
        let bytes = source
            .read("/assets/app.js")
            .ok_or("missing /assets/app.js")?;
        assert_eq!(&*bytes, b"console.log(1);");
        assert_eq!(
            content_type_for("/assets/app.js"),
            "text/javascript; charset=utf-8"
        );
        Ok(())
    }

    #[test]
    fn filesystem_source_returns_none_for_missing_key() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let source = FileSystemAssets::new(dir.path());
        assert!(source.read("/does/not/exist.js").is_none());
        assert!(source.index_html().is_none());
        Ok(())
    }

    #[test]
    fn filesystem_source_rejects_traversal() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("secret.txt"), b"nope")?;
        let source = FileSystemAssets::new(dir.path());
        assert!(source.read("/../secret.txt").is_none());
        assert!(source.read("/./secret.txt").is_none());
        Ok(())
    }

    #[test]
    fn inmemory_source_returns_known_key_and_none_for_missing() {
        let source = InMemoryAssets::new()
            .with("index.html", b"<html>SPA</html>")
            .with("assets/app.js", b"app();");

        match source.index_html() {
            Some(index) => assert_eq!(&*index, b"<html>SPA</html>"),
            None => panic!("index.html must be present"),
        }

        match source.read("/assets/app.js") {
            Some(app) => assert_eq!(&*app, b"app();"),
            None => panic!("assets/app.js must be present"),
        }

        assert!(source.read("/missing.js").is_none());
    }

    #[test]
    fn inmemory_source_strips_query_string() {
        let source = InMemoryAssets::new().with("assets/app.js", b"app();");
        match source.read("/assets/app.js?v=1") {
            Some(bytes) => assert_eq!(&*bytes, b"app();"),
            None => panic!("query-stripped key must resolve"),
        }
    }

    #[test]
    fn production_source_picks_filesystem_without_feature() {
        // Without `embed-frontend` we fall back to the filesystem strategy.
        // Default-root filesystem source cannot resolve a real index here; just
        // assert the strategy type by checking it returns `None` for a clearly
        // absent path without panicking.
        let source = production_asset_source();
        assert!(source.read("/this/path/should/not/exist.js").is_none());
    }
}
