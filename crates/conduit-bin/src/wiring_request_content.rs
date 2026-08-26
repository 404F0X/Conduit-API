//! WIRE-REQUEST-CONTENT — host adapters for the two `/admin/requests/...`
//! detail endpoints, filling the last two `AppServices` handler seams:
//!
//! * [`conduit_http::request_content_handlers::RequestContentService`] —
//!   `GET /admin/requests/{request_id}/content` (Go `request_content.go`,
//!   `DownloadRequestContent`). Bridged by [`DbRequestContentService`].
//! * [`conduit_http::request_preview_handlers::RequestPreviewService`] —
//!   `GET /admin/requests/{request_id}/preview` (Go `request_live.go`,
//!   `PreviewRequest`). Bridged by [`DbRequestPreviewService`].
//!
//! ## Go parity anchors
//!
//! Go wires both handlers over the request-scoped ent client
//! (`ent.FromContext(ctx).Request.Get`) plus `*biz.DataStorageService` /
//! `*biz.RequestService` / `*biz.LiveStreamRegistry` via fx. The Rust host
//! folds that onto the sqlx repos:
//!
//! * `Request.Get` → [`RequestRepo::find_request_by_id_unchecked`] with the
//!   trusted admin [`RequestContext`] (the HTTP layer already enforces the
//!   JWT admin guard + `X-Project-ID` scoping before the service is called,
//!   mirroring Go's middleware stack).
//! * `DataStorageService.GetDataStorageByID` (biz/data_storage.go:281) →
//!   [`DataStorageRepo::find_data_storage_unchecked`] (both hide soft-deleted
//!   rows: ent interceptor ⇔ the repo's `deleted_at = 0` filter).
//! * `RequestService.LoadResponseChunks` (biz/request.go:1217-1258) →
//!   [`DbRequestPreviewService::load_response_chunks`], including the
//!   `getDataStorage` primary-fallback (biz/request_internal.go:10-22) and
//!   `shouldUseExternalStorage` (biz/request.go:59-67) decision tree.
//!
//! ## Deferred surfaces
//!
//! * **Live stream registry** — `biz.LiveStreamRegistry` is not wired in the
//!   host, so [`RequestPreviewService::get_request_buffer`] returns `None`
//!   (static-fetch fallback) and the processing-stream branch of
//!   `load_response_chunks` returns `Ok(None)`, byte-identical to Go's
//!   `GetRequestChunks` registry miss (nil slice → `"responseChunks":null`).
//!
//! External fs/s3/gcs/webdav persistence and read-back are wired through
//! `conduit-storage`; request/execution JSON is hydrated before GraphQL detail
//! nodes are returned.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use conduit_db::row::{DataStorageRow, RequestExecutionRow, RequestRow};
use conduit_db::{DataStorageRepo, PolicyContext, Principal, RequestContext, RequestRepo};
use conduit_http::request_content_handlers::{
    ContentDataStorage, ContentFile, ContentOpenError, ContentRequestRow, ContentStorageType,
    RequestContentService,
};
use conduit_http::request_preview_handlers::{
    PreviewChunkBuffer, PreviewRequestRow, REQUEST_STATUS_PROCESSING, RequestPreviewService,
};
use conduit_orchestrator::middlewares::persist::{RequestArtifactStorage, RequestStorageTarget};
use conduit_services::SystemService as DomainSystemService;
use conduit_storage::{DataStorageConfig, DataStorageKind, DataStorageService};

/// ent `request.StatusCompleted` (conduit/internal/ent/request/request.go —
/// the enum value the Go `LoadResponseChunks` gate compares against). The
/// processing sibling is re-used from the http crate's
/// [`REQUEST_STATUS_PROCESSING`].
const REQUEST_STATUS_COMPLETED: &str = "completed";

/// Trusted admin request context for repo calls, mirroring the fx-injected
/// request-scoped ent client Go hands these handlers (the HTTP layer has
/// already authenticated the caller). Same pattern as `wiring.rs`'s
/// `boot_request_context`.
fn admin_ctx() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::test()))
}

/// Parse a stringified integer edge id off a repo row (`CAST(id AS TEXT)` in
/// the SELECT). Failure means a corrupted row; surfaced as `Err` so the
/// handler renders its 500 "Failed to load ..." branch.
fn parse_i64(field: &'static str, raw: &str) -> Result<i64, String> {
    raw.parse::<i64>()
        .map_err(|_| format!("{field} is not an integer id: {raw:?}"))
}

/// [`parse_i64`] over an optional column.
fn parse_opt_i64(field: &'static str, raw: Option<&str>) -> Result<Option<i64>, String> {
    raw.map(|value| parse_i64(field, value)).transpose()
}

/// Map the `response_chunks` JSON column onto the handler's
/// `Option<Vec<Value>>` shape. Go `req.ResponseChunks` is
/// `[]objects.JSONRawMessage`: the column is either NULL / JSON `null` (a nil
/// slice, which marshals to `null` in the fallback payload) or a JSON array.
/// Any other value is unwritable through the Go persistence path and maps to
/// the nil-slice shape.
fn response_chunks_to_vec(value: Option<Value>) -> Option<Vec<Value>> {
    match value {
        Some(Value::Array(items)) => Some(items),
        _ => None,
    }
}

/// Project a full [`RequestRow`] onto the content handler's row shape
/// (request_content.go:55-84 — identity/ownership + the `content_*` columns).
fn content_request_row(row: &RequestRow) -> Result<ContentRequestRow, String> {
    Ok(ContentRequestRow {
        id: parse_i64("requests.id", &row.id)?,
        project_id: parse_i64("requests.project_id", &row.project_id)?,
        content_saved: row.content_saved,
        content_storage_id: parse_opt_i64(
            "requests.content_storage_id",
            row.content_storage_id.as_deref(),
        )?,
        content_storage_key: row.content_storage_key.clone(),
    })
}

/// Map the ent `datastorage.Type` enum string
/// (ent/datastorage/datastorage.go:111-115) onto the handler enum. Unknown
/// values are unrepresentable via the ent validator; a corrupted row degrades
/// to the handler's 500 "Failed to load content storage" branch.
fn content_storage_type(raw: &str) -> Result<ContentStorageType, String> {
    match raw {
        "database" => Ok(ContentStorageType::Database),
        "fs" => Ok(ContentStorageType::Fs),
        "s3" => Ok(ContentStorageType::S3),
        "gcs" => Ok(ContentStorageType::Gcs),
        "webdav" => Ok(ContentStorageType::Webdav),
        other => Err(format!("unknown data storage type: {other:?}")),
    }
}

/// Project a [`DataStorageRow`] onto the content handler's storage shape
/// (request_content.go:95-123). The `settings` JSON mirrors Go
/// `objects.DataStorageSettings` (internal/objects/data_stograge.go):
/// `directory` is `*string` (fs), `s3.pathStyle` is `bool`.
fn content_data_storage(row: &DataStorageRow) -> Result<ContentDataStorage, String> {
    Ok(ContentDataStorage {
        storage_id: parse_i64("data_storages.id", &row.id)?,
        primary: row.primary,
        storage_type: content_storage_type(&row.storage_type)?,
        // Go gates the fs fast path on `ds.Settings != nil &&
        // ds.Settings.Directory != nil` (request_content.go:100): JSON
        // null / absent ⇔ a nil pointer ⇔ `None`.
        directory: match row.settings.get("directory") {
            Some(Value::String(directory)) => Some(directory.clone()),
            _ => None,
        },
        // request_content.go:120 — `ds.Settings.S3 != nil && PathStyle`.
        s3_path_style: row
            .settings
            .get("s3")
            .and_then(|s3| s3.get("pathStyle"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Map the ent `datastorage.Type` enum string onto the storage crate's
/// [`DataStorageKind`]. Mirrors Go's `buildFileSystem` type switch
/// (`biz/data_storage.go:145-190`): `fs` → local filesystem, the object
/// backends map 1:1. `database` never reaches `open_content` (the handler
/// rejects primary/database storages before calling it, request_content.go:96),
/// but is mapped to `Memory` for completeness so an unexpected caller degrades
/// rather than panicking.
fn storage_kind_from_type(storage_type: &ContentStorageType) -> DataStorageKind {
    match storage_type {
        ContentStorageType::Fs => DataStorageKind::Local,
        ContentStorageType::S3 => DataStorageKind::S3,
        ContentStorageType::Gcs => DataStorageKind::Gcs,
        ContentStorageType::Webdav => DataStorageKind::WebDav,
        ContentStorageType::Database => DataStorageKind::Memory,
    }
}

/// Build a live [`DataStorageService`] from a persisted [`DataStorageRow`],
/// mirroring Go's `DataStorageService.GetFileSystem` (biz/data_storage.go:481):
/// resolve the backend `Type` + parse the credential `settings` blob, then
/// construct the concrete filesystem/object client. A backend that fails to
/// build (bad/missing credentials, unsupported type) surfaces as `Err` so the
/// caller can map it onto Go's GetFileSystem-failure branch (500 "Failed to
/// open content storage").
pub(crate) fn build_data_storage_service(
    row: &DataStorageRow,
) -> Result<DataStorageService, String> {
    let storage_type = content_storage_type(&row.storage_type)?;
    let kind = storage_kind_from_type(&storage_type);
    let config = DataStorageConfig::from_value(&row.settings)
        .map_err(|err| format!("failed to parse data storage settings: {err}"))?;
    DataStorageService::new(kind, Some(&config))
        .map_err(|err| format!("failed to build storage backend: {err}"))
}

/// Normalize a Go-style storage key to the storage crate's slash-free key
/// convention. Go's `GenerateResponseChunksKey` / content keys carry a leading
/// `/` (`/{project}/requests/{id}/...`) because Go's afero filesystem accepts
/// rooted paths; the Rust storage adapters (`normalize_key`) reject a leading
/// slash and treat keys as relative. Stripping exactly one leading `/` bridges
/// the two conventions and is idempotent for already-relative keys. This is the
/// same adjustment the download handler applies via `adjust_key_for_storage`'s
/// path-style S3 branch (request_content.go:119-123).
pub(crate) fn storage_object_key(key: &str) -> &str {
    key.strip_prefix('/').unwrap_or(key)
}

/// Production bridge used by request persistence. It resolves the configured
/// default storage for every request and writes external artifacts through the
/// same backend factory used by admin downloads and previews.
pub(crate) struct DbRequestArtifactStorage {
    system: Arc<DomainSystemService>,
    data_storage_repo: Arc<dyn DataStorageRepo>,
}

impl DbRequestArtifactStorage {
    pub(crate) fn new(
        system: Arc<DomainSystemService>,
        data_storage_repo: Arc<dyn DataStorageRepo>,
    ) -> Self {
        Self {
            system,
            data_storage_repo,
        }
    }
}

#[async_trait]
impl RequestArtifactStorage for DbRequestArtifactStorage {
    async fn current_default(&self) -> Result<Option<RequestStorageTarget>, String> {
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let id = self
            .system
            .default_data_storage_id(&ctx)
            .await
            .map_err(|error| error.to_string())?;
        if id <= 0 {
            return Ok(None);
        }
        let id = id.to_string();
        let row = self
            .data_storage_repo
            .find_data_storage_unchecked(&ctx, &id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("default data storage {id} does not exist"))?;
        if row.status != "active" {
            return Err(format!("default data storage {id} is not active"));
        }
        Ok(Some(RequestStorageTarget {
            id,
            external: !row.primary,
        }))
    }

    async fn save(&self, storage_id: &str, key: &str, data: Vec<u8>) -> Result<(), String> {
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let row = self
            .data_storage_repo
            .find_data_storage_unchecked(&ctx, storage_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("data storage {storage_id} does not exist"))?;
        if row.primary {
            return Err(format!("data storage {storage_id} is not external"));
        }
        let service = build_data_storage_service(&row)?;
        service
            .save_data(storage_object_key(key), &data)
            .await
            .map_err(|error| error.to_string())
    }
}

async fn external_storage_for_row(
    repo: &dyn DataStorageRepo,
    storage_id: Option<&str>,
) -> Option<DataStorageService> {
    let storage_id = storage_id?;
    let ctx = admin_ctx();
    let row = repo
        .find_data_storage_unchecked(&ctx, storage_id)
        .await
        .ok()
        .flatten()?;
    if row.primary {
        return None;
    }
    build_data_storage_service(&row).ok()
}

async fn load_json_artifact(service: &DataStorageService, key: String) -> Option<Value> {
    let bytes = service.load_data(storage_object_key(&key)).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Hydrate externally stored request JSON before exposing a GraphQL node.
pub(crate) async fn hydrate_request_artifacts(repo: &dyn DataStorageRepo, row: &mut RequestRow) {
    let Some(service) = external_storage_for_row(repo, row.data_storage_id.as_deref()).await else {
        return;
    };
    let prefix = format!("/{}/requests/{}", row.project_id, row.id);
    if row.request_body.is_null()
        && let Some(value) =
            load_json_artifact(&service, format!("{prefix}/request_body.json")).await
    {
        row.request_body = value;
    }
    if row.response_body.is_none() {
        row.response_body =
            load_json_artifact(&service, format!("{prefix}/response_body.json")).await;
    }
    if row.response_chunks.is_none() {
        row.response_chunks =
            load_json_artifact(&service, format!("{prefix}/response_chunks.json")).await;
    }
}

/// Hydrate externally stored execution JSON before exposing a GraphQL node.
pub(crate) async fn hydrate_execution_artifacts(
    repo: &dyn DataStorageRepo,
    row: &mut RequestExecutionRow,
) {
    let Some(service) = external_storage_for_row(repo, row.data_storage_id.as_deref()).await else {
        return;
    };
    let prefix = format!(
        "/{}/requests/{}/executions/{}",
        row.project_id, row.request_id, row.id
    );
    if row.request_body.is_null()
        && let Some(value) =
            load_json_artifact(&service, format!("{prefix}/request_body.json")).await
    {
        row.request_body = value;
    }
    if row.response_body.is_none() {
        row.response_body =
            load_json_artifact(&service, format!("{prefix}/response_body.json")).await;
    }
    if row.response_chunks.is_none() {
        row.response_chunks =
            load_json_artifact(&service, format!("{prefix}/response_chunks.json")).await;
    }
}

// ---------------------------------------------------------------------------
// Content download service
// ---------------------------------------------------------------------------

/// Host-side [`RequestContentService`] over the live request + data-storage
/// repos. Stands in for the fx pair Go injects into
/// `RequestContentHandlers` (request_content.go:20-34).
pub struct DbRequestContentService {
    request_repo: Arc<dyn RequestRepo>,
    data_storage_repo: Arc<dyn DataStorageRepo>,
}

impl DbRequestContentService {
    pub fn new(
        request_repo: Arc<dyn RequestRepo>,
        data_storage_repo: Arc<dyn DataStorageRepo>,
    ) -> Self {
        Self {
            request_repo,
            data_storage_repo,
        }
    }
}

#[async_trait]
impl RequestContentService for DbRequestContentService {
    /// `ent.FromContext(ctx).Request.Get(ctx, id)` (request_content.go:55-63).
    /// `Ok(None)` ⇔ `ent.IsNotFound` → handler 404 "Request not found";
    /// `Err` → handler 500 "Failed to load request" (payload is log-only).
    async fn get_request(&self, request_id: i64) -> Result<Option<ContentRequestRow>, String> {
        let ctx = admin_ctx();
        let Some(row) = self
            .request_repo
            .find_request_by_id_unchecked(&ctx, &request_id.to_string())
            .await
            .map_err(|err| err.to_string())?
        else {
            return Ok(None);
        };
        Ok(Some(content_request_row(&row)?))
    }

    /// `DataStorageService.GetDataStorageByID` (request_content.go:85-93,
    /// biz/data_storage.go:281). Soft-deleted rows are hidden by the repo's
    /// `deleted_at = 0` filter, matching the ent soft-delete interceptor —
    /// both surface as `Ok(None)` → handler 404 "Content storage not found".
    async fn get_data_storage(
        &self,
        storage_id: i64,
    ) -> Result<Option<ContentDataStorage>, String> {
        let ctx = admin_ctx();
        let Some(row) = self
            .data_storage_repo
            .find_data_storage_unchecked(&ctx, &storage_id.to_string())
            .await
            .map_err(|err| err.to_string())?
        else {
            return Ok(None);
        };
        Ok(Some(content_data_storage(&row)?))
    }

    /// `DataStorageService.GetFileSystem` + `fs.Open(key)` + read
    /// (request_content.go:112-143, biz/data_storage.go:483).
    ///
    /// The handler only reaches here for non-primary, non-database file-based
    /// storages whose fs fast path did not fire (an fs storage without a local
    /// `directory`, or an s3/gcs/webdav backend). We reload the full storage
    /// row by id to recover its credential `settings` (the projected
    /// [`ContentDataStorage`] intentionally drops them), build the concrete
    /// backend via the storage crate, and read the object.
    ///
    /// Failure mapping mirrors Go's two branches:
    /// - Backend build failure ⇔ `GetFileSystem` error → [`StorageUnavailable`]
    ///   (500 "Failed to open content storage", request_content.go:113-117).
    /// - Missing object / read failure ⇔ `fs.Open` error → [`NotFound`]
    ///   (404 "Content not found", request_content.go:125-129).
    ///
    /// [`StorageUnavailable`]: ContentOpenError::StorageUnavailable
    /// [`NotFound`]: ContentOpenError::NotFound
    async fn open_content(
        &self,
        storage: &ContentDataStorage,
        key: &str,
    ) -> Result<ContentFile, ContentOpenError> {
        let ctx = admin_ctx();
        // Reload the full row for the credential settings the projection drops.
        let row = self
            .data_storage_repo
            .find_data_storage_unchecked(&ctx, &storage.storage_id.to_string())
            .await
            .map_err(|_| ContentOpenError::StorageUnavailable)?
            .ok_or(ContentOpenError::StorageUnavailable)?;

        let service = build_data_storage_service(&row).map_err(|_| {
            // GetFileSystem-failure branch: bad/missing credentials or an
            // unsupported backend type.
            ContentOpenError::StorageUnavailable
        })?;

        // `LoadData` reads the object; a missing key / read error is the
        // fs.Open-failure branch (404), matching the facade's "failed to read
        // file" surface for object backends. The handler already applied
        // `adjust_key_for_storage`; normalize once more to the storage crate's
        // slash-free convention (strip leading `/`).
        match service.load_data(storage_object_key(key)).await {
            Ok(bytes) => {
                let size = bytes.len() as u64;
                Ok(ContentFile {
                    data: bytes,
                    size: Some(size),
                })
            }
            Err(_) => Err(ContentOpenError::NotFound),
        }
    }
}

// ---------------------------------------------------------------------------
// Live preview service
// ---------------------------------------------------------------------------

/// Host-side [`RequestPreviewService`] over the live request + data-storage
/// repos. Stands in for the fx pair `*biz.RequestService` +
/// `*biz.LiveStreamRegistry` plus the request-scoped ent client
/// (request_live.go:27-38).
pub struct DbRequestPreviewService {
    request_repo: Arc<dyn RequestRepo>,
    data_storage_repo: Arc<dyn DataStorageRepo>,
}

impl DbRequestPreviewService {
    pub fn new(
        request_repo: Arc<dyn RequestRepo>,
        data_storage_repo: Arc<dyn DataStorageRepo>,
    ) -> Self {
        Self {
            request_repo,
            data_storage_repo,
        }
    }
}

#[async_trait]
impl RequestPreviewService for DbRequestPreviewService {
    /// `ent.FromContext(ctx).Request.Get(ctx, id)` (request_live.go:112-120).
    async fn get_request(&self, request_id: i64) -> Result<Option<PreviewRequestRow>, String> {
        let ctx = admin_ctx();
        let Some(row) = self
            .request_repo
            .find_request_by_id_unchecked(&ctx, &request_id.to_string())
            .await
            .map_err(|err| err.to_string())?
        else {
            return Ok(None);
        };
        Ok(Some(PreviewRequestRow {
            id: parse_i64("requests.id", &row.id)?,
            project_id: parse_i64("requests.project_id", &row.project_id)?,
            status: row.status,
            stream: row.stream,
            response_chunks: response_chunks_to_vec(row.response_chunks),
        }))
    }

    /// `RequestService.LoadResponseChunks(ctx, req)` (biz/request.go:1217-1258)
    /// with the `getDataStorage` helper (biz/request_internal.go:10-22) and
    /// `shouldUseExternalStorage` (biz/request.go:59-67) folded in.
    async fn load_response_chunks(
        &self,
        request: &PreviewRequestRow,
    ) -> Result<Option<Vec<Value>>, String> {
        // request.go:1222-1226 — live streaming requests read the in-memory
        // registry. DEFER: `LiveStreamRegistry` is not wired in the host;
        // Go's `GetRequestChunks` returns a nil slice on a registry miss,
        // which is exactly `Ok(None)` here.
        if request.stream && request.status == REQUEST_STATUS_PROCESSING {
            return Ok(None);
        }

        // request.go:1227-1230 — only completed streaming requests carry
        // chunks; everything else is the empty non-nil slice (`[]`).
        if !request.stream || request.status != REQUEST_STATUS_COMPLETED {
            return Ok(Some(Vec::new()));
        }

        // Go works off the ent row already in hand (`req.DataStorageID` +
        // `req.ResponseChunks`); `PreviewRequestRow` doesn't carry those, so
        // reload the full row.
        let ctx = admin_ctx();
        let Some(row) = self
            .request_repo
            .find_request_by_id_unchecked(&ctx, &request.id.to_string())
            .await
            .map_err(|err| err.to_string())?
        else {
            // Row vanished between the handler load and here — degrade like
            // Go's getDataStorage warn branch (request.go:1231-1235).
            return Ok(Some(Vec::new()));
        };

        // getDataStorage (request_internal.go:10-22): a zero/unset
        // `data_storage_id` resolves the primary storage instead.
        let storage_lookup = match row.data_storage_id.as_deref() {
            None | Some("0") => {
                self.data_storage_repo
                    .find_primary_data_storage_unchecked(&ctx)
                    .await
            }
            Some(storage_id) => {
                self.data_storage_repo
                    .find_data_storage_unchecked(&ctx, storage_id)
                    .await
            }
        };
        let storage = match storage_lookup {
            Ok(Some(storage)) => storage,
            // Lookup failure or missing row (including "no primary storage",
            // Go's ent NotFound from GetPrimaryDataStorage): warn + empty
            // slice (request.go:1231-1235).
            Ok(None) | Err(_) => return Ok(Some(Vec::new())),
        };

        // shouldUseExternalStorage (request.go:59-67): primary storage keeps
        // the chunks inline on the requests row.
        if storage.primary {
            return Ok(response_chunks_to_vec(row.response_chunks));
        }

        // External storage chunk load (request.go:1237-1258): build the backend
        // from the storage row and read the `response_chunks.json` object. Every
        // failure below degrades to Go's warn + empty-slice branch
        // (request.go:1245-1257) — a missing backend, a read error, an empty
        // payload, or a malformed JSON body all yield `[]`, never an error.
        let Ok(service) = build_data_storage_service(&storage) else {
            return Ok(Some(Vec::new()));
        };
        // GenerateResponseChunksKey (biz/request.go:95):
        // `/{projectID}/requests/{requestID}/response_chunks.json`. Go's afero
        // accepts the leading slash; the Rust storage crate's key convention is
        // slash-free (see `storage_object_key`), so drop it here.
        let raw_key = format!(
            "/{}/requests/{}/response_chunks.json",
            request.project_id, request.id
        );
        let key = storage_object_key(&raw_key);
        let Ok(data) = service.load_data(key).await else {
            return Ok(Some(Vec::new()));
        };
        if data.is_empty() {
            return Ok(Some(Vec::new()));
        }
        // Go: `json.Unmarshal(data, &chunks)` into `[]objects.JSONRawMessage`;
        // a non-array/invalid body degrades to the empty slice.
        match serde_json::from_slice::<Vec<Value>>(&data) {
            Ok(chunks) => Ok(Some(chunks)),
            Err(_) => Ok(Some(Vec::new())),
        }
    }

    /// `LiveStreamRegistry.GetRequestBuffer(req.ID)` (request_live.go:132,
    /// biz/stream_preview.go:42-54).
    ///
    /// DEFER: the live-broadcast registry (Go: orchestrator
    /// `live_streaming.go` registering a `chunkbuffer.Buffer` per in-flight
    /// streaming request) is not wired in the host. Returning `None` takes
    /// the handler down the static-fetch fallback, exactly as Go behaves for
    /// any request with no registered buffer.
    fn get_request_buffer(&self, _request_id: i64) -> Option<Arc<dyn PreviewChunkBuffer>> {
        None
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use conduit_db::InMemoryDataStorageRepo;
    use serde_json::json;

    fn external_storage_row(directory: &std::path::Path) -> DataStorageRow {
        let now = Utc::now();
        DataStorageRow {
            id: "9".to_string(),
            name: "external".to_string(),
            status: "active".to_string(),
            description: String::new(),
            primary: false,
            storage_type: "fs".to_string(),
            settings: json!({"directory": directory.to_string_lossy()}),
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    fn external_request_row() -> RequestRow {
        let now = Utc::now();
        RequestRow {
            id: "11".to_string(),
            project_id: "7".to_string(),
            status: "completed".to_string(),
            source: "api".to_string(),
            model_id: "gpt-4".to_string(),
            format: "openai/chat_completions".to_string(),
            stream: false,
            client_ip: String::new(),
            content_saved: false,
            api_key_id: None,
            trace_id: None,
            data_storage_id: Some("9".to_string()),
            reasoning_effort: None,
            request_headers: None,
            request_body: Value::Null,
            response_body: None,
            response_chunks: None,
            channel_id: None,
            external_id: None,
            metrics_latency_ms: None,
            metrics_first_token_latency_ms: None,
            metrics_reasoning_duration_ms: None,
            content_storage_id: None,
            content_storage_key: None,
            content_saved_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn graphql_request_hydration_loads_external_json_artifacts()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let storage_row = external_storage_row(directory.path());
        let service = build_data_storage_service(&storage_row)?;
        service
            .save_data("7/requests/11/request_body.json", br#"{"model":"gpt-4"}"#)
            .await?;
        service
            .save_data(
                "7/requests/11/response_body.json",
                br#"{"id":"response-1"}"#,
            )
            .await?;
        service
            .save_data(
                "7/requests/11/response_chunks.json",
                br#"[{"delta":"hello"}]"#,
            )
            .await?;
        let repo = InMemoryDataStorageRepo::from_rows([storage_row]);
        let mut row = external_request_row();

        hydrate_request_artifacts(&repo, &mut row).await;

        assert_eq!(row.request_body, json!({"model": "gpt-4"}));
        assert_eq!(row.response_body, Some(json!({"id": "response-1"})));
        assert_eq!(row.response_chunks, Some(json!([{"delta": "hello"}])));
        Ok(())
    }
}
