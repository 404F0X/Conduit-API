//! RUST-P11-003 S13 — request content/preview access shaping (pure HTTP-layer).
//!
//! Mirrors the response-header shaping and path-security helpers in
//! `conduit/internal/server/api/request_content.go`:
//!
//! - `Content-Disposition: attachment; filename="<filename>"` (`%q`-quoted,
//!   `request_content.go:138`).
//! - `Content-Type: application/octet-stream` for binary content streams
//!   (`request_content.go:139`).
//! - `Cache-Control: private, max-age=0, no-cache` (`request_content.go:140`).
//! - `filenameFromKey(key, requestID)` (`request_content.go:161-167`): the
//!   `filepath.Base`, falling back to `request-<id>-content` when empty.
//! - `safeRelativePath(key)` (`request_content.go:146-159`): rejects directory
//!   traversal outside the storage root.
//!
//! The domain-layer `RequestContentResponseMetadata` lives in
//! `conduit-services::request_service`; this module is the pure HTTP-layer
//! shaping helper that does not require a live storage handle, so handlers can
//! unit-test the header/path decisions without a filesystem.

use std::collections::BTreeMap;

/// `Content-Type` written for binary content streams.
///
/// Mirrors `request_content.go:139`: `c.Header("Content-Type", "application/octet-stream")`.
pub const BINARY_CONTENT_TYPE: &str = "application/octet-stream";

/// `Cache-Control` written on content downloads to keep private, no-cache.
///
/// Mirrors `request_content.go:140`.
pub const CONTENT_CACHE_CONTROL: &str = "private, max-age=0, no-cache";

/// Shaping for a content-download/preview response: the headers that Go would
/// write onto the `gin.Context` for `DownloadRequestContent`.
///
/// Mirrors the header block in `request_content.go:134-140` plus the optional
/// `Content-Range` extension used by preview endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentMeta {
    /// HTTP header name -> value (lower-cased names, matching the Go writer's
    /// canonicalization once emitted by the HTTP server).
    pub headers: BTreeMap<String, String>,
}

impl ContentMeta {
    /// Returns the value of a header, if present.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// Optional byte range for preview/content streaming.
///
/// Mirrors a single `bytes=start-end` range; Go's `DownloadRequestContent` does
/// not itself emit `Content-Range`, but the preview surface this helper feeds
/// does, so we model the shaping only (no I/O).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
}

impl ContentRange {
    /// Formats the `Content-Range` header value for a known total length.
    pub fn header_value(self, total_len: u64) -> String {
        format!("bytes {}-{}/{}", self.start, self.end, total_len)
    }
}

/// Builds the content-download/preview response metadata as a pure function of
/// the inputs the Go handler derives.
///
/// Parameters mirror the Go header decisions in `request_content.go:134-140`:
///
/// - `content_type`: when `None`, defaults to
///   [`BINARY_CONTENT_TYPE`] (`application/octet-stream`).
/// - `range`: optional byte range for preview streaming; when `Some`, a
///   `Content-Range` header is added.
/// - `disposition`: typically
///   [`ContentDisposition::attachment`] mirroring Go's `attachment; filename=...`
///   (`request_content.go:138`).
/// - `content_length`: optional `Content-Length` value (Go writes it from
///   `f.Stat()` at `request_content.go:134-136`).
pub fn build_content_response_meta(
    content_type: Option<&str>,
    range: Option<ContentRange>,
    disposition: ContentDisposition,
    content_length: Option<u64>,
) -> ContentMeta {
    let mut headers = BTreeMap::new();

    headers.insert(
        "content-type".to_string(),
        content_type.unwrap_or(BINARY_CONTENT_TYPE).to_string(),
    );
    headers.insert(
        "content-disposition".to_string(),
        disposition.header_value(),
    );
    headers.insert(
        "cache-control".to_string(),
        CONTENT_CACHE_CONTROL.to_string(),
    );

    if let Some(length) = content_length {
        headers.insert("content-length".to_string(), length.to_string());
    }

    if let Some(range) = range
        && let Some(total) = content_length
    {
        headers.insert("content-range".to_string(), range.header_value(total));
    }

    ContentMeta { headers }
}

/// Content-Disposition shaping matching Go's header writers.
///
/// Go's `DownloadRequestContent` always writes `attachment` with a quoted
/// filename (`request_content.go:138`); the preview surface may also use
/// `inline`, so both variants are supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentDisposition {
    /// `inline` — preview-only, no filename.
    Inline,
    /// `attachment; filename="<filename>"` — download, mirrors Go `%q` quoting
    /// (`request_content.go:138`).
    Attachment { filename: String },
}

impl ContentDisposition {
    /// Convenience constructor for [`ContentDisposition::Attachment`].
    pub fn attachment(filename: impl Into<String>) -> Self {
        Self::Attachment {
            filename: filename.into(),
        }
    }

    /// Formats the `Content-Disposition` header value.
    ///
    /// Mirrors `fmt.Sprintf("attachment; filename=%q", filename)` from Go
    /// (`request_content.go:138`): the filename is double-quoted with internal
    /// `"` and `\` escaped, exactly like Go's `%q` verb.
    pub fn header_value(&self) -> String {
        match self {
            Self::Inline => "inline".to_string(),
            Self::Attachment { filename } => {
                format!("attachment; filename=\"{}\"", escape_go_quoted(filename))
            }
        }
    }
}

/// Escapes a filename for Go's `%q` verb (used in
/// `fmt.Sprintf("attachment; filename=%q", filename)`).
///
/// Go's `%q` quotes the string with double quotes and escapes `\` and `"` (plus
/// non-printables, which we approximate for the common header-safe subset).
fn escape_go_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Derives the download filename from a storage key, mirroring Go's
/// `filenameFromKey(key, requestID)` (`request_content.go:161-167`).
///
/// Go: `filename := filepath.Base(key); if empty -> "request-<id>-content"`.
pub fn filename_from_key(key: &str, request_id: i64) -> String {
    let base = key
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("");

    if base.is_empty() {
        format!("request-{}-content", request_id)
    } else {
        base.to_string()
    }
}

/// Validates a storage key as a safe relative path, mirroring Go's
/// `safeRelativePath(key)` (`request_content.go:146-159`).
///
/// Returns `Some(relative)` when the cleaned key stays under the storage root,
/// and `None` when it escapes (directory traversal) or is empty.
pub fn safe_relative_path(key: &str) -> Option<String> {
    // Go: strings.TrimPrefix(filepath.FromSlash(key), "/"); filepath.Clean(...)
    let trimmed = key.replace('\\', "/").trim_start_matches('/').to_string();
    let cleaned = clean_path(&trimmed);

    if cleaned.is_empty() || cleaned == "." {
        return None;
    }
    if cleaned == ".." || cleaned.starts_with("../") {
        return None;
    }

    Some(cleaned)
}

/// Port of Go's `filepath.Clean` for the subset of cases relevant to storage
/// keys (collapses `.`, resolves `..` where it does not escape root, dedups
/// slashes). Rejects any `..` that would traverse above root by leaving it
/// in-place, which the caller then matches against the `..`/`../` guard.
fn clean_path(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();

    for segment in path.split('/') {
        match segment {
            "" | "." => continue,
            ".." => {
                // Only pop if the top is a real segment; otherwise leave the
                // `..` so the caller's traversal guard rejects it.
                if let Some(top) = stack.last()
                    && *top != ".."
                {
                    stack.pop();
                    continue;
                }
                stack.push("..");
            }
            other => stack.push(other),
        }
    }

    stack.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- build_content_response_meta ----------------------------------------

    #[test]
    fn build_meta_defaults_to_octet_stream_and_go_disposition_quoting() {
        // Mirrors request_content.go:138-140: default Content-Type is
        // application/octet-stream; filename is %q-quoted.
        let meta = build_content_response_meta(
            None,
            None,
            ContentDisposition::attachment("video.mp4"),
            None,
        );

        assert_eq!(
            meta.header("content-type"),
            Some("application/octet-stream")
        );
        assert_eq!(
            meta.header("content-disposition"),
            Some("attachment; filename=\"video.mp4\"")
        );
        assert_eq!(meta.header("cache-control"), Some(CONTENT_CACHE_CONTROL));
        assert_eq!(meta.header("content-length"), None);
        assert_eq!(meta.header("content-range"), None);
    }

    #[test]
    fn build_meta_honors_explicit_content_type_and_length() {
        let meta = build_content_response_meta(
            Some("video/mp4"),
            None,
            ContentDisposition::Inline,
            Some(1024),
        );

        assert_eq!(meta.header("content-type"), Some("video/mp4"));
        assert_eq!(meta.header("content-disposition"), Some("inline"));
        assert_eq!(meta.header("content-length"), Some("1024"));
    }

    #[test]
    fn build_meta_adds_content_range_when_range_and_length_present() {
        let range = ContentRange { start: 0, end: 99 };
        let meta = build_content_response_meta(
            Some("video/mp4"),
            Some(range),
            ContentDisposition::attachment("clip.mp4"),
            Some(100),
        );

        assert_eq!(meta.header("content-range"), Some("bytes 0-99/100"));
    }

    #[test]
    fn build_meta_omits_content_range_without_total_length() {
        // Go's DownloadRequestContent never emits Content-Range; the helper
        // mirrors that when total length is unknown.
        let range = ContentRange { start: 0, end: 99 };
        let meta = build_content_response_meta(
            None,
            Some(range),
            ContentDisposition::attachment("clip.mp4"),
            None,
        );

        assert_eq!(meta.header("content-range"), None);
    }

    // --- ContentDisposition %q quoting --------------------------------------

    #[test]
    fn disposition_quotes_filename_with_double_quotes() {
        // Mirrors Go fmt.Sprintf("attachment; filename=%q", "video.mp4")
        // -> attachment; filename="video.mp4"
        let d = ContentDisposition::attachment("video.mp4");
        assert_eq!(d.header_value(), "attachment; filename=\"video.mp4\"");
    }

    #[test]
    fn disposition_escapes_embedded_quotes_and_backslashes() {
        // Go %q escapes both `"` and `\`.
        let d = ContentDisposition::attachment("weird\"name\\.bin");
        assert_eq!(
            d.header_value(),
            "attachment; filename=\"weird\\\"name\\\\.bin\""
        );
    }

    #[test]
    fn disposition_inline_has_no_filename() {
        assert_eq!(ContentDisposition::Inline.header_value(), "inline");
    }

    // --- filename_from_key --------------------------------------------------

    #[test]
    fn filename_from_key_uses_basename() {
        // Mirrors Go filepath.Base of "/1/requests/2/video/video.mp4".
        assert_eq!(
            filename_from_key("/1/requests/2/video/video.mp4", 2),
            "video.mp4"
        );
    }

    #[test]
    fn filename_from_key_falls_back_to_request_id_when_empty() {
        // Mirrors request_content.go:161-167: filepath.Base returns "." for ""
        // and "/" -> fallback "request-<id>-content". Trailing-slash keys like
        // "/trail/" have basename "trail" (verified against Go filepath.Base).
        assert_eq!(filename_from_key("/", 7), "request-7-content");
        assert_eq!(filename_from_key("", 7), "request-7-content");
        assert_eq!(filename_from_key("/trail/", 7), "trail");
    }

    // --- safe_relative_path -------------------------------------------------

    #[test]
    fn safe_relative_path_accepts_nested_key() {
        assert_eq!(
            safe_relative_path("/1/requests/2/video/video.mp4"),
            Some("1/requests/2/video/video.mp4".to_string())
        );
    }

    #[test]
    fn safe_relative_path_rejects_directory_traversal() {
        // Mirrors request_content_test.go:177-206 "escapes directory traversal".
        assert_eq!(safe_relative_path("/../../etc/passwd"), None);
        assert_eq!(safe_relative_path("../../etc/passwd"), None);
        assert_eq!(safe_relative_path("/foo/../../bar"), None);
    }

    #[test]
    fn safe_relative_path_rejects_empty() {
        assert_eq!(safe_relative_path(""), None);
        assert_eq!(safe_relative_path("/"), None);
    }

    #[test]
    fn safe_relative_path_collapses_dot_segments() {
        assert_eq!(safe_relative_path("/a/./b/c"), Some("a/b/c".to_string()));
    }
}
