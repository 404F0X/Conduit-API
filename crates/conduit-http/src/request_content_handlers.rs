//! Request content download endpoint (RUST-P11-001 MAP-02 + RUST-P10-001 S12).
//!
//! Ports `conduit/internal/server/api/request_content.go`
//! (`RequestContentHandlers.DownloadRequestContent`) plus the `X-Project-ID`
//! semantics of `middleware/project.go` that Go layers in front of it:
//!
//! | method | path                                   | Go handler |
//! |--------|----------------------------------------|------------|
//! | GET    | `/admin/requests/{request_id}/content` | `DownloadRequestContent` (routes.go:130-134) |
//!
//! Go mounts the route on the `/admin` group wrapped in
//! `middleware.WithJWTAuth(services.AuthService)` + `middleware.WithProjectID()`
//! (routes.go:96) and `middleware.WithTimeout(server.Config.RequestTimeout)`
//! (routes.go:132). The JWT middleware is not yet ported (the same documented
//! gap as the OIDC manual-link route, see `oidc_handlers.rs`); the
//! `WithProjectID` header contract is reproduced inline by
//! [`resolve_project_id`].
//!
//! Service surface: Go injects `*biz.DataStorageService` via fx
//! (request_content.go:20-34) and reads the request row straight off the ent
//! client in the request context (`ent.FromContext(ctx).Request.Get`,
//! request_content.go:55). The Rust handler folds both onto the minimal
//! [`RequestContentService`] trait; the host bridges it to
//! `conduit-services::request_service` (whose pure
//! `resolve_request_content_location` mirrors the same Go lines 70-99) and the
//! storage layer at boot.

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::api_error::json_error;
use crate::app_state::AppState;
use crate::middleware::{
    AuthRequestContextExtension, ProjectIdStatus, caller_can_read_requests, project_id_outcome,
};
use crate::request_content_helpers::{
    BINARY_CONTENT_TYPE, CONTENT_CACHE_CONTROL, ContentDisposition, filename_from_key,
    safe_relative_path,
};

/// Why [`resolve_project_id`] rejected the request. Both map to HTTP 400 in
/// the `JSONError` shape, but with distinct messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectIdRejection {
    /// Middleware branch (project.go:22-26): header present but not a valid
    /// `Project` GUID → `AbortWithError(400, "Invalid project ID")`
    /// (middleware/error.go:12-20 — same JSON shape as `api.JSONError`).
    InvalidProjectId,
    /// Handler prologue (request_content.go:43-47 / request_live.go:100-104):
    /// `contexts.GetProjectID` missing (header absent/empty, project.go:17-20)
    /// or `projectID <= 0` → `400 "Project ID not found in context"`.
    NotFoundInContext,
}

/// Resolve the project id exactly as the Go stack does for these admin routes,
/// collapsing `middleware.WithProjectID()` (project.go:14-33 — already ported
/// as the pure [`project_id_outcome`] in `middleware.rs`) and the handler
/// prologue `contexts.GetProjectID` / `projectID <= 0` check into one
/// decision.
pub fn resolve_project_id(headers: &HeaderMap) -> Result<i64, ProjectIdRejection> {
    let outcome = project_id_outcome(headers);
    match outcome.status {
        // project.go:17-20 — no header: middleware passes through and the
        // handler then fails contexts.GetProjectID.
        ProjectIdStatus::Missing => Err(ProjectIdRejection::NotFoundInContext),
        // project.go:22-26 — bad GUID / non-Project type.
        ProjectIdStatus::Invalid => Err(ProjectIdRejection::InvalidProjectId),
        ProjectIdStatus::Ok => match outcome.project_id {
            // Handler prologue: `!ok || projectID <= 0`.
            Some(id) if id > 0 => Ok(id),
            _ => Err(ProjectIdRejection::NotFoundInContext),
        },
    }
}

/// Render a [`ProjectIdRejection`] as the Go error response.
pub(crate) fn project_id_rejection_response(rejection: ProjectIdRejection) -> Response {
    match rejection {
        ProjectIdRejection::InvalidProjectId => {
            json_error(StatusCode::BAD_REQUEST, "Invalid project ID")
        }
        ProjectIdRejection::NotFoundInContext => {
            json_error(StatusCode::BAD_REQUEST, "Project ID not found in context")
        }
    }
}

/// Go `DownloadContentRequest` (request_content.go:36-38):
/// `RequestID int \`uri:"request_id"\``. The preview handler binds the same
/// struct (request_live.go:106-110).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadContentRequest {
    pub request_id: i64,
}

/// Bind the `{request_id}` URI param the way gin's `ShouldBindUri` binds
/// `DownloadContentRequest.RequestID int`: `strconv.ParseInt(value, 10, 64)`
/// (gin binding `setIntField`), whose `*strconv.NumError` renders as
/// `strconv.ParseInt: parsing "<raw>": invalid syntax|value out of range`.
/// The handlers wrap the message as `Invalid request body: <err>`
/// (request_content.go:49-53).
pub fn parse_request_id_param(raw: &str) -> Result<i64, String> {
    match raw.parse::<i64>() {
        Ok(id) => Ok(id),
        Err(err) => {
            let reason = match err.kind() {
                std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
                    "value out of range"
                }
                _ => "invalid syntax",
            };
            Err(format!("strconv.ParseInt: parsing {raw:?}: {reason}"))
        }
    }
}

// ---- service trait surface ------------------------------------------------

/// Projection of the `ent.Request` row consumed by `DownloadRequestContent`
/// (request_content.go:55-84): identity/ownership plus the `content_*`
/// columns. Field-for-field this matches
/// `conduit-services::request_service::RequestContentLocation`
/// (content_saved / content_storage_id / content_storage_key) plus the
/// id/project_id pair the handler compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRequestRow {
    /// Go `req.ID`.
    pub id: i64,
    /// Go `req.ProjectID`.
    pub project_id: i64,
    /// Go `req.ContentSaved`.
    pub content_saved: bool,
    /// Go `req.ContentStorageID` (`*int`).
    pub content_storage_id: Option<i64>,
    /// Go `req.ContentStorageKey` (`*string`).
    pub content_storage_key: Option<String>,
}

/// Go `datastorage.Type` enum (ent/datastorage/datastorage.go:111-115 —
/// `database`, `fs`, `s3`, plus `gcs`/`webdav` accepted by the validator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentStorageType {
    Database,
    Fs,
    S3,
    Gcs,
    Webdav,
}

/// Projection of the `*ent.DataStorage` row consumed by the handler
/// (request_content.go:96-123): `Primary`, `Type`, `Settings.Directory` (fs),
/// `Settings.S3.PathStyle`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentDataStorage {
    /// The `DataStorage` row id (`ds.ID`). The handler does not consult this,
    /// but the service's `open_content` uses it to reload the full row's
    /// credentials and build the backend client (Go's `GetFileSystem` resolves
    /// the fs from the same row); carrying the id keeps this projection cheap
    /// while still letting the external-storage path reach the credentials.
    pub storage_id: i64,
    /// Go `ds.Primary`.
    pub primary: bool,
    /// Go `ds.Type`.
    pub storage_type: ContentStorageType,
    /// Go `ds.Settings.Directory` — `Some` only for fs storages configured
    /// with a local directory (enables the serve-from-disk fast path,
    /// request_content.go:101-111).
    pub directory: Option<String>,
    /// Go `ds.Settings.S3.PathStyle` (request_content.go:121-123).
    pub s3_path_style: bool,
}

/// Content bytes handed back by [`RequestContentService::open_content`].
///
/// `size` mirrors Go's optional `f.Stat()` (request_content.go:134-136):
/// `Some` sets an explicit `Content-Length`, `None` omits it exactly as Go
/// does when `Stat` fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFile {
    pub data: Vec<u8>,
    pub size: Option<u64>,
}

/// Error surface of [`RequestContentService::open_content`], mirroring the two
/// Go failure branches of the external-storage path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentOpenError {
    /// `DataStorageService.GetFileSystem` failed → 500 "Failed to open
    /// content storage" (request_content.go:113-117).
    StorageUnavailable,
    /// `fs.Open(key)` failed → 404 "Content not found"
    /// (request_content.go:125-129).
    NotFound,
}

/// Minimal service trait behind the download endpoint. Stands in for the pair
/// Go wires via fx: the request-scoped ent client (`Request.Get`) and
/// `*biz.DataStorageService` (request_content.go:20-34).
#[async_trait::async_trait]
pub trait RequestContentService: Send + Sync {
    /// `ent.FromContext(ctx).Request.Get(ctx, id)` (request_content.go:55-63):
    /// `Ok(None)` is `ent.IsNotFound` → 404 "Request not found"; `Err` → 500
    /// "Failed to load request". The error payload is log-only.
    async fn get_request(&self, request_id: i64) -> Result<Option<ContentRequestRow>, String>;

    /// `DataStorageService.GetDataStorageByID` (request_content.go:86-94,
    /// biz/data_storage.go:281): `Ok(None)` is `ent.IsNotFound` → 404
    /// "Content storage not found"; `Err` → 500 "Failed to load content
    /// storage".
    async fn get_data_storage(&self, storage_id: i64)
    -> Result<Option<ContentDataStorage>, String>;

    /// `DataStorageService.GetFileSystem` + `fs.Open(key)` + read
    /// (request_content.go:113-143, biz/data_storage.go:483). `key` already
    /// carries the Go per-type adjustment (fs `filepath.FromSlash`, path-style
    /// S3 leading-`/` strip, request_content.go:119-123).
    async fn open_content(
        &self,
        storage: &ContentDataStorage,
        key: &str,
    ) -> Result<ContentFile, ContentOpenError>;
}

// ---- handler ----------------------------------------------------------------

/// `GET /admin/requests/{request_id}/content` — Go
/// `RequestContentHandlers.DownloadRequestContent` (request_content.go:40-144).
///
/// Response table (verbatim Go; errors in the `JSONError` shape unless noted):
///
/// | condition                                   | status | message |
/// |---------------------------------------------|--------|---------|
/// | project id missing from context             | 400    | `Project ID not found in context` (43-47) |
/// | invalid `X-Project-ID` GUID                 | 400    | `Invalid project ID` (middleware, project.go:24) |
/// | non-integer `request_id`                    | 400    | `Invalid request body: <strconv err>` (49-53) |
/// | request row not found / project mismatch    | 404    | `Request not found` (56-68) |
/// | request row load failure                    | 500    | `Failed to load request` (61) |
/// | content gate / key prefix / traversal fail  | 404    | `Content not found` (70-84, 102-106) |
/// | storage row not found                       | 404    | `Content storage not found` (88-90) |
/// | storage row load failure                    | 500    | `Failed to load content storage` (92) |
/// | storage primary or database-typed           | 400    | `Content storage is not file-based` (96-99) |
/// | fs fast path, file missing                  | 404    | plain-text `404 page not found` (http.ServeFile) |
/// | `GetFileSystem` failure                     | 500    | `Failed to open content storage` (113-117) |
/// | `fs.Open` failure                           | 404    | `Content not found` (125-129) |
/// | success (external storage)                  | 200    | attachment octet-stream (132-143) |
pub async fn download_request_content(
    State(state): State<AppState>,
    auth: Option<axum::Extension<AuthRequestContextExtension>>,
    Path(request_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    // P-22: the client-supplied `X-Project-ID` alone must NOT grant access —
    // require the JWT caller to be an owner or hold `read_requests` (Go's
    // Request ent policy owner + UserReadScopeRule branches). Unauthorized
    // callers get 404 (never leak that the request exists). Checked first, so a
    // foreign `X-Project-ID` can never reach the row lookup.
    if !caller_can_read_requests(auth.as_ref().map(|ext| &ext.0)) {
        return json_error(StatusCode::NOT_FOUND, "Request not found");
    }

    // request_content.go:43-47 (+ middleware/project.go).
    let project_id = match resolve_project_id(&headers) {
        Ok(id) => id,
        Err(rejection) => return project_id_rejection_response(rejection),
    };

    // request_content.go:49-53 — ShouldBindUri.
    let request = match parse_request_id_param(&request_id) {
        Ok(request_id) => DownloadContentRequest { request_id },
        Err(err) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                format!("Invalid request body: {err}"),
            );
        }
    };

    let Some(service) = state.services().request_content_service() else {
        // Unwired service (Rust-only state; fx guarantees injection in Go)
        // degrades to the row-load 500 branch.
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load request");
    };

    // request_content.go:55-63 — ent Request.Get.
    let req = match service.get_request(request.request_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return json_error(StatusCode::NOT_FOUND, "Request not found"),
        Err(_) => {
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load request");
        }
    };

    // request_content.go:65-68 — cross-project rows are hidden as 404.
    if project_id != req.project_id {
        return json_error(StatusCode::NOT_FOUND, "Request not found");
    }

    // request_content.go:70-73 — content_saved gate.
    let raw_key = req.content_storage_key.as_deref().unwrap_or("").trim();
    let Some(content_storage_id) = req.content_storage_id else {
        return json_error(StatusCode::NOT_FOUND, "Content not found");
    };
    if !req.content_saved || raw_key.is_empty() {
        return json_error(StatusCode::NOT_FOUND, "Content not found");
    }

    // request_content.go:75-79 — normalise to a single leading '/'.
    let key = if raw_key.starts_with('/') {
        raw_key.to_string()
    } else {
        format!("/{raw_key}")
    };

    // request_content.go:80-84 — key must be scoped to this project/request.
    let expected_prefix = format!("/{}/requests/{}/", req.project_id, req.id);
    if !key.starts_with(&expected_prefix) {
        return json_error(StatusCode::NOT_FOUND, "Content not found");
    }

    // request_content.go:86-94 — resolve the DataStorage row.
    let ds = match service.get_data_storage(content_storage_id).await {
        Ok(Some(ds)) => ds,
        Ok(None) => return json_error(StatusCode::NOT_FOUND, "Content storage not found"),
        Err(_) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load content storage",
            );
        }
    };

    // request_content.go:96-99 — only non-primary file-based storages serve
    // downloads.
    if ds.primary || ds.storage_type == ContentStorageType::Database {
        return json_error(StatusCode::BAD_REQUEST, "Content storage is not file-based");
    }

    // request_content.go:101-111 — fs-with-directory fast path: serve straight
    // from local disk via the gin FileAttachment / http.ServeFile pair.
    if ds.storage_type == ContentStorageType::Fs
        && let Some(directory) = ds.directory.as_deref()
    {
        let Some(rel) = safe_relative_path(&key) else {
            return json_error(StatusCode::NOT_FOUND, "Content not found");
        };
        let full_path = std::path::Path::new(directory).join(rel);
        return serve_file_attachment(full_path, &filename_from_key(&key, req.id)).await;
    }

    // request_content.go:119-123 — per-type key adjustment before fs.Open.
    let key = adjust_key_for_storage(&ds, &key);

    // request_content.go:113-129 — open via the abstract filesystem.
    let file = match service.open_content(&ds, &key).await {
        Ok(file) => file,
        Err(ContentOpenError::StorageUnavailable) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to open content storage",
            );
        }
        Err(ContentOpenError::NotFound) => {
            return json_error(StatusCode::NOT_FOUND, "Content not found");
        }
    };

    // request_content.go:132-143 — attachment headers + body copy.
    let filename = filename_from_key(&key, req.id);
    octet_stream_attachment_response(&filename, file)
}

// ---- response shaping helpers -----------------------------------------------

/// Go per-type key adjustment before `fs.Open` (request_content.go:119-123):
/// fs storages get `filepath.FromSlash(key)` (platform separators — a no-op on
/// the POSIX hosts the Go gateway targets, `\` on Windows); path-style S3
/// strips the single leading `/`.
fn adjust_key_for_storage(ds: &ContentDataStorage, key: &str) -> String {
    match ds.storage_type {
        ContentStorageType::Fs => key.replace('/', std::path::MAIN_SEPARATOR_STR),
        ContentStorageType::S3 if ds.s3_path_style => {
            key.strip_prefix('/').unwrap_or(key).to_string()
        }
        _ => key.to_string(),
    }
}

/// Build the external-storage success response
/// (request_content.go:132-143):
///
/// * `Content-Length` only when the stat size is known (134-136);
/// * `Content-Disposition: attachment; filename=%q` (138);
/// * `Content-Type: application/octet-stream` (139);
/// * `Cache-Control: private, max-age=0, no-cache` (140);
/// * status 200 + body copy (142-143).
fn octet_stream_attachment_response(filename: &str, file: ContentFile) -> Response {
    let disposition = ContentDisposition::attachment(filename).header_value();
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CONTENT_TYPE, BINARY_CONTENT_TYPE)
        .header(header::CACHE_CONTROL, CONTENT_CACHE_CONTROL);
    if let Some(size) = file.size {
        builder = builder.header(header::CONTENT_LENGTH, size);
    }
    match builder.body(Body::from(file.data)) {
        Ok(response) => response,
        // Unreachable with the fixed header set above; degrade to the Go
        // open-failure 500 rather than panicking (workspace forbids unwrap).
        Err(_) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to open content storage",
        ),
    }
}

/// Port of gin `Context.FileAttachment(fullPath, filename)` +
/// `http.ServeFile` as used by the fs fast path (request_content.go:109).
///
/// * gin sets `Content-Disposition: attachment; filename="<escaped>"` for
///   ASCII filenames (escaping `\` and `"`), or
///   `attachment; filename*=UTF-8''<url.QueryEscape(name)>` otherwise.
/// * `http.ServeFile` then streams the file; on any open failure it writes the
///   plain-text `404 page not found` (Content-Type `text/plain; charset=utf-8`,
///   `X-Content-Type-Options: nosniff`) — NOT the JSONError shape.
/// * Content-Type on success comes from `mime.TypeByExtension`; we cover the
///   extensions Conduit API stores (video/audio/json) and fall back to
///   `application/octet-stream` instead of Go's byte-sniffing.
async fn serve_file_attachment(full_path: PathBuf, filename: &str) -> Response {
    let disposition = file_attachment_disposition(filename);
    let read = tokio::task::spawn_blocking(move || std::fs::read(&full_path)).await;
    let data = match read {
        Ok(Ok(data)) => data,
        // I/O error (missing file) or join error: http.ServeFile's toHTTPError
        // collapses open failures to 404 "404 page not found".
        _ => {
            return (
                StatusCode::NOT_FOUND,
                [
                    (
                        header::CONTENT_TYPE,
                        "text/plain; charset=utf-8".to_string(),
                    ),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                ],
                // Go http.Error appends "\n" via Fprintln.
                "404 page not found\n",
            )
                .into_response();
        }
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_DISPOSITION, disposition),
            (
                header::CONTENT_TYPE,
                content_type_by_extension(filename).to_string(),
            ),
        ],
        Body::from(data),
    )
        .into_response()
}

/// gin `FileAttachment` Content-Disposition value: ASCII names use
/// `attachment; filename="<escapeQuotes(name)>"` (escaping `\` and `"`),
/// non-ASCII names use the RFC 5987 form
/// `attachment; filename*=UTF-8''<url.QueryEscape(name)>`.
fn file_attachment_disposition(filename: &str) -> String {
    if filename.is_ascii() {
        let escaped = filename.replace('\\', "\\\\").replace('"', "\\\"");
        format!("attachment; filename=\"{escaped}\"")
    } else {
        format!("attachment; filename*=UTF-8''{}", go_query_escape(filename))
    }
}

/// Port of Go `url.QueryEscape`: unreserved `[A-Za-z0-9-_.~]` kept, space
/// becomes `+`, everything else percent-encoded (uppercase hex) per byte.
fn go_query_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Minimal `mime.TypeByExtension` port for the content types Conduit API persists
/// (video/audio artifacts). Unknown extensions fall back to
/// `application/octet-stream` (Go would sniff the first 512 bytes instead —
/// documented approximation).
fn content_type_by_extension(filename: &str) -> &'static str {
    let ext = filename.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "json" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        _ => BINARY_CONTENT_TYPE,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::error::Error as StdError;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::to_bytes;
    use axum::http::Request;
    use conduit_config::AppConfig;
    use serde_json::{Value, json};
    use tower::Service;

    use super::*;
    use crate::app_state::AppServices;
    use crate::middleware::{
        JwtIdentityResolution, JwtIdentityResolver, JwtUserIdentity, PROJECT_ID_HEADER,
    };
    use crate::router::build_router;

    /// P-22 test seam: the content route is now behind an owner/read_requests
    /// check. The minted JWT resolves to an owner via this resolver, so the
    /// existing golden cases exercise the *authorized* path; a dedicated test
    /// (`unauthorized_caller_gets_not_found`) covers the deny path.
    struct OwnerResolver;

    #[async_trait::async_trait]
    impl JwtIdentityResolver for OwnerResolver {
        async fn resolve(&self, _user_id: i64) -> JwtIdentityResolution {
            JwtIdentityResolution::Found(JwtUserIdentity {
                is_owner: true,
                scope_slugs: Vec::new(),
            })
        }
    }

    /// A non-owner caller with NO `read_requests` scope — must be denied (P-22).
    struct DenyResolver;

    #[async_trait::async_trait]
    impl JwtIdentityResolver for DenyResolver {
        async fn resolve(&self, _user_id: i64) -> JwtIdentityResolution {
            JwtIdentityResolution::Found(JwtUserIdentity {
                is_owner: false,
                scope_slugs: Vec::new(),
            })
        }
    }

    /// Configurable fake standing in for the ent client + DataStorageService
    /// pair (mirrors the enttest wiring in request_content_test.go:31-54).
    #[derive(Default)]
    struct FakeContentService {
        rows: HashMap<i64, ContentRequestRow>,
        storages: HashMap<i64, ContentDataStorage>,
        fail_get_request: bool,
        fail_get_storage: bool,
        open_result: Option<Result<ContentFile, ContentOpenError>>,
        /// Records the key passed to `open_content` for adjustment assertions.
        opened_key: Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl RequestContentService for FakeContentService {
        async fn get_request(&self, request_id: i64) -> Result<Option<ContentRequestRow>, String> {
            if self.fail_get_request {
                return Err("db down".to_string());
            }
            Ok(self.rows.get(&request_id).cloned())
        }

        async fn get_data_storage(
            &self,
            storage_id: i64,
        ) -> Result<Option<ContentDataStorage>, String> {
            if self.fail_get_storage {
                return Err("db down".to_string());
            }
            Ok(self.storages.get(&storage_id).cloned())
        }

        async fn open_content(
            &self,
            _storage: &ContentDataStorage,
            key: &str,
        ) -> Result<ContentFile, ContentOpenError> {
            if let Ok(mut opened) = self.opened_key.lock() {
                *opened = Some(key.to_string());
            }
            self.open_result
                .clone()
                .unwrap_or(Err(ContentOpenError::NotFound))
        }
    }

    /// Saved row mirroring the fixture in request_content_test.go:84-107
    /// (project 1, request 42, fs storage 7, key under the request dir).
    fn saved_row(
        project_id: i64,
        request_id: i64,
        storage_id: i64,
        key: &str,
    ) -> ContentRequestRow {
        ContentRequestRow {
            id: request_id,
            project_id,
            content_saved: true,
            content_storage_id: Some(storage_id),
            content_storage_key: Some(key.to_string()),
        }
    }

    fn fs_storage(directory: Option<&str>) -> ContentDataStorage {
        ContentDataStorage {
            storage_id: 9,
            primary: false,
            storage_type: ContentStorageType::Fs,
            directory: directory.map(str::to_string),
            s3_path_style: false,
        }
    }

    /// Shared HS256 secret for the admin-group JWT guard in these tests.
    ///
    /// `/admin/requests/{request_id}/content` lives under Go's `adminGroup`
    /// (`middleware.WithJWTAuth`, routes.go:96); the Rust router mounts it
    /// behind `jwt_admin_auth`, which reads its signing secret from
    /// `config.api_auth.jwt_secret`. The fixtures set the same secret used by
    /// [`mint_admin_jwt`] so a valid bearer token reaches the handler.
    const TEST_JWT_SECRET: &str = "request-content-test-secret";

    /// Mint a valid HS256 bearer token accepted by the admin JWT guard,
    /// signed with [`TEST_JWT_SECRET`].
    fn mint_admin_jwt() -> String {
        use conduit_auth::jwt::{Claims, encode_hs256};
        encode_hs256(&Claims::new(42, "user:42".to_string()), TEST_JWT_SECRET).unwrap_or_default()
    }

    fn app_with(service: FakeContentService) -> Router {
        let mut config = AppConfig::default();
        config.api_auth.jwt_secret = Some(TEST_JWT_SECRET.to_string());
        let services = AppServices::new()
            .with_request_content_service(Arc::new(service))
            .with_user_principal_service(Arc::new(OwnerResolver));
        build_router(AppState::new(Arc::new(config), Arc::new(services)))
    }

    /// Same as [`app_with`] but the JWT caller is a non-owner without
    /// `read_requests` — used to assert the P-22 deny path.
    fn app_with_unauthorized(service: FakeContentService) -> Router {
        let mut config = AppConfig::default();
        config.api_auth.jwt_secret = Some(TEST_JWT_SECRET.to_string());
        let services = AppServices::new()
            .with_request_content_service(Arc::new(service))
            .with_user_principal_service(Arc::new(DenyResolver));
        build_router(AppState::new(Arc::new(config), Arc::new(services)))
    }

    /// P-22: a valid JWT + a matching `X-Project-ID` is NOT enough — a caller
    /// lacking owner/`read_requests` gets 404 (the IDOR is closed; existence is
    /// never leaked).
    #[tokio::test]
    async fn unauthorized_caller_gets_not_found() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with_unauthorized(FakeContentService::default());
        let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        Ok(())
    }

    /// Router with the JWT guard secret wired but NO request-content service,
    /// exercising the handler's unwired-service degradation branch. The secret
    /// is required so the request clears the `jwt_admin_auth` guard and reaches
    /// the handler (a bare `AppState::default()` has no secret, so the guard
    /// would 500 with "Failed to validate token" before the handler runs).
    fn app_without_service() -> Router {
        let mut config = AppConfig::default();
        config.api_auth.jwt_secret = Some(TEST_JWT_SECRET.to_string());
        build_router(AppState::new(
            Arc::new(config),
            Arc::new(AppServices::default().with_user_principal_service(Arc::new(OwnerResolver))),
        ))
    }

    /// GET the content route with an `X-Project-ID` GUID header
    /// (`gid://conduit/Project/<id>` — middleware/project.go:16-22).
    async fn get_content(
        app: &mut Router,
        request_id: &str,
        project_header: Option<&str>,
    ) -> Result<Response, Box<dyn StdError>> {
        // The route sits under Go's `adminGroup` JWT guard (routes.go:96);
        // attach a valid bearer token so the request reaches the handler
        // instead of short-circuiting at the `jwt_admin_auth` 401.
        let mut builder = Request::builder()
            .uri(format!("/admin/requests/{request_id}/content"))
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", mint_admin_jwt()),
            );
        if let Some(header_value) = project_header {
            builder = builder.header(PROJECT_ID_HEADER, header_value);
        }
        let request = builder.body(axum::body::Body::empty())?;
        Ok(app.call(request).await?)
    }

    fn project_guid(id: i64) -> String {
        format!("gid://conduit/Project/{id}")
    }

    async fn response_json(response: Response) -> Result<(StatusCode, Value), Box<dyn StdError>> {
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    // ---- pure helper golden cases ----

    /// GUID/type/id gating flows through `middleware::project_id_outcome`
    /// (project.go port, tested there); this covers the handler-level
    /// `projectID <= 0` gate stacked on top (request_content.go:43-47).
    #[test]
    fn resolve_project_id_gates_non_positive_ids() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            resolve_project_id(&headers),
            Err(ProjectIdRejection::NotFoundInContext)
        );

        if let Ok(value) = "gid://conduit/Project/0".parse() {
            headers.insert(PROJECT_ID_HEADER, value);
        }
        assert_eq!(
            resolve_project_id(&headers),
            Err(ProjectIdRejection::NotFoundInContext)
        );

        if let Ok(value) = "gid://conduit/Project/12".parse() {
            headers.insert(PROJECT_ID_HEADER, value);
        }
        assert_eq!(resolve_project_id(&headers), Ok(12));

        if let Ok(value) = "gid://conduit/User/12".parse() {
            headers.insert(PROJECT_ID_HEADER, value);
        }
        assert_eq!(
            resolve_project_id(&headers),
            Err(ProjectIdRejection::InvalidProjectId)
        );
    }

    /// strconv.ParseInt error strings surfaced through gin's uri binding.
    #[test]
    fn parse_request_id_param_matches_go_strconv_errors() {
        assert_eq!(parse_request_id_param("42"), Ok(42));
        assert_eq!(
            parse_request_id_param("abc"),
            Err("strconv.ParseInt: parsing \"abc\": invalid syntax".to_string())
        );
        assert_eq!(
            parse_request_id_param("99999999999999999999"),
            Err(
                "strconv.ParseInt: parsing \"99999999999999999999\": value out of range"
                    .to_string()
            )
        );
    }

    /// url.QueryEscape port golden cases (Go net/url).
    #[test]
    fn go_query_escape_matches_go() {
        assert_eq!(go_query_escape("视频.mp4"), "%E8%A7%86%E9%A2%91.mp4");
        assert_eq!(go_query_escape("a b.txt"), "a+b.txt");
        assert_eq!(go_query_escape("safe-name_1.~"), "safe-name_1.~");
    }

    /// Storage-key adjustment (request_content.go:119-123).
    #[test]
    fn adjust_key_for_storage_matches_go_branches() {
        let mut s3 = ContentDataStorage {
            storage_id: 9,
            primary: false,
            storage_type: ContentStorageType::S3,
            directory: None,
            s3_path_style: true,
        };
        assert_eq!(
            adjust_key_for_storage(&s3, "/1/requests/2/video/video.mp4"),
            "1/requests/2/video/video.mp4"
        );
        s3.s3_path_style = false;
        assert_eq!(
            adjust_key_for_storage(&s3, "/1/requests/2/video/video.mp4"),
            "/1/requests/2/video/video.mp4"
        );
        // fs: filepath.FromSlash — platform separators.
        let fs = fs_storage(None);
        assert_eq!(
            adjust_key_for_storage(&fs, "/a/b"),
            format!("{sep}a{sep}b", sep = std::path::MAIN_SEPARATOR_STR)
        );
    }

    // ---- integration: fs fast path (mirrors request_content_test.go) ----

    /// Mirrors `TestRequestContentHandlers_DownloadRequestContent/downloads
    /// content` (request_content_test.go:111-128): fs storage with a local
    /// directory serves the file with an attachment disposition.
    #[tokio::test]
    async fn downloads_content_from_fs_directory() -> Result<(), Box<dyn StdError>> {
        let dir = tempfile::tempdir()?;
        let key = "/1/requests/42/video/video.mp4";
        let full_path = dir.path().join("1/requests/42/video");
        std::fs::create_dir_all(&full_path)?;
        std::fs::write(full_path.join("video.mp4"), b"video-content")?;

        let mut service = FakeContentService::default();
        service.rows.insert(42, saved_row(1, 42, 7, key));
        service
            .storages
            .insert(7, fs_storage(Some(&dir.path().to_string_lossy())));

        let mut app = app_with(service);
        let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;

        let status = response.status();
        let disposition = response
            .headers()
            .get(header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = to_bytes(response.into_body(), 64 * 1024).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"video-content");
        // Go asserts Contains "video.mp4"; the exact gin FileAttachment value
        // is `attachment; filename="video.mp4"`.
        assert!(disposition.contains("video.mp4"), "{disposition}");
        assert_eq!(disposition, "attachment; filename=\"video.mp4\"");
        Ok(())
    }

    /// Mirrors `returns 404 for mismatched project`
    /// (request_content_test.go:130-147).
    #[tokio::test]
    async fn returns_404_for_mismatched_project() -> Result<(), Box<dyn StdError>> {
        let mut service = FakeContentService::default();
        service
            .rows
            .insert(42, saved_row(1, 42, 7, "/1/requests/42/video/video.mp4"));
        service.storages.insert(7, fs_storage(Some("/tmp")));

        let mut app = app_with(service);
        let response = get_content(&mut app, "42", Some(&project_guid(1000))).await?;
        let (status, body) = response_json(response).await?;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body,
            json!({"error": {"type": "Not Found", "message": "Request not found"}})
        );
        Ok(())
    }

    /// Mirrors `returns 404 when not saved` (request_content_test.go:149-175):
    /// the content gate (request_content.go:70-73).
    #[tokio::test]
    async fn returns_404_when_content_not_saved() -> Result<(), Box<dyn StdError>> {
        let mut service = FakeContentService::default();
        service.rows.insert(
            43,
            ContentRequestRow {
                id: 43,
                project_id: 1,
                content_saved: false,
                content_storage_id: None,
                content_storage_key: None,
            },
        );

        let mut app = app_with(service);
        let response = get_content(&mut app, "43", Some(&project_guid(1))).await?;
        let (status, body) = response_json(response).await?;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "Content not found");
        Ok(())
    }

    /// Mirrors `escapes directory traversal on fs`
    /// (request_content_test.go:177-206): the traversal key
    /// `/../../etc/passwd` fails the request-prefix check first
    /// (request_content.go:80-84) → 404 "Content not found".
    #[tokio::test]
    async fn escapes_directory_traversal_on_fs() -> Result<(), Box<dyn StdError>> {
        let mut service = FakeContentService::default();
        service
            .rows
            .insert(44, saved_row(1, 44, 7, "/../../etc/passwd"));
        service.storages.insert(7, fs_storage(Some("/tmp")));

        let mut app = app_with(service);
        let response = get_content(&mut app, "44", Some(&project_guid(1))).await?;
        let (status, body) = response_json(response).await?;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "Content not found");
        Ok(())
    }

    /// fs fast path with a missing file: http.ServeFile's plain-text 404
    /// (NOT the JSONError shape).
    #[tokio::test]
    async fn fs_fast_path_missing_file_returns_serve_file_404() -> Result<(), Box<dyn StdError>> {
        let dir = tempfile::tempdir()?;
        let mut service = FakeContentService::default();
        service
            .rows
            .insert(42, saved_row(1, 42, 7, "/1/requests/42/video/missing.mp4"));
        service
            .storages
            .insert(7, fs_storage(Some(&dir.path().to_string_lossy())));

        let mut app = app_with(service);
        let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;

        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let nosniff = response
            .headers()
            .get(header::X_CONTENT_TYPE_OPTIONS)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = to_bytes(response.into_body(), 1024).await?;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(content_type, "text/plain; charset=utf-8");
        assert_eq!(nosniff, "nosniff");
        assert_eq!(&body[..], b"404 page not found\n");
        Ok(())
    }

    // ---- integration: external storage path ----

    /// External storage success: headers verbatim from
    /// request_content.go:132-143 (Content-Length from Stat, `%q` attachment,
    /// octet-stream, private no-cache) and the path-style S3 key strip.
    #[tokio::test]
    async fn downloads_content_from_external_storage() -> Result<(), Box<dyn StdError>> {
        let mut service = FakeContentService::default();
        service
            .rows
            .insert(42, saved_row(1, 42, 9, "/1/requests/42/video/video.mp4"));
        service.storages.insert(
            9,
            ContentDataStorage {
                storage_id: 9,
                primary: false,
                storage_type: ContentStorageType::S3,
                directory: None,
                s3_path_style: true,
            },
        );
        service.open_result = Some(Ok(ContentFile {
            data: b"remote-bytes".to_vec(),
            size: Some(12),
        }));
        let service = Arc::new(service);
        let services = AppServices::new()
            .with_request_content_service(Arc::clone(&service) as Arc<_>)
            .with_user_principal_service(Arc::new(OwnerResolver));
        // Wire the JWT secret so the admin-group guard (routes.go:96) accepts
        // the token attached by `get_content`.
        let mut config = AppConfig::default();
        config.api_auth.jwt_secret = Some(TEST_JWT_SECRET.to_string());
        let mut app = build_router(AppState::new(Arc::new(config), Arc::new(services)));

        let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), 64 * 1024).await?;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"remote-bytes");
        assert_eq!(
            headers
                .get(header::CONTENT_DISPOSITION)
                .and_then(|v| v.to_str().ok()),
            Some("attachment; filename=\"video.mp4\"")
        );
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/octet-stream")
        );
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("private, max-age=0, no-cache")
        );
        assert_eq!(
            headers
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok()),
            Some("12")
        );
        // Path-style S3: leading '/' stripped before fs.Open
        // (request_content.go:121-123).
        let opened = service.opened_key.lock().map(|k| k.clone());
        assert_eq!(
            opened.ok().flatten().as_deref(),
            Some("1/requests/42/video/video.mp4")
        );
        Ok(())
    }

    /// External-storage failure branches: open NotFound → 404 "Content not
    /// found" (125-129); GetFileSystem failure → 500 "Failed to open content
    /// storage" (113-117).
    #[tokio::test]
    async fn external_storage_open_failures_map_to_go_branches() -> Result<(), Box<dyn StdError>> {
        for (open_result, want_status, want_message) in [
            (
                Err(ContentOpenError::NotFound),
                StatusCode::NOT_FOUND,
                "Content not found",
            ),
            (
                Err(ContentOpenError::StorageUnavailable),
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to open content storage",
            ),
        ] {
            let mut service = FakeContentService::default();
            service
                .rows
                .insert(42, saved_row(1, 42, 9, "/1/requests/42/video/video.mp4"));
            // fs storage WITHOUT a directory skips the fast path
            // (request_content.go:101) and goes through GetFileSystem.
            service.storages.insert(9, fs_storage(None));
            service.open_result = Some(open_result);

            let mut app = app_with(service);
            let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;
            let (status, body) = response_json(response).await?;

            assert_eq!(status, want_status, "{want_message}");
            assert_eq!(body["error"]["message"], want_message);
        }
        Ok(())
    }

    /// Storage-row branches: missing row → 404 "Content storage not found"
    /// (88-90); lookup error → 500 "Failed to load content storage" (92);
    /// primary / database storage → 400 "Content storage is not file-based"
    /// (96-99).
    #[tokio::test]
    async fn storage_row_branches_match_go() -> Result<(), Box<dyn StdError>> {
        // Missing storage row.
        let mut service = FakeContentService::default();
        service
            .rows
            .insert(42, saved_row(1, 42, 9, "/1/requests/42/a.bin"));
        let mut app = app_with(service);
        let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;
        let (status, body) = response_json(response).await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "Content storage not found");

        // Lookup failure.
        let mut service = FakeContentService::default();
        service
            .rows
            .insert(42, saved_row(1, 42, 9, "/1/requests/42/a.bin"));
        service.fail_get_storage = true;
        let mut app = app_with(service);
        let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;
        let (status, body) = response_json(response).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["message"], "Failed to load content storage");

        // Primary or database-typed storage.
        for storage in [
            ContentDataStorage {
                storage_id: 9,
                primary: true,
                storage_type: ContentStorageType::Fs,
                directory: None,
                s3_path_style: false,
            },
            ContentDataStorage {
                storage_id: 9,
                primary: false,
                storage_type: ContentStorageType::Database,
                directory: None,
                s3_path_style: false,
            },
        ] {
            let mut service = FakeContentService::default();
            service
                .rows
                .insert(42, saved_row(1, 42, 9, "/1/requests/42/a.bin"));
            service.storages.insert(9, storage);
            let mut app = app_with(service);
            let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;
            let (status, body) = response_json(response).await?;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(
                body["error"]["message"],
                "Content storage is not file-based"
            );
        }
        Ok(())
    }

    /// Request-row branches: not found → 404 "Request not found" (57-60);
    /// load failure → 500 "Failed to load request" (61); unwired service
    /// degrades to the same 500.
    #[tokio::test]
    async fn request_row_branches_match_go() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(FakeContentService::default());
        let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;
        let (status, body) = response_json(response).await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["message"], "Request not found");

        let service = FakeContentService {
            fail_get_request: true,
            ..FakeContentService::default()
        };
        let mut app = app_with(service);
        let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;
        let (status, body) = response_json(response).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["message"], "Failed to load request");

        // Unwired service still clears the JWT guard (secret wired) and then
        // degrades to the handler's 500 branch.
        let mut app = app_without_service();
        let response = get_content(&mut app, "42", Some(&project_guid(1))).await?;
        let (status, body) = response_json(response).await?;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["message"], "Failed to load request");
        Ok(())
    }

    /// Project-header branches: absent → 400 "Project ID not found in
    /// context" (43-47); invalid GUID / wrong type → the middleware 400
    /// "Invalid project ID" (project.go:22-26).
    #[tokio::test]
    async fn project_header_branches_match_go() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(FakeContentService::default());
        let response = get_content(&mut app, "42", None).await?;
        let (status, body) = response_json(response).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["message"], "Project ID not found in context");

        for bad in [
            "not-a-guid",
            "gid://conduit/User/1",
            "gid://conduit/Project/x",
        ] {
            let mut app = app_with(FakeContentService::default());
            let response = get_content(&mut app, "42", Some(bad)).await?;
            let (status, body) = response_json(response).await?;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(body["error"]["message"], "Invalid project ID", "{bad}");
        }
        Ok(())
    }

    /// gin ShouldBindUri failure (49-53): non-integer request_id → 400 with
    /// the wrapped strconv message.
    #[tokio::test]
    async fn invalid_request_id_returns_strconv_wrapped_400() -> Result<(), Box<dyn StdError>> {
        let mut app = app_with(FakeContentService::default());
        let response = get_content(&mut app, "abc", Some(&project_guid(1))).await?;
        let (status, body) = response_json(response).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"]["message"],
            "Invalid request body: strconv.ParseInt: parsing \"abc\": invalid syntax"
        );
        Ok(())
    }
}
