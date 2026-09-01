use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{Response, StatusCode};
use axum::response::IntoResponse;
use serde_json::json;

pub const DEFAULT_STATIC_ROOT: &str = "frontend/dist";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticFallbackDecision {
    ApiNotFound,
    StaticFile { file_path: PathBuf },
    StaticAssetNotFound { file_path: PathBuf },
    FrontendIndex { index_path: PathBuf },
}

pub fn default_static_root() -> PathBuf {
    PathBuf::from(DEFAULT_STATIC_ROOT)
}

pub fn decide_static_fallback(
    request_path: &str,
    static_root: impl AsRef<Path>,
) -> StaticFallbackDecision {
    if is_api_path(request_path) || is_handler_owned_path(request_path) {
        StaticFallbackDecision::ApiNotFound
    } else if let Some(file_path) = static_file_path(request_path, static_root.as_ref()) {
        if file_path.is_file() {
            StaticFallbackDecision::StaticFile { file_path }
        } else if request_has_extension(request_path) {
            StaticFallbackDecision::StaticAssetNotFound { file_path }
        } else {
            StaticFallbackDecision::FrontendIndex {
                index_path: static_root.as_ref().join("index.html"),
            }
        }
    } else {
        StaticFallbackDecision::FrontendIndex {
            index_path: static_root.as_ref().join("index.html"),
        }
    }
}

pub fn static_fallback_response(
    request_path: &str,
    static_root: impl AsRef<Path>,
) -> Response<Body> {
    match decide_static_fallback(request_path, static_root) {
        StaticFallbackDecision::ApiNotFound => api_not_found_response(),
        StaticFallbackDecision::StaticFile { file_path } => static_file_response(&file_path),
        StaticFallbackDecision::StaticAssetNotFound { file_path } => {
            static_asset_not_found_response(&file_path)
        }
        StaticFallbackDecision::FrontendIndex { index_path } => {
            frontend_index_response(&index_path)
        }
    }
}

/// Header value set on the SPA index document by Go `serveSPAIndex`.
pub const SPA_INDEX_CACHE_CONTROL: &str = "no-cache, no-store, must-revalidate";
/// Header value set on the SPA index document by Go `serveSPAIndex`.
pub const SPA_INDEX_PRAGMA: &str = "no-cache";
/// Header value set on the SPA index document by Go `serveSPAIndex`.
pub const SPA_INDEX_EXPIRES: &str = "0";

/// Serve a request using a generic [`AssetSource`] (S08 dual-strategy). The
/// decision logic is identical to [`static_fallback_response`]; only the byte
/// source differs. API paths always return the JSON 404, never the SPA index;
/// SPA routes return `index.html` with the no-cache headers Go sets in
/// `serveSPAIndex`; known assets are served from the source with long/short
/// cache headers per [`static_cache_control_for_path`]; unknown assets with an
/// extension return the JSON 404.
pub fn serve_from_asset_source(
    request_path: &str,
    source: &dyn crate::asset_source::AssetSource,
) -> Response<Body> {
    serve_from_asset_source_with_base_path(request_path, "", source)
}

/// Serve static content mounted beneath `base_path`. SPA index responses are
/// annotated with a document base and a runtime base-path meta value so both
/// Vite chunks, client-side navigation, and API calls resolve consistently.
pub fn serve_from_asset_source_with_base_path(
    request_path: &str,
    base_path: &str,
    source: &dyn crate::asset_source::AssetSource,
) -> Response<Body> {
    use axum::http::header::{EXPIRES, PRAGMA};

    let cleaned = request_path
        .split_once('?')
        .map_or(request_path, |(p, _)| p);

    if is_api_path(cleaned) || is_handler_owned_path(cleaned) {
        return api_not_found_response();
    }

    // SPA index document for any non-asset, non-API path.
    if !request_has_extension(cleaned) {
        return match source.index_html() {
            Some(bytes) => (
                StatusCode::OK,
                [
                    (CONTENT_TYPE, "text/html; charset=utf-8"),
                    (CACHE_CONTROL, SPA_INDEX_CACHE_CONTROL),
                    (PRAGMA, SPA_INDEX_PRAGMA),
                    (EXPIRES, SPA_INDEX_EXPIRES),
                ],
                inject_frontend_base_path(bytes.as_ref(), base_path),
            )
                .into_response(),
            None => static_index_missing_response(),
        };
    }

    // Asset path (has an extension): try the source, else JSON 404.
    match source.read(cleaned) {
        Some(bytes) => {
            let content_path = Path::new(cleaned);
            (
                StatusCode::OK,
                [
                    (CONTENT_TYPE, static_content_type_for_path(content_path)),
                    (CACHE_CONTROL, static_cache_control_for_path(content_path)),
                ],
                bytes.into_owned(),
            )
                .into_response()
        }
        None => static_asset_not_found_response(Path::new(cleaned)),
    }
}

fn inject_frontend_base_path(index: &[u8], base_path: &str) -> Vec<u8> {
    let base_path = base_path.trim_end_matches('/');
    let href = if base_path.is_empty() {
        "/".to_string()
    } else {
        format!("{base_path}/")
    };
    let runtime =
        format!("<base href=\"{href}\"><meta name=\"conduit-base-path\" content=\"{base_path}\">");
    let html = String::from_utf8_lossy(index);
    // The HTML parser resolves relative resource URLs as it encounters them.
    // Injecting <base> at the end of <head> is therefore too late for Vite's
    // preceding ./assets/* tags on a nested SPA route such as /project/wallet.
    // Keep it as the first head child so every relative URL uses the mount root.
    let head_content_start = html
        .find("<head")
        .and_then(|head_start| html[head_start..].find('>').map(|end| head_start + end + 1));
    if let Some(head_content_start) = head_content_start {
        let mut output = String::with_capacity(html.len() + runtime.len());
        output.push_str(&html[..head_content_start]);
        output.push_str(&runtime);
        output.push_str(&html[head_content_start..]);
        output.into_bytes()
    } else {
        format!("{runtime}{html}").into_bytes()
    }
}

fn static_index_missing_response() -> Response<Body> {
    (
        StatusCode::NOT_FOUND,
        [
            (CONTENT_TYPE, "application/json"),
            (CACHE_CONTROL, "no-cache"),
        ],
        json!({
            "error": {
                "code": "static_index_missing",
                "message": "frontend index.html is not available"
            }
        })
        .to_string(),
    )
        .into_response()
}

pub fn is_api_path(path: &str) -> bool {
    let path = request_path_without_query(path);

    [
        "/api",
        "/admin",
        "/openapi",
        "/v1",
        "/v1beta",
        "/oauth",
        "/anthropic",
        "/gemini",
        "/jina",
        "/doubao",
    ]
    .iter()
    .any(|prefix| matches_api_prefix(path, prefix))
}

fn is_handler_owned_path(path: &str) -> bool {
    let path = request_path_without_query(path);

    // Exact handler-owned paths must not be masked by the SPA fallback while
    // their real handlers are still being wired into the router.
    matches_api_prefix(path, "/favicon") || matches_api_prefix(path, "/favicon.ico")
}

fn matches_api_prefix(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}

pub(crate) fn api_not_found_response() -> Response<Body> {
    let body = json!({
        "error": {
            "code": "not_found",
            "message": "Not found"
        }
    })
    .to_string();

    (
        StatusCode::NOT_FOUND,
        [
            (CONTENT_TYPE, "application/json"),
            (CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

fn static_file_response(file_path: &Path) -> Response<Body> {
    match std::fs::read(file_path) {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (CONTENT_TYPE, static_content_type_for_path(file_path)),
                (CACHE_CONTROL, static_cache_control_for_path(file_path)),
            ],
            bytes,
        )
            .into_response(),
        Err(err) => static_file_error_response("static_file_read_failed", err),
    }
}

fn static_asset_not_found_response(file_path: &Path) -> Response<Body> {
    (
        StatusCode::NOT_FOUND,
        [
            (CONTENT_TYPE, "application/json"),
            (CACHE_CONTROL, "no-cache"),
        ],
        json!({
            "error": {
                "code": "static_asset_not_found",
                "message": format!("static asset not found: {}", file_path.display())
            }
        })
        .to_string(),
    )
        .into_response()
}

fn frontend_index_response(index_path: &Path) -> Response<Body> {
    match std::fs::read_to_string(index_path) {
        Ok(index_html) => (
            StatusCode::OK,
            [
                (CONTENT_TYPE, "text/html; charset=utf-8"),
                (CACHE_CONTROL, "no-cache"),
            ],
            index_html,
        )
            .into_response(),
        Err(err) => static_file_error_response("static_index_missing", err),
    }
}

fn static_file_error_response(code: &'static str, err: std::io::Error) -> Response<Body> {
    (
        StatusCode::NOT_FOUND,
        [
            (CONTENT_TYPE, "application/json"),
            (CACHE_CONTROL, "no-cache"),
        ],
        json!({
            "error": {
                "code": code,
                "message": err.to_string()
            }
        })
        .to_string(),
    )
        .into_response()
}

fn static_file_path(request_path: &str, static_root: &Path) -> Option<PathBuf> {
    let relative_path = request_path_without_query(request_path).trim_start_matches('/');

    if relative_path.is_empty()
        || relative_path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return None;
    }

    Some(static_root.join(relative_path))
}

fn request_has_extension(request_path: &str) -> bool {
    Path::new(request_path_without_query(request_path))
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| !extension.is_empty())
}

fn request_path_without_query(request_path: &str) -> &str {
    request_path
        .split_once('?')
        .map_or(request_path, |(path, _)| path)
}

pub fn static_content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "txt" => "text/plain; charset=utf-8",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

pub fn static_cache_control_for_path(path: &Path) -> &'static str {
    if is_index_html_path(path) {
        "no-cache"
    } else if is_hash_asset_path(path) {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    }
}

fn is_index_html_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|file_name| file_name.to_str())
        .is_some_and(|file_name| file_name.eq_ignore_ascii_case("index.html"))
}

fn is_hash_asset_path(path: &Path) -> bool {
    path.file_stem()
        .and_then(|file_stem| file_stem.to_str())
        .is_some_and(|file_stem| file_stem.split(['.', '-', '_']).any(looks_like_asset_hash))
}

fn looks_like_asset_hash(part: &str) -> bool {
    part.len() >= 8
        && (part.bytes().all(|byte| byte.is_ascii_hexdigit())
            || (part.bytes().all(|byte| byte.is_ascii_alphanumeric())
                && part.bytes().any(|byte| byte.is_ascii_alphabetic())
                && part.bytes().any(|byte| byte.is_ascii_digit())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_paths_do_not_fallback_to_frontend_index() {
        for path in [
            "/api/unknown",
            "/admin/unknown",
            "/admin/system/status",
            "/openapi/unknown",
            "/v1/unknown",
            "/v1beta/models/foo",
            "/oauth/unknown",
            "/anthropic/v1/messages",
            "/gemini/v1/models/foo",
            "/jina/unknown",
            "/doubao/unknown",
        ] {
            assert_eq!(
                decide_static_fallback(path, "dist"),
                StaticFallbackDecision::ApiNotFound,
                "{path}"
            );
        }
    }

    #[test]
    fn handler_owned_paths_do_not_fallback_to_frontend_index() {
        for path in ["/favicon", "/favicon.ico", "/favicon?theme=dark"] {
            assert_eq!(
                decide_static_fallback(path, "dist"),
                StaticFallbackDecision::ApiNotFound,
                "{path}"
            );
        }
    }

    #[test]
    fn frontend_paths_fallback_to_index_html() {
        assert_eq!(
            decide_static_fallback("/projects/123", "dist"),
            StaticFallbackDecision::FrontendIndex {
                index_path: PathBuf::from("dist").join("index.html")
            }
        );
    }

    #[test]
    fn missing_asset_with_extension_does_not_fallback_to_index_html() {
        assert_eq!(
            decide_static_fallback("/assets/app.js", "dist"),
            StaticFallbackDecision::StaticAssetNotFound {
                file_path: PathBuf::from("dist").join("assets/app.js")
            }
        );
    }

    #[test]
    fn static_content_type_matches_common_static_extensions() {
        for (path, expected_content_type) in [
            ("index.html", "text/html; charset=utf-8"),
            ("assets/app.js", "text/javascript; charset=utf-8"),
            ("assets/app.css", "text/css; charset=utf-8"),
            ("assets/manifest.json", "application/json"),
            ("assets/logo.png", "image/png"),
            ("assets/icon.svg", "image/svg+xml"),
            ("assets/file.unknown", "application/octet-stream"),
        ] {
            assert_eq!(
                static_content_type_for_path(Path::new(path)),
                expected_content_type,
                "{path}"
            );
        }
    }

    #[test]
    fn static_cache_control_keeps_index_html_fresh() {
        for path in ["index.html", "assets/index.html", "INDEX.HTML"] {
            assert_eq!(
                static_cache_control_for_path(Path::new(path)),
                "no-cache",
                "{path}"
            );
        }
    }

    #[test]
    fn static_cache_control_long_caches_hash_assets() {
        for path in [
            "assets/app.abcdef12.js",
            "assets/app-abcdef12.css",
            "assets/logo_abc123def.png",
        ] {
            assert_eq!(
                static_cache_control_for_path(Path::new(path)),
                "public, max-age=31536000, immutable",
                "{path}"
            );
        }
    }

    #[test]
    fn static_cache_control_short_caches_plain_assets() {
        for path in [
            "assets/app.js",
            "assets/app.css",
            "assets/config.json",
            "assets/logo.png",
            "assets/icon.svg",
            "assets/file.unknown",
        ] {
            assert_eq!(
                static_cache_control_for_path(Path::new(path)),
                "public, max-age=3600",
                "{path}"
            );
        }
    }

    #[test]
    fn serve_pipeline_returns_index_html_with_no_cache_headers_for_spa_route() {
        use crate::asset_source::InMemoryAssets;

        let source = InMemoryAssets::new().with("index.html", b"<html>SPA</html>");
        let response = serve_from_asset_source("/projects/123", &source);
        assert_eq!(response.status(), StatusCode::OK);

        let headers = response.headers();
        assert_eq!(
            headers.get(CONTENT_TYPE).map(|v| v.to_str().unwrap_or("")),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            headers.get(CACHE_CONTROL).map(|v| v.to_str().unwrap_or("")),
            Some(SPA_INDEX_CACHE_CONTROL)
        );
        assert_eq!(
            headers
                .get(axum::http::header::PRAGMA)
                .map(|v| v.to_str().unwrap_or("")),
            Some(SPA_INDEX_PRAGMA)
        );
        assert_eq!(
            headers
                .get(axum::http::header::EXPIRES)
                .map(|v| v.to_str().unwrap_or("")),
            Some(SPA_INDEX_EXPIRES)
        );
    }

    #[test]
    fn spa_index_injection_carries_runtime_base_path() {
        let output = inject_frontend_base_path(
            b"<html><head><title>Conduit</title></head><body></body></html>",
            "/gateway",
        );
        let html = String::from_utf8(output).expect("injected index remains UTF-8");

        assert!(html.contains("<base href=\"/gateway/\">"));
        assert!(html.contains("name=\"conduit-base-path\" content=\"/gateway\""));
    }

    #[test]
    fn spa_index_injection_precedes_relative_assets() {
        let output = inject_frontend_base_path(
            br#"<html><head data-app="conduit"><script src="./assets/app.js"></script><link href="./assets/app.css"></head><body></body></html>"#,
            "",
        );
        let html = String::from_utf8(output).expect("injected index remains UTF-8");

        let base = html.find("<base href=\"/\">").expect("base is injected");
        let script = html
            .find("./assets/app.js")
            .expect("script remains present");
        let stylesheet = html
            .find("./assets/app.css")
            .expect("stylesheet remains present");
        assert!(base < script, "base must precede the first relative script");
        assert!(base < stylesheet, "base must precede relative stylesheets");
    }

    #[test]
    fn serve_pipeline_returns_asset_bytes_for_known_path() {
        use crate::asset_source::InMemoryAssets;

        let source = InMemoryAssets::new()
            .with("index.html", b"<html>SPA</html>")
            .with("assets/app.abcdef12.js", b"app();");
        let response = serve_from_asset_source("/assets/app.abcdef12.js", &source);
        assert_eq!(response.status(), StatusCode::OK);

        let headers = response.headers();
        assert_eq!(
            headers.get(CONTENT_TYPE).map(|v| v.to_str().unwrap_or("")),
            Some("text/javascript; charset=utf-8")
        );
        // Hash-named assets get the long cache.
        assert_eq!(
            headers.get(CACHE_CONTROL).map(|v| v.to_str().unwrap_or("")),
            Some("public, max-age=31536000, immutable")
        );
    }

    #[test]
    fn serve_pipeline_returns_json_404_for_api_path_not_index() {
        use crate::asset_source::InMemoryAssets;

        let source = InMemoryAssets::new().with("index.html", b"<html>SPA</html>");
        let response = serve_from_asset_source("/v1/messages", &source);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .map(|v| v.to_str().unwrap_or("")),
            Some("application/json")
        );
    }

    #[test]
    fn serve_pipeline_returns_json_404_for_missing_asset_with_extension() {
        use crate::asset_source::InMemoryAssets;

        let source = InMemoryAssets::new().with("index.html", b"<html>SPA</html>");
        let response = serve_from_asset_source("/assets/missing.js", &source);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .map(|v| v.to_str().unwrap_or("")),
            Some("application/json")
        );
    }
}
