use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_db::RequestContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::request_service::{RequestRecord, RequestService, RequestServiceError, RequestStatus};

pub type VideoServiceResult<T> = Result<T, VideoServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VideoServiceError {
    #[error("video task not found: {0}")]
    TaskNotFound(String),
    #[error("video task persistence lock poisoned")]
    LockPoisoned,
    /// **S08/S12**: the request row has no `external_id`. Go:
    /// `fmt.Errorf("%w: missing external_id for task", ErrInternal)`
    /// (`biz/video.go:128-130`; `ErrInternal` renders as "server internal
    /// error, please try again later", `biz/errors.go:15`).
    #[error("server internal error, please try again later: missing external_id for task")]
    MissingExternalId,
    /// **S12**: the request row has no `channel_id` (Go optional-int zero).
    /// Go: `fmt.Errorf("%w: missing channel_id for task", ErrInternal)`
    /// (`biz/video.go:132-134`).
    #[error("server internal error, please try again later: missing channel_id for task")]
    MissingChannelId,
    /// **S12**: the resolved channel has no video-task outbound transformer.
    /// Go: `fmt.Errorf("%w: channel does not support video task operations",
    /// ErrInternal)` (`biz/video.go:158-160`). Gateway implementors MUST map
    /// their "no outbound for this channel" case to this variant.
    #[error(
        "server internal error, please try again later: channel does not support video task operations"
    )]
    UnsupportedChannel,
    /// **S08/S12**: provider-side HTTP round-trip failure. Go propagates the
    /// raw transport/parse error from `ch.HTTPClient.Do` /
    /// `ParseGetVideoTaskResponse` unchanged (`biz/video.go:38-46, 106-109`);
    /// the Rust gateway folds it into this string-carrying variant.
    #[error("video task provider error: {0}")]
    Provider(String),
    /// **S08/S12**: local request-row persistence error (lookup or write).
    /// Go propagates ent errors unchanged (`biz/video.go:70-72, 88-90,
    /// 123-126`).
    #[error("request persistence error: {0}")]
    Request(#[from] RequestServiceError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoTaskStatus {
    Pending,
    Processing,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoContentMetadata {
    pub storage_id: Option<String>,
    pub storage_key: String,
    pub content_type: Option<String>,
    pub bytes: Option<u64>,
    pub saved_at: DateTime<Utc>,
    #[serde(flatten)]
    pub metadata: BTreeMap<String, Value>,
}

impl VideoContentMetadata {
    pub fn new(storage_key: impl Into<String>, saved_at: DateTime<Utc>) -> Self {
        Self {
            storage_id: None,
            storage_key: storage_key.into(),
            content_type: None,
            bytes: None,
            saved_at,
            metadata: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoTask {
    pub id: String,
    pub project_id: String,
    pub request_id: Option<String>,
    pub external_task_id: String,
    pub status: VideoTaskStatus,
    pub provider_status: Option<String>,
    pub content_saved: bool,
    pub content: Option<VideoContentMetadata>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(flatten)]
    pub metadata: BTreeMap<String, Value>,
}

impl VideoTask {
    pub fn new(
        project_id: impl Into<String>,
        external_task_id: impl Into<String>,
        request_id: Option<String>,
    ) -> Self {
        let project_id = project_id.into();
        let external_task_id = external_task_id.into();
        let now = Utc::now();
        Self {
            id: scoped_video_task_id(&project_id, &external_task_id),
            project_id,
            request_id,
            external_task_id,
            status: VideoTaskStatus::Pending,
            provider_status: None,
            content_saved: false,
            content: None,
            created_at: now,
            updated_at: now,
            metadata: BTreeMap::new(),
        }
    }

    pub fn with_status(mut self, status: VideoTaskStatus, provider_status: Option<String>) -> Self {
        self.status = status;
        self.provider_status = provider_status;
        self.updated_at = Utc::now();
        self
    }

    pub fn with_saved_content(mut self, content: VideoContentMetadata) -> Self {
        self.content_saved = true;
        self.content = Some(content);
        self.updated_at = Utc::now();
        self
    }
}

#[async_trait]
pub trait VideoTaskRepo: Send + Sync {
    async fn upsert_task(
        &self,
        ctx: &RequestContext,
        task: VideoTask,
    ) -> VideoServiceResult<VideoTask>;

    async fn get_task(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        task_id: &str,
    ) -> VideoServiceResult<Option<VideoTask>>;

    async fn get_task_by_external_id(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        external_task_id: &str,
    ) -> VideoServiceResult<Option<VideoTask>>;

    async fn delete_task(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        task_id: &str,
    ) -> VideoServiceResult<Option<VideoTask>>;
}

pub struct VideoService {
    repo: Arc<dyn VideoTaskRepo>,
}

impl VideoService {
    pub fn new(repo: Arc<dyn VideoTaskRepo>) -> Self {
        Self { repo }
    }

    pub async fn map_external_task(
        &self,
        ctx: &RequestContext,
        project_id: impl Into<String>,
        external_task_id: impl Into<String>,
        request_id: Option<String>,
    ) -> VideoServiceResult<VideoTask> {
        self.repo
            .upsert_task(
                ctx,
                VideoTask::new(project_id, external_task_id, request_id),
            )
            .await
    }

    pub async fn save_task(
        &self,
        ctx: &RequestContext,
        task: VideoTask,
    ) -> VideoServiceResult<VideoTask> {
        self.repo.upsert_task(ctx, task).await
    }

    pub async fn get_task(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        task_id: &str,
    ) -> VideoServiceResult<VideoTask> {
        self.repo
            .get_task(ctx, project_id, task_id)
            .await?
            .ok_or_else(|| VideoServiceError::TaskNotFound(task_id.to_string()))
    }

    pub async fn query_task_by_external_id(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        external_task_id: &str,
    ) -> VideoServiceResult<Option<VideoTask>> {
        self.repo
            .get_task_by_external_id(ctx, project_id, external_task_id)
            .await
    }

    pub async fn delete_task(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        task_id: &str,
    ) -> VideoServiceResult<VideoTask> {
        self.repo
            .delete_task(ctx, project_id, task_id)
            .await?
            .ok_or_else(|| VideoServiceError::TaskNotFound(task_id.to_string()))
    }
}

fn scoped_video_task_id(project_id: &str, external_task_id: &str) -> String {
    format!("video-task:{project_id}:{external_task_id}")
}

// ============================================================================
// RUST-P13-006 S07/S10/S11/S12 — pure scan/save-content/delete decision logic
//
// Mirrors `conduit/internal/server/video_storage/worker.go` +
// `conduit/internal/server/biz/video.go`. These functions are intentionally
// side-effect-free and DB-agnostic so they can be unit-tested without a
// runtime. The worker wiring (DB query, HTTP download, storage write) is a
// separate concern that will live in the scheduler/worker crate; here we only
// encode the *decisions* the worker must make.
// ============================================================================

/// Default scan limit when `VideoStorageSettings.scan_limit <= 0`.
/// Go: `if limit <= 0 { limit = 50 }` (worker.go L132-135).
pub const DEFAULT_VIDEO_SCAN_LIMIT: i64 = 50;

/// Default scan interval (minutes) when `VideoStorageSettings.scan_interval_minutes <= 0`.
/// Go: `if intervalMinutes <= 0 { intervalMinutes = 1 }` (worker.go L61-64, L83-86).
pub const DEFAULT_VIDEO_SCAN_INTERVAL_MINUTES: i64 = 1;

/// System settings for video storage persistence.
/// Mirrors Go `biz.VideoStorageSettings` (system.go L131-140). Go json tags are
/// **snake_case** (`data_storage_id`/`scan_interval_minutes`/`scan_limit`), so
/// this struct intentionally omits `rename_all` — Rust snake_case field names
/// serialize byte-identically to the Go wire form consumed by the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoStorageSettings {
    /// Controls whether to persist generated videos to external storage.
    #[serde(default)]
    pub enabled: bool,
    /// Target data storage ID for saving video files.
    #[serde(default)]
    pub data_storage_id: i64,
    /// How often (in minutes) to scan for completed video requests.
    #[serde(default)]
    pub scan_interval_minutes: i64,
    /// Max number of requests processed per scan.
    #[serde(default)]
    pub scan_limit: i64,
}

impl Default for VideoStorageSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            data_storage_id: 0,
            scan_interval_minutes: DEFAULT_VIDEO_SCAN_INTERVAL_MINUTES,
            scan_limit: DEFAULT_VIDEO_SCAN_LIMIT,
        }
    }
}

impl VideoStorageSettings {
    /// Returns the effective scan limit, applying the Go default of 50 when
    /// the configured value is non-positive.
    /// Go: `if limit <= 0 { limit = 50 }` (worker.go L132-135).
    pub fn effective_scan_limit(&self) -> i64 {
        if self.scan_limit <= 0 {
            DEFAULT_VIDEO_SCAN_LIMIT
        } else {
            self.scan_limit
        }
    }

    /// Returns the effective scan interval in minutes, applying the Go default
    /// of 1 minute when the configured value is non-positive.
    /// Go: `if intervalMinutes <= 0 { intervalMinutes = 1 }` (worker.go L61-64).
    pub fn effective_scan_interval_minutes(&self) -> i64 {
        if self.scan_interval_minutes <= 0 {
            DEFAULT_VIDEO_SCAN_INTERVAL_MINUTES
        } else {
            self.scan_interval_minutes
        }
    }
}

/// A candidate video request awaiting content save. The shape mirrors the
/// fields the Go worker reads off `ent.Request` to make scan/save decisions
/// (worker.go L137-222). It is deliberately minimal so the pure planner does
/// not need to know about the full request row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoScanCandidate {
    /// Monotonically increasing request id — Go orders by `request.FieldID`
    /// asc and slices at `Limit` (worker.go L143-144).
    pub request_id: i64,
    pub project_id: i64,
    /// Whether the request already has a saved video artifact. Go filters
    /// `ContentSaved(false)`; candidates fed to the planner should already be
    /// filtered, but this field is retained so the planner can double-check.
    #[serde(default)]
    pub content_saved: bool,
    /// Whether the request body already carries a downloadable video URL.
    /// Go: `extractVideoURLFromResponseBody` short-circuits the GetTask call
    /// (worker.go L161-178).
    #[serde(default)]
    pub has_cached_video_url: bool,
}

/// Result of `build_scan_plan`: the subset of candidates the worker should
/// process this cycle plus the resolved effective settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanPlan {
    /// Effective scan limit applied for this cycle.
    pub effective_limit: i64,
    /// Effective scan interval (minutes) — informational; the worker uses it
    /// to schedule the next cycle.
    pub effective_interval_minutes: i64,
    /// Candidate request ids selected for processing this cycle, in the order
    /// the worker should visit them (id-ascending, mirroring Go `Order(Asc)`).
    pub selected_request_ids: Vec<i64>,
    /// Number of candidates dropped because the limit was reached.
    pub deferred_count: usize,
}

/// Builds a scan plan from the worker's candidate set.
///
/// This is the pure decision half of `Worker.scanAndSave` (worker.go
/// L109-158). The DB query, `enabled`/`data_storage_id` validation and the
/// per-request download/upload loop are the caller's responsibility — here we
/// only decide *which* requests to process this cycle:
///
/// 1. Drop any candidate whose `content_saved` is already `true` (defensive;
///    the Go query already filters `ContentSaved(false)`, worker.go L141).
/// 2. Sort by `request_id` ascending (Go: `Order(ent.Asc(request.FieldID))`,
///    worker.go L143).
/// 3. Truncate at the effective limit (Go: `.Limit(limit)`, worker.go L144).
///
/// `now` is accepted for symmetry with future backoff logic but is not used
/// to filter candidates in the current Go implementation (the scan rate is
/// governed by the scheduler's `FixRate`, not per-row timestamps).
pub fn build_scan_plan(
    mut candidates: Vec<VideoScanCandidate>,
    settings: &VideoStorageSettings,
    _now: DateTime<Utc>,
) -> ScanPlan {
    let effective_limit = settings.effective_scan_limit();
    let effective_interval_minutes = settings.effective_scan_interval_minutes();

    // Defensive filter — Go's query already enforces ContentSaved(false), but
    // we re-check so that a stale feed cannot re-process saved rows.
    candidates.retain(|c| !c.content_saved);

    // Go: Order(ent.Asc(request.FieldID)) — stable ascending sort by id.
    candidates.sort_by_key(|c| c.request_id);

    let total = candidates.len();
    let limit = effective_limit.max(0) as usize;
    let selected_request_ids: Vec<i64> = candidates
        .iter()
        .take(limit)
        .map(|c| c.request_id)
        .collect();
    let deferred_count = total.saturating_sub(selected_request_ids.len());

    ScanPlan {
        effective_limit,
        effective_interval_minutes,
        selected_request_ids,
        deferred_count,
    }
}

/// Outcome of attempting to save one video request's content to external
/// storage. The caller (worker) produces this after running the actual
/// download + storage upload; `apply_save_outcome` then derives the DB row
/// update to persist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SaveOutcome {
    /// Save succeeded. Mirrors Go's successful path (worker.go L196-212):
    /// `SetContentSaved(true).SetContentStorageID(ds.ID).
    ///  SetContentStorageKey(storageKey).SetContentSavedAt(now)`.
    Saved {
        data_storage_id: i64,
        storage_key: String,
        saved_at: DateTime<Utc>,
    },
    /// Save failed. Mirrors Go's `log.Warn(...); continue` (worker.go L151-
    /// 155): the error is recorded but the request itself is left intact so
    /// the next scan cycle can retry.
    Failed { error: String },
    /// The request has no downloadable video URL yet (e.g. the provider task
    /// is still queued/running). Mirrors Go's early `return nil` paths
    /// (worker.go L173-178, L180-182).
    NotReady,
}

/// The DB row update the worker should apply for a given request after a save
/// attempt. Returned by `apply_save_outcome`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RequestUpdate {
    /// Persist `content_saved=true` plus storage coordinates.
    MarkSaved {
        content_storage_id: i64,
        content_storage_key: String,
        content_saved_at: DateTime<Utc>,
    },
    /// Do not mutate the request's saved-content fields. The caller may still
    /// record the error to logs/metrics. Mirrors Go's `continue` on error.
    LeaveUnchanged,
}

/// Derives the request update for a given save outcome.
///
/// This is the pure half of `Worker.processOne`'s success path (worker.go
/// L196-212). For `SaveOutcome::Saved` it returns the storage coordinates to
/// persist; for `Failed` and `NotReady` it returns `LeaveUnchanged` so the
/// next scan cycle can retry — matching Go's `log.Warn(...); continue` and
/// `return nil` early-exit behaviors.
pub fn apply_save_outcome(_request_id: i64, outcome: &SaveOutcome) -> RequestUpdate {
    match outcome {
        SaveOutcome::Saved {
            data_storage_id,
            storage_key,
            saved_at,
        } => RequestUpdate::MarkSaved {
            content_storage_id: *data_storage_id,
            content_storage_key: storage_key.clone(),
            content_saved_at: *saved_at,
        },
        SaveOutcome::Failed { .. } | SaveOutcome::NotReady => RequestUpdate::LeaveUnchanged,
    }
}

// ============================================================================
// RUST-P13-006 S08/S10 — scan preflight (Go scanAndSave L115-130)
//
// Mirrors the *precondition* checks at the top of Go's `Worker.scanAndSave`
// (worker.go L109-130). These run *before* the candidate query and decide
// whether the scan should proceed at all. They are the natural complement to
// `due_for_scan` (the *timing* decision) and `build_scan_plan` (the
// *selection* decision): together the three answer "should we scan now?",
// "may we scan at all?", and "which rows do we process?".
//
// Go paths mirrored:
//   - `if !settings.Enabled { return nil }` (L115-117) -> `ScanPreflight::Disabled`
//   - `if settings.DataStorageID == 0 { return err }` (L119-121)
//     -> `ScanPreflight::MissingDataStorageId`
//   - `ds.Primary || ds.Type == datastorage.TypeDatabase` (L128-129)
//     -> `ScanPreflight::InvalidDataStorage`; the data-storage row fetch itself
//       (L123-126) is the caller's responsibility (DB I/O), so we model only
//       the *properties* of the resolved storage.
// ============================================================================

/// Properties of the resolved data-storage row that the preflight needs to
/// validate. Mirrors the fields Go reads off `*ent.DataStorage` (worker.go
/// L128). The caller loads the row from the DB and hands us these flags; the
/// pure planner does not touch the DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataStorageProps {
    /// Mirrors Go's `ds.Primary` — whether this storage is the system primary.
    /// Go rejects primary storage for video saves (worker.go L128).
    #[serde(default)]
    pub is_primary: bool,
    /// Mirrors Go's `ds.Type == datastorage.TypeDatabase`. Go rejects database
    /// storage for video saves (worker.go L128-129).
    #[serde(default)]
    pub is_database: bool,
}

/// Outcome of `evaluate_scan_preflight`: should the scan cycle proceed?
///
/// Mirrors the early-return / error paths at the top of Go's `scanAndSave`
/// (worker.go L115-130). Each variant records *why* the decision was made so
/// the caller can log/metricate it the same way Go does (`return nil` silently
/// vs `return fmt.Errorf(...)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ScanPreflight {
    /// Scan may proceed. Go falls through to the candidate query (L132+).
    Proceed,
    /// `settings.enabled == false` — Go returns `nil` silently (L115-117).
    /// This is *not* an error: the worker is simply turned off.
    Disabled,
    /// `settings.data_storage_id == 0` while enabled — Go returns an error
    /// (L119-121): `"video storage enabled but data_storage_id is not set"`.
    MissingDataStorageId,
    /// The configured data storage is the primary storage or a database — Go
    /// returns an error (L128-129): `"video storage must be non-database
    /// storage"`. We distinguish the two sub-causes for observability; Go
    /// collapses them into one error message.
    InvalidDataStorage {
        #[serde(default)]
        is_primary: bool,
        #[serde(default)]
        is_database: bool,
    },
}

impl ScanPreflight {
    /// True when the preflight allows the scan to proceed.
    pub fn should_proceed(&self) -> bool {
        matches!(self, ScanPreflight::Proceed)
    }

    /// True when this decision represents a Go-style error (the worker should
    /// log `scanAndSave` failure). `Disabled` is *not* an error — Go returns
    /// `nil` silently when the feature is off.
    pub fn is_error(&self) -> bool {
        matches!(
            self,
            ScanPreflight::MissingDataStorageId | ScanPreflight::InvalidDataStorage { .. }
        )
    }

    /// Returns the error message Go would have produced, or `None` for
    /// `Proceed` / `Disabled`. Mirrors worker.go L120 and L129 verbatim so
    /// log/metric assertions can compare exact strings.
    pub fn go_error_message(&self) -> Option<&'static str> {
        match self {
            ScanPreflight::Proceed | ScanPreflight::Disabled => None,
            ScanPreflight::MissingDataStorageId => {
                Some("video storage enabled but data_storage_id is not set")
            }
            ScanPreflight::InvalidDataStorage { .. } => {
                Some("video storage must be non-database storage")
            }
        }
    }
}

/// Evaluates the scan-cycle preflight against the resolved settings.
///
/// This is the pure half of Go's `scanAndSave` prologue (worker.go L115-130).
/// The caller is responsible for loading the `DataStorage` row from the DB
/// (Go L123-126); once it has the row it constructs a `DataStorageProps` and
/// hands it here. The function then decides:
///
/// 1. If `settings.enabled == false` -> `ScanPreflight::Disabled` (Go returns
///    `nil`, L115-117).
/// 2. If `settings.data_storage_id == 0` -> `ScanPreflight::MissingDataStorageId`
///    (Go error, L119-121). Note we check this *before* inspecting
///    `storage_props` — Go fetches the row only after this guard, so a missing
///    id short-circuits before any DB call.
/// 3. If the resolved storage is primary or database-backed ->
///    `ScanPreflight::InvalidDataStorage { .. }` (Go error, L128-129).
/// 4. Otherwise -> `ScanPreflight::Proceed`.
///
/// `storage_props` is `None` when the caller has not yet resolved the storage
/// row (e.g. because an earlier guard already rejected the scan); the function
/// only inspects it when the earlier guards pass, mirroring Go's evaluation
/// order.
pub fn evaluate_scan_preflight(
    settings: &VideoStorageSettings,
    storage_props: Option<DataStorageProps>,
) -> ScanPreflight {
    // Go L115-117: `if !settings.Enabled { return nil }`.
    if !settings.enabled {
        return ScanPreflight::Disabled;
    }

    // Go L119-121: `if settings.DataStorageID == 0 { return err }`. Checked
    // *before* the row fetch — short-circuits without a DB call.
    if settings.data_storage_id == 0 {
        return ScanPreflight::MissingDataStorageId;
    }

    // Go L128-129: `if ds.Primary || ds.Type == datastorage.TypeDatabase`.
    // The row must have been resolved by the caller to reach this branch.
    if let Some(props) = storage_props
        && (props.is_primary || props.is_database)
    {
        return ScanPreflight::InvalidDataStorage {
            is_primary: props.is_primary,
            is_database: props.is_database,
        };
    }

    ScanPreflight::Proceed
}

/// Decision returned by `decide_delete_object`: should the worker also delete
/// the saved video object when a video task is deleted?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeleteObjectDecision {
    /// Delete the saved object from external storage. The caller MUST have the
    /// storage coordinates already loaded.
    DeleteObject,
    /// Do not touch external storage — the request has no saved content, or
    /// the provider task cannot be safely deleted.
    SkipObject,
}

/// Decides whether deleting a video task should also delete its saved content
/// object.
///
/// Mirrors Go's `VideoService.DeleteTask` (biz/video.go L95-115). The Go
/// implementation deletes the *provider task* via the outbound transformer
/// and then marks the local request as `StatusCanceled` — but it does **not**
/// delete the saved content object from external storage, and it does not
/// guard against missing `ExternalID`/`ChannelID` here (those are validated
/// earlier in `loadTask`, L117-163). So the Go-faithful decision is:
///
/// - If the request has saved content AND the provider task is deletable, we
///   *could* delete the object — but Go does not, so we return `SkipObject`.
/// - The current Go behavior is always `SkipObject`; `DeleteObject` is
///   retained in the enum for forward-compatibility with the Rust worker if a
///   later Go revision adds object cleanup.
///
/// Parameters:
/// - `request_has_saved_content`: whether the request row has
///   `content_saved=true` (and thus a storage object exists).
/// - `provider_task_deletable`: whether the provider's delete-task call
///   succeeded / is expected to succeed. Go ignores this for the object-
///   deletion decision; we accept it for symmetry with a future fix.
pub fn decide_delete_object(
    request_has_saved_content: bool,
    provider_task_deletable: bool,
) -> DeleteObjectDecision {
    // Go's DeleteTask (biz/video.go L95-115) never touches saved content:
    // it only cancels the request locally. So the faithful decision is always
    // SkipObject. The boolean parameters document *why* — a future revision
    // might decide to delete the object when both conditions hold.
    let _ = (request_has_saved_content, provider_task_deletable);
    DeleteObjectDecision::SkipObject
}

/// Builds the object-storage key for a saved video.
///
/// Mirrors Go's `GenerateVideoKey` (worker.go L224-231):
/// `/%d/requests/%d/video/%s` with `project_id`, `request_id`, and the
/// filename (defaulting to `"video.mp4"` when empty/whitespace). The Go
/// helper also strips the path down to `filepath.Base(name)`.
///
/// `filepath.Base` semantics mirrored here (verified against Go 1.26 stdlib
/// on Linux, the production target):
/// - `Base("foo.mp4")` = `"foo.mp4"`
/// - `Base("/tmp/clip.mp4")` = `"clip.mp4"`
/// - `Base("a/b/c")` = `"c"`
/// - `Base("clip/")` = `"clip"` (trailing separator stripped, returns the
///   preceding segment — *not* the empty string)
/// - `Base("/foo/")` = `"foo"`
/// - `Base("/")` = `"/"` on Linux, `\` on Windows — platform-dependent.
///   This is pathological for the worker (the upstream `filenameFromResponse`
///   already guards `base == "." || base == "/" || base == ""` and substitutes
///   `video-<unix>.mp4`), so we keep the safer `video.mp4` fallback rather
///   than emit a bare separator into the storage key.
///
/// `\` is normalized to `/` so Windows-sourced filenames behave the same as
/// on Linux (Go's `filepath.Base` is OS-aware; the worker runs on Linux in
/// production, so we follow Linux semantics unconditionally).
pub fn generate_video_key(project_id: i64, request_id: i64, filename: &str) -> String {
    let trimmed = filename.trim();
    let name = if trimmed.is_empty() {
        "video.mp4".to_string()
    } else {
        filepath_base_linux(trimmed)
    };
    format!("/{project_id}/requests/{request_id}/video/{name}")
}

/// Mirrors Go's `filepath.Base` for POSIX/Linux path semantics.
///
/// Go's `filepath.Base` (path/filepath.Base) returns the last element of
/// path. Trailing separators are removed before extracting the last element.
/// If the path is empty, Base returns `.`. If the path consists entirely of
/// separators, Base returns a single separator (`/` on Linux).
///
/// We deviate from Go in exactly two safe ways, both documented on
/// `generate_video_key`:
/// 1. The empty/all-separator case returns `"video.mp4"` (the worker's own
///    default) instead of `.` or `/` — never emit a bare separator into a
///    storage key.
/// 2. `\` is treated as a path separator unconditionally (matching Linux
///    Go), so Windows-sourced filenames don't leak backslashes into keys.
fn filepath_base_linux(path: &str) -> String {
    // Go: normalize backslashes to forward slashes (Linux filepath semantics).
    let normalized = path.replace('\\', "/");
    // Go: strip trailing separators. filepath.Base keeps stripping until the
    // last char is not a separator (or the string is empty/all-separators).
    let trimmed_end = normalized.trim_end_matches('/');
    if trimmed_end.is_empty() {
        // Path was all separators (or just "/"): Go returns "/" on Linux.
        // We return the worker default — never emit a bare separator.
        return "video.mp4".to_string();
    }
    // Go: take the last segment after the remaining final separator.
    match trimmed_end.rsplit('/').next() {
        Some(seg) if !seg.is_empty() => seg.to_string(),
        // `trimmed_end` is non-empty here, so this branch is unreachable; keep
        // the fallback for defensive symmetry with the empty-path guard.
        _ => "video.mp4".to_string(),
    }
}

// ============================================================================
// RUST-P13-006 S08 — scheduler tick plan (pure duration arithmetic)
//
// Mirrors the *intent* of Go's `scheduler.TaskSpec{FixRate: interval}` +
// `ScheduleFuncAtFixRate` (scheduler.go L173-174). The Go executor fires the
// task unconditionally every `FixRate` regardless of `lastRunAt`; `lastRunAt`
// is purely informational (exposed via `List()`, task.go L36). These helpers
// encode the same fixed-rate cadence as pure duration math so a future Rust
// scheduler (or a backoff-aware variant) can decide whether to fire now and
// when to fire next without re-deriving the arithmetic.
// ============================================================================

/// Decides whether a scan is due at `now`, given the last scan time and the
/// effective interval.
///
/// Go's `ScheduleFuncAtFixRate` fires unconditionally every interval, so in
/// the strict Go-faithful model this would always return `true` once the
/// interval has elapsed since `last`. We implement the natural fixed-rate
/// predicate (`now - last >= interval`) so the helper is useful to a Rust
/// scheduler that chooses to skip ticks when a previous run overran.
///
/// `last` is the timestamp of the most recent scan start (Go's `lastRunAt`,
/// set at the *start* of the wrapped closure — scheduler.go L148-151). The
/// comparison is inclusive: a scan is due exactly when one full interval has
/// elapsed.
pub fn due_for_scan(last: DateTime<Utc>, now: DateTime<Utc>, interval_minutes: i64) -> bool {
    let interval = chrono::Duration::minutes(interval_minutes.max(0));
    now - last >= interval
}

/// Returns the next wall-clock time at which a scan should fire, given the
/// last scan time and the effective interval.
///
/// This is pure fixed-rate scheduling: `last + interval`. Go's executor does
/// not drift-compensate (it does not add latency from the previous run), so
/// the next fire is always exactly one interval after the last — matching
/// `time.Ticker` / `ScheduleFuncAtFixRate` semantics.
pub fn next_scan_at(last: DateTime<Utc>, interval_minutes: i64) -> DateTime<Utc> {
    last + chrono::Duration::minutes(interval_minutes.max(0))
}

// ============================================================================
// RUST-P13-006 S11 — failure-isolation reducer
//
// Mirrors Go's `Worker.scanAndSave` per-candidate loop (worker.go L150-157):
//
//   for _, req := range reqs {
//       if err := w.processOne(ctx, ds, req); err != nil {
//           log.Warn(ctx, "Failed to save video request", ...)
//           continue   // <-- one failure MUST NOT abort the batch
//       }
//   }
//
// The Go loop swallows per-candidate errors (logs + continues), so a batch of
// N candidates yields N independent outcomes. `reduce_scan_outcomes` is the
// pure reducer that aggregates those outcomes into a summary without
// re-running any side effect.
// ============================================================================

/// Aggregated result of a scan cycle, produced by `reduce_scan_outcomes`.
///
/// Mirrors the bookkeeping the Go worker *would* expose if it returned a
/// summary from `scanAndSave` (it currently returns only `error`, logging
/// per-candidate failures at warn level — worker.go L152-154).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSummary {
    /// Number of candidates whose content was saved successfully.
    pub succeeded: usize,
    /// Number of candidates whose save failed. The caller should leave the
    /// request unchanged so the next scan cycle retries (Go: `continue`).
    pub failed: usize,
    /// Number of candidates skipped because they had no downloadable video
    /// URL yet (`SaveOutcome::NotReady`). Go: early `return nil` paths
    /// (worker.go L173-178, L180-182).
    pub skipped: usize,
}

impl ScanSummary {
    /// Total candidates processed (succeeded + failed + skipped).
    pub fn total(&self) -> usize {
        self.succeeded + self.failed + self.skipped
    }
}

/// Aggregates per-candidate save outcomes into a `ScanSummary`.
///
/// This is the pure reducer over Go's `scanAndSave` loop (worker.go L150-157).
/// Each `SaveOutcome` maps to exactly one counter:
/// - `SaveOutcome::Saved { .. }` -> `succeeded`
/// - `SaveOutcome::Failed { .. }` -> `failed`
/// - `SaveOutcome::NotReady` -> `skipped`
///
/// The reducer never short-circuits on failure — matching Go's `continue`
/// semantics, one error does not abort the batch. The caller is expected to
/// have already produced one `SaveOutcome` per candidate (the worker's
/// `processOne` call); this function only tallies them.
pub fn reduce_scan_outcomes<I>(outcomes: I) -> ScanSummary
where
    I: IntoIterator,
    I::Item: AsRef<SaveOutcome>,
{
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    for outcome in outcomes {
        match outcome.as_ref() {
            SaveOutcome::Saved { .. } => succeeded += 1,
            SaveOutcome::Failed { .. } => failed += 1,
            SaveOutcome::NotReady => skipped += 1,
        }
    }
    ScanSummary {
        succeeded,
        failed,
        skipped,
    }
}

impl AsRef<SaveOutcome> for SaveOutcome {
    fn as_ref(&self) -> &SaveOutcome {
        self
    }
}

// ============================================================================
// RUST-P13-006 A01 — worker.go pure helpers (extract URL / filename / scheme /
// status+format filters / max-bytes cap).
//
// Go has no `*_test.go` for `internal/server/video_storage/worker.go`; these
// helpers + tests pin its side-effect-free decision logic with inline line
// references to `worker.go` (the production Go source
// `conduit/internal/server/video_storage/worker.go`, 298 lines).
// ============================================================================

/// Hard cap on a single video download. Mirrors Go's `const maxBytes =
/// 512 * 1024 * 1024` (worker.go L193). The Go worker wraps the response body
/// in `io.LimitReader(resp, maxBytes)` so any bytes beyond this are silently
/// truncated; the Rust port should apply the same ceiling.
pub const MAX_VIDEO_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// Extracts a downloadable video URL from a cached provider response body, if
/// any. Mirrors Go's `extractVideoURLFromResponseBody` (worker.go L233-244):
///
/// - Empty body -> `None` (Go returns `("", nil)` — worker.go L234-236).
/// - Valid JSON with `"video_url"` -> `Some(url)` (worker.go L242-243).
/// - Valid JSON without the field / empty string -> `None` (Go returns the
///   zero-value `""`, which the caller treats as "no URL" — worker.go L167).
/// - Invalid JSON -> `None`. Go propagates the `json.Unmarshal` error
///   (worker.go L239-241), but the only caller (`processOne`, L163) discards
///   it via `if v, err := ...; err == nil && ...` — so the observable
///   behavior is "no cached URL, fall back to GetTask".
pub fn extract_video_url_from_response_body(raw: &[u8]) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    // Go errors are discarded by the caller; we collapse them to None so the
    // pure helper has no error surface.
    let value: Value = serde_json::from_slice(raw).ok()?;
    let url = value.get("video_url")?.as_str()?;
    let trimmed = url.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Decides whether a downloaded video URL's scheme is acceptable. Mirrors
/// Go's `openVideoStream` scheme guard (worker.go L252-253): only `http` and
/// `https` are allowed; anything else (including `file://`, `ftp://`, or a
/// malformed URL) is rejected.
///
/// Go parses with `url.Parse` first and reports a parse error (worker.go
/// L247-250); we mirror that by returning `false` on parse failure too.
pub fn is_valid_video_download_url(url: &str) -> bool {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    matches!(parsed.scheme(), "http" | "https")
}

/// Parses the filename for a downloaded video, mirroring Go's
/// `filenameFromResponse` (worker.go L276-298):
///
/// 1. If `Content-Disposition` is present and contains `filename=`, use the
///    trimmed + unquoted value (worker.go L278-286).
/// 2. Otherwise, take `filepath.Base` of the URL with any query fragment
///    stripped (worker.go L289-293).
/// 3. If that yields `.` / `/` / empty, fall back to
///    `video-<unix_seconds>.mp4` (worker.go L294-296).
///
/// `now_unix` is accepted as a parameter so the helper stays deterministic
/// under test (Go uses `time.Now().Unix()` — worker.go L295).
pub fn filename_from_response(
    content_disposition: Option<&str>,
    fallback_url: &str,
    now_unix: i64,
) -> String {
    // Go: Content-Disposition parsing (worker.go L278-286).
    if let Some(cd) = content_disposition
        && let Some((_, after)) = cd.split_once("filename=")
    {
        let trimmed = after.trim().trim_matches('"');
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Go: strip the query fragment before taking filepath.Base (worker.go
    // L289-292).
    let before_query = fallback_url.split('?').next().unwrap_or(fallback_url);
    let base = filepath_base_linux(before_query);

    // Go: `base == "." || base == "/" || base == ""` (worker.go L294). Our
    // `filepath_base_linux` never returns `.` (it falls back to `video.mp4`),
    // so we only need to check for the separator and the worker-default
    // empty/video.mp4 cases the worker would also substitute.
    if base == "/" || base == "." || base.is_empty() || base == "video.mp4" {
        // The worker's own fallback for pathological bases (worker.go L295).
        return format!("video-{now_unix}.mp4");
    }
    base
}

/// Mirrors Go's scan candidate `StatusIn` filter (worker.go L139). Go admits
/// `StatusProcessing` and `StatusCompleted` (the two non-terminal "in-flight"
/// statuses a video task can be in once external_id has been written). The
/// Rust enum mapping (per `map_video_status_to_request_status`):
/// `StatusProcessing` -> `Running`, `StatusCompleted` -> `Succeeded`.
pub fn passes_scan_status_filter(status: RequestStatus) -> bool {
    matches!(status, RequestStatus::Running | RequestStatus::Succeeded)
}

/// Mirrors Go's scan candidate `FormatIn` filter (worker.go L140). Go admits
/// the two video API formats `llm.APIFormatOpenAIVideo` ("openai/video") and
/// `llm.APIFormatSeedanceVideo` ("seedance/video") — request rows carrying
/// any other format are not video tasks and are skipped by the scan.
pub fn passes_scan_format_filter(api_format: &str) -> bool {
    matches!(api_format, "openai/video" | "seedance/video")
}

// ============================================================================
// RUST-P7-006 S08 + S12 — video task external-id persistence & delete flow
//
// Mirrors `conduit/internal/server/biz/video.go` (`VideoService`). Go models a
// video task as a *request row*: the provider-side task id lives in
// `requests.external_id` (ent/schema/request.go:88-91) and is first written by
// the persist middleware right after the provider's create response
// (orchestrator/request.go:110-126 — status stays "processing", external_id =
// llmResp.ID). `GetTask` (video.go:27-57) polls the provider and re-persists
// the status/external-id/response-body snapshot best-effort; `DeleteTask`
// (video.go:95-115) deletes the provider-side task FIRST and only then
// best-effort-cancels the local row. Lookups by provider task id go through
// `RequestService::get_request_by_external_id` (ent `.Only`, video.go:59-93).
// ============================================================================

/// Loaded view of a video-task request row — exactly the fields Go's
/// `loadTask` reads off `*ent.Request` (`biz/video.go:123-134`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoTaskRow {
    /// Local request id (Go `task.ID`).
    pub request_id: String,
    /// Owning project. Go's ent lookups are global, but the Rust repo surface
    /// is project-scoped for writes, so the row carries its project along.
    pub project_id: String,
    /// Provider task id (`requests.external_id`, Go `task.ExternalID`). Go
    /// zero value is the empty string.
    #[serde(default)]
    pub external_id: String,
    /// Serving channel (`requests.channel_id`, Go `task.ChannelID`). Go zero
    /// value for the unset optional int is `0`.
    #[serde(default)]
    pub channel_id: i64,
}

impl VideoTaskRow {
    /// Projects a persisted [`RequestRecord`] into the loadTask view.
    /// `external_id`/`channel_id` come from the record's extra-column face
    /// (see `RequestRecord::external_id` / `RequestRecord::channel_id`),
    /// defaulting to Go's zero values (`""` / `0`) when never set.
    pub fn from_record(record: &RequestRecord) -> Self {
        Self {
            request_id: record.id.clone(),
            project_id: record.project_id.clone(),
            external_id: record.external_id().unwrap_or_default().to_string(),
            channel_id: record.channel_id(),
        }
    }

    /// The `loadTask` row guards, in Go's order (`biz/video.go:128-134`):
    /// 1. `strings.TrimSpace(task.ExternalID) == ""` -> missing external_id;
    /// 2. `task.ChannelID == 0` -> missing channel_id.
    ///
    /// The third loadTask guard — "channel does not support video task
    /// operations" (video.go:145-160) — needs the channel row and therefore
    /// lives behind [`VideoTaskGateway`] (implementors return
    /// [`VideoServiceError::UnsupportedChannel`]).
    pub fn validate(&self) -> VideoServiceResult<()> {
        if self.external_id.trim().is_empty() {
            return Err(VideoServiceError::MissingExternalId);
        }
        if self.channel_id == 0 {
            return Err(VideoServiceError::MissingChannelId);
        }
        Ok(())
    }
}

/// Maps the provider's unified video status onto the local request status.
/// Mirrors `mapVideoStatusToRequestStatus` (`biz/video.go:165-176`) verbatim:
/// trim + case-insensitive compare; `succeeded` -> completed, `failed` ->
/// failed, `queued`/`running` -> processing, anything else -> processing.
///
/// Status-name mapping between the two enums (established by
/// `request_service.rs` S15 docs): Go `StatusCompleted` -> `Succeeded`,
/// `StatusProcessing` -> `Running`, `StatusFailed` -> `Failed`.
pub fn map_video_status_to_request_status(status: &str) -> RequestStatus {
    match status.trim().to_lowercase().as_str() {
        // Go video.go:167-168: "succeeded" -> request.StatusCompleted.
        "succeeded" => RequestStatus::Succeeded,
        // Go video.go:169-170: "failed" -> request.StatusFailed.
        "failed" => RequestStatus::Failed,
        // Go video.go:171-175: "queued"/"running" and the default arm both
        // land on request.StatusProcessing.
        "queued" | "running" => RequestStatus::Running,
        _ => RequestStatus::Running,
    }
}

/// Resolves which video API format key a channel's outbound is looked up
/// under. Mirrors the switch in `loadTask` (`biz/video.go:145-150`): only
/// `channel.TypeDoubao` ("doubao", ent/channel/channel.go:216) selects the
/// Seedance format; every other type — including "doubao_anthropic" — falls
/// through to the OpenAI video format. Key strings are
/// `llm.APIFormatSeedanceVideo` / `llm.APIFormatOpenAIVideo`
/// (`llm/constants.go:53` / `:37`).
pub fn video_api_format_for_channel_type(channel_type: &str) -> &'static str {
    if channel_type == "doubao" {
        "seedance/video"
    } else {
        "openai/video"
    }
}

/// Provider-side snapshot returned by the GetTask round-trip. Carries the two
/// pieces Go consumes from the parsed `*llm.Response` (`biz/video.go:43-56`):
/// the unified status (for `mapVideoStatusToRequestStatus`) and the
/// `Response.Video` payload (persisted verbatim as the response-body
/// snapshot — video.go:52 passes `video.Video`, i.e. the `llm.VideoResponse`,
/// to `UpdateRequestStatusExternalIDAndResponseBody`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderVideoTask {
    /// Unified status: "queued" | "running" | "succeeded" | "failed"
    /// (`llm/video.go:74-75`).
    pub status: String,
    /// The `llm.VideoResponse` JSON payload (`llm/video.go:71-101`).
    pub video: Value,
}

/// Provider-side video task operations, injected into [`VideoTaskService`].
/// Bundles what Go composes inline in `biz/video.go`: channel fetch
/// (`ChannelService.GetChannel`, video.go:136-139), outbound resolution by
/// video API format (video.go:141-160, see
/// [`video_api_format_for_channel_type`]), request building
/// (`BuildGetVideoTaskRequest` / `BuildDeleteVideoTaskRequest`,
/// `llm/transformer/interfaces.go:66-71`) and the HTTP round-trip
/// (`ch.HTTPClient.Do`).
#[async_trait]
pub trait VideoTaskGateway: Send + Sync {
    /// Provider GetTask: build + do + parse (`biz/video.go:33-46`).
    /// Implementors return [`VideoServiceError::UnsupportedChannel`] when the
    /// channel has no video outbound (video.go:158-160) and
    /// [`VideoServiceError::Provider`] for transport/parse failures.
    async fn get_video_task(
        &self,
        ctx: &RequestContext,
        channel_id: i64,
        external_id: &str,
    ) -> VideoServiceResult<ProviderVideoTask>;

    /// Provider DeleteTask: build + do, response body discarded
    /// (`biz/video.go:101-109` — `_, err = ch.HTTPClient.Do(ctx, httpReq)`).
    async fn delete_video_task(
        &self,
        ctx: &RequestContext,
        channel_id: i64,
        external_id: &str,
    ) -> VideoServiceResult<()>;
}

/// Ordered delete plan for a video task (S12). Encodes the Go `DeleteTask`
/// sequence (`biz/video.go:95-115`):
/// 1. delete the task at the provider (fatal on failure — the local row is
///    left untouched, video.go:101-109);
/// 2. only then mark the local request canceled, best-effort (errors ignored,
///    video.go:111-112 `_ = s.RequestService.UpdateRequestStatus(...)`).
///
/// The local row is *never hard-deleted* — Go keeps the request and flips its
/// status to `canceled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteTaskPlan {
    /// Project owning the local request row (for the Rust write surface).
    pub project_id: String,
    /// Local request row to mark canceled in step 2.
    pub request_id: String,
    /// Channel to route the provider delete through in step 1.
    pub channel_id: i64,
    /// Provider task id passed to `BuildDeleteVideoTaskRequest`
    /// (video.go:101 uses `task.ExternalID` untrimmed).
    pub external_id: String,
}

/// Validates the loaded row with the `loadTask` guards and produces the
/// ordered [`DeleteTaskPlan`]. Pure — no I/O; this is the decision half of
/// `VideoService.DeleteTask` (`biz/video.go:95-115` + guards at 128-134).
pub fn plan_delete_task(task: &VideoTaskRow) -> VideoServiceResult<DeleteTaskPlan> {
    task.validate()?;
    Ok(DeleteTaskPlan {
        project_id: task.project_id.clone(),
        request_id: task.request_id.clone(),
        channel_id: task.channel_id,
        external_id: task.external_id.clone(),
    })
}

/// Rust counterpart of Go `biz.VideoService` (`biz/video.go:15-25`,
/// `VideoService{ChannelService, RequestService}`) for the request-row-backed
/// task flows. Channel + outbound + HTTP concerns are injected as
/// [`VideoTaskGateway`]; local persistence goes through [`RequestService`].
/// (The sibling [`VideoService`] in this module is the P13-006 task-model
/// skeleton and is unrelated to these Go-parity flows.)
pub struct VideoTaskService {
    gateway: Arc<dyn VideoTaskGateway>,
    requests: Arc<RequestService>,
}

impl VideoTaskService {
    pub fn new(gateway: Arc<dyn VideoTaskGateway>, requests: Arc<RequestService>) -> Self {
        Self { gateway, requests }
    }

    /// Mirrors `VideoService.loadTask` (`biz/video.go:117-134`): fetch the
    /// request row (ent `.Get`, NotFound propagates) and run the row guards.
    /// The channel/outbound guard (video.go:136-160) fires inside the gateway.
    async fn load_task(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> VideoServiceResult<VideoTaskRow> {
        let record = self
            .requests
            .get_request(ctx, project_id, request_id)
            .await?;
        let row = VideoTaskRow::from_record(&record);
        row.validate()?;
        Ok(row)
    }

    /// Mirrors `VideoService.GetTask` (`biz/video.go:27-57`).
    ///
    /// Sequence: loadTask -> provider GetTask round-trip -> map the provider
    /// status -> persist the snapshot (status + external_id + `video` payload,
    /// metrics nil) via `UpdateRequestStatusExternalIDAndResponseBody` —
    /// **best-effort**: Go swallows the persistence error and returns the
    /// provider data anyway ("non-fatal: return data anyway", video.go:51-54).
    pub async fn get_task(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> VideoServiceResult<ProviderVideoTask> {
        let row = self.load_task(ctx, project_id, request_id).await?;

        let provider = self
            .gateway
            .get_video_task(ctx, row.channel_id, &row.external_id)
            .await?;

        // Persist snapshot to the request row for task tracking
        // (video.go:48-52). Go passes `task.ExternalID` (the row's stored id,
        // not the provider response id) and nil metrics.
        let status = map_video_status_to_request_status(&provider.status);
        let _ = self
            .requests
            .update_request_status_external_id_and_response_body(
                ctx,
                project_id,
                request_id,
                status,
                &row.external_id,
                None,
                Some(provider.video.clone()),
            )
            .await;

        Ok(provider)
    }

    /// Mirrors `VideoService.GetTaskByExternalID` (`biz/video.go:59-75`):
    /// resolve the unique row via ent `.Only` semantics, then delegate to
    /// [`Self::get_task`] with the found row's id (video.go:74).
    pub async fn get_task_by_external_id(
        &self,
        ctx: &RequestContext,
        external_id: &str,
    ) -> VideoServiceResult<ProviderVideoTask> {
        let record = self
            .requests
            .get_request_by_external_id(ctx, external_id)
            .await?;
        self.get_task(ctx, &record.project_id, &record.id).await
    }

    /// Mirrors `VideoService.DeleteTask` (`biz/video.go:95-115`).
    ///
    /// Order and fault-tolerance, verbatim from Go:
    /// 1. loadTask guards (row missing / external_id empty / channel_id zero
    ///    abort before any side effect);
    /// 2. provider delete FIRST — on failure the error is returned and the
    ///    local row is left untouched (video.go:106-109);
    /// 3. only after provider success: mark the local request canceled,
    ///    **best-effort** — a local failure is ignored and the call still
    ///    succeeds (video.go:111-112 `_ = ...UpdateRequestStatus(...,
    ///    request.StatusCanceled)`). The row is never hard-deleted.
    pub async fn delete_task(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> VideoServiceResult<()> {
        let row = self.load_task(ctx, project_id, request_id).await?;
        let plan = plan_delete_task(&row)?;

        self.gateway
            .delete_video_task(ctx, plan.channel_id, &plan.external_id)
            .await?;

        // Best effort: mark canceled locally (video.go:111-112).
        let _ = self
            .requests
            .update_request_status(
                ctx,
                &plan.project_id,
                &plan.request_id,
                RequestStatus::Cancelled,
            )
            .await;

        Ok(())
    }

    /// Mirrors `VideoService.DeleteTaskByExternalID` (`biz/video.go:77-93`):
    /// resolve the unique row via ent `.Only` semantics (lookup errors
    /// propagate, video.go:88-90), then delegate to [`Self::delete_task`].
    pub async fn delete_task_by_external_id(
        &self,
        ctx: &RequestContext,
        external_id: &str,
    ) -> VideoServiceResult<()> {
        let record = self
            .requests
            .get_request_by_external_id(ctx, external_id)
            .await?;
        self.delete_task(ctx, &record.project_id, &record.id).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use conduit_db::{PolicyContext, Principal, RequestContext};
    use serde_json::json;

    use super::*;
    use crate::request_service::{
        ExecutionCompletionPatch, ExecutionRecord, ExecutionStatusPatch,
        InMemoryRequestPersistenceRepo, RequestCompletionPatch, RequestPersistenceRepo,
        RequestServiceResult,
    };

    #[derive(Debug, Default)]
    struct FakeVideoTaskRepo {
        tasks: Mutex<BTreeMap<(String, String), VideoTask>>,
        external_index: Mutex<BTreeMap<(String, String), String>>,
    }

    impl FakeVideoTaskRepo {
        fn task_count(&self) -> VideoServiceResult<usize> {
            Ok(self.lock_tasks()?.len())
        }

        fn lock_tasks(
            &self,
        ) -> VideoServiceResult<std::sync::MutexGuard<'_, BTreeMap<(String, String), VideoTask>>>
        {
            self.tasks
                .lock()
                .map_err(|_| VideoServiceError::LockPoisoned)
        }

        fn lock_external_index(
            &self,
        ) -> VideoServiceResult<std::sync::MutexGuard<'_, BTreeMap<(String, String), String>>>
        {
            self.external_index
                .lock()
                .map_err(|_| VideoServiceError::LockPoisoned)
        }
    }

    #[async_trait]
    impl VideoTaskRepo for FakeVideoTaskRepo {
        async fn upsert_task(
            &self,
            _ctx: &RequestContext,
            task: VideoTask,
        ) -> VideoServiceResult<VideoTask> {
            let mut tasks = self.lock_tasks()?;
            let mut external_index = self.lock_external_index()?;
            let key = (task.project_id.clone(), task.id.clone());
            let external_key = (task.project_id.clone(), task.external_task_id.clone());

            tasks.insert(key, task.clone());
            external_index.insert(external_key, task.id.clone());
            Ok(task)
        }

        async fn get_task(
            &self,
            _ctx: &RequestContext,
            project_id: &str,
            task_id: &str,
        ) -> VideoServiceResult<Option<VideoTask>> {
            Ok(self
                .lock_tasks()?
                .get(&(project_id.to_string(), task_id.to_string()))
                .cloned())
        }

        async fn get_task_by_external_id(
            &self,
            _ctx: &RequestContext,
            project_id: &str,
            external_task_id: &str,
        ) -> VideoServiceResult<Option<VideoTask>> {
            let external_key = (project_id.to_string(), external_task_id.to_string());
            let Some(task_id) = self.lock_external_index()?.get(&external_key).cloned() else {
                return Ok(None);
            };

            self.get_task(_ctx, project_id, &task_id).await
        }

        async fn delete_task(
            &self,
            _ctx: &RequestContext,
            project_id: &str,
            task_id: &str,
        ) -> VideoServiceResult<Option<VideoTask>> {
            let removed = self
                .lock_tasks()?
                .remove(&(project_id.to_string(), task_id.to_string()));
            if let Some(task) = &removed {
                self.lock_external_index()?
                    .remove(&(project_id.to_string(), task.external_task_id.clone()));
            }
            Ok(removed)
        }
    }

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    #[tokio::test]
    async fn maps_external_task_id_to_task() -> VideoServiceResult<()> {
        let repo = Arc::new(FakeVideoTaskRepo::default());
        let service = VideoService::new(repo.clone());
        let ctx = ctx();

        let task = service
            .map_external_task(
                &ctx,
                "project-a",
                "provider-task-1",
                Some("request-1".to_string()),
            )
            .await?;
        let queried = service
            .query_task_by_external_id(&ctx, "project-a", "provider-task-1")
            .await?;

        assert_eq!(queried, Some(task.clone()));
        assert_eq!(task.request_id.as_deref(), Some("request-1"));
        assert_eq!(task.status, VideoTaskStatus::Pending);
        assert_eq!(repo.task_count()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn external_task_ids_are_project_scoped() -> VideoServiceResult<()> {
        let repo = Arc::new(FakeVideoTaskRepo::default());
        let service = VideoService::new(repo.clone());
        let ctx = ctx();

        let project_a = service
            .map_external_task(&ctx, "project-a", "provider-task-1", None)
            .await?;
        let project_b = service
            .map_external_task(&ctx, "project-b", "provider-task-1", None)
            .await?;

        assert_ne!(project_a.id, project_b.id);
        assert_eq!(project_a.external_task_id, project_b.external_task_id);
        assert_eq!(repo.task_count()?, 2);
        Ok(())
    }

    #[tokio::test]
    async fn get_and_delete_task_by_internal_id() -> VideoServiceResult<()> {
        let repo = Arc::new(FakeVideoTaskRepo::default());
        let service = VideoService::new(repo);
        let ctx = ctx();
        let task = service
            .map_external_task(&ctx, "project-a", "provider-task-1", None)
            .await?;

        let fetched = service.get_task(&ctx, "project-a", &task.id).await?;
        let deleted = service.delete_task(&ctx, "project-a", &task.id).await?;
        let after_delete = service
            .query_task_by_external_id(&ctx, "project-a", "provider-task-1")
            .await?;

        assert_eq!(fetched, task);
        assert_eq!(deleted, task);
        assert_eq!(after_delete, None);
        Ok(())
    }

    #[tokio::test]
    async fn saved_content_metadata_is_preserved() -> VideoServiceResult<()> {
        let repo = Arc::new(FakeVideoTaskRepo::default());
        let service = VideoService::new(repo);
        let ctx = ctx();
        let mut content =
            VideoContentMetadata::new("videos/project-a/provider-task-1.mp4", Utc::now());
        content.storage_id = Some("storage-1".to_string());
        content.content_type = Some("video/mp4".to_string());
        content.bytes = Some(42);
        content
            .metadata
            .insert("checksum".to_string(), json!("sha256:abc"));

        let task = service
            .map_external_task(
                &ctx,
                "project-a",
                "provider-task-1",
                Some("request-1".to_string()),
            )
            .await?
            .with_status(
                VideoTaskStatus::Completed,
                Some("provider_succeeded".to_string()),
            )
            .with_saved_content(content.clone());
        let saved = service.save_task(&ctx, task.clone()).await?;
        let fetched = service
            .query_task_by_external_id(&ctx, "project-a", "provider-task-1")
            .await?
            .ok_or_else(|| VideoServiceError::TaskNotFound("task should exist".to_string()))?;

        assert_eq!(saved, task);
        assert!(fetched.content_saved);
        assert_eq!(fetched.content, Some(content));
        assert_eq!(
            fetched.provider_status.as_deref(),
            Some("provider_succeeded")
        );
        Ok(())
    }

    // ========================================================================
    // RUST-P13-006 S07/S10/S11/S12 — scan plan / save outcome / delete decision
    // Mirrors Go `video_storage/worker.go` golden intent. There is no
    // `*_test.go` for worker.go; these tests pin the worker's pure-logic
    // contract as inferred from the production code paths cited inline.
    // ========================================================================

    fn candidate(request_id: i64, project_id: i64) -> VideoScanCandidate {
        VideoScanCandidate {
            request_id,
            project_id,
            content_saved: false,
            has_cached_video_url: false,
        }
    }

    #[test]
    fn build_scan_plan_orders_by_request_id_ascending() -> VideoServiceResult<()> {
        // Go: Order(ent.Asc(request.FieldID)) (worker.go L143).
        let settings = VideoStorageSettings {
            scan_limit: 10,
            ..VideoStorageSettings::default()
        };
        let candidates = vec![candidate(30, 1), candidate(10, 1), candidate(20, 1)];
        let plan = build_scan_plan(candidates, &settings, Utc::now());

        assert_eq!(plan.selected_request_ids, vec![10, 20, 30]);
        assert_eq!(plan.deferred_count, 0);
        assert_eq!(plan.effective_limit, 10);
        assert_eq!(plan.effective_interval_minutes, 1);
        Ok(())
    }

    #[test]
    fn build_scan_plan_applies_default_limit_of_50() -> VideoServiceResult<()> {
        // Go: `if limit <= 0 { limit = 50 }` (worker.go L132-135).
        let settings = VideoStorageSettings {
            scan_limit: 0, // non-positive → default
            ..VideoStorageSettings::default()
        };
        // 60 candidates, default limit 50 → only first 50 selected.
        let candidates: Vec<VideoScanCandidate> = (1..=60).map(|i| candidate(i, 1)).collect();
        let plan = build_scan_plan(candidates, &settings, Utc::now());

        assert_eq!(plan.effective_limit, 50);
        assert_eq!(plan.selected_request_ids.len(), 50);
        assert_eq!(*plan.selected_request_ids.first().unwrap_or(&0), 1);
        assert_eq!(*plan.selected_request_ids.last().unwrap_or(&0), 50);
        assert_eq!(plan.deferred_count, 10);
        Ok(())
    }

    #[test]
    fn build_scan_plan_applies_default_interval_of_one_minute() -> VideoServiceResult<()> {
        // Go: `if intervalMinutes <= 0 { intervalMinutes = 1 }` (worker.go L61-64).
        let settings = VideoStorageSettings {
            scan_interval_minutes: -5,
            ..VideoStorageSettings::default()
        };
        let plan = build_scan_plan(vec![], &settings, Utc::now());
        assert_eq!(plan.effective_interval_minutes, 1);
        Ok(())
    }

    #[test]
    fn build_scan_plan_drops_already_saved_candidates() -> VideoServiceResult<()> {
        // Defensive: Go's query filters ContentSaved(false) (worker.go L141);
        // the planner re-checks so a stale feed cannot re-process saved rows.
        let settings = VideoStorageSettings::default();
        let mut saved = candidate(5, 1);
        saved.content_saved = true;
        let candidates = vec![candidate(1, 1), saved, candidate(9, 1)];
        let plan = build_scan_plan(candidates, &settings, Utc::now());

        assert_eq!(plan.selected_request_ids, vec![1, 9]);
        assert_eq!(plan.deferred_count, 0);
        Ok(())
    }

    #[test]
    fn build_scan_plan_respects_explicit_limit_smaller_than_pool() -> VideoServiceResult<()> {
        // Go: `.Limit(limit)` (worker.go L144) — explicit small limit truncates.
        let settings = VideoStorageSettings {
            scan_limit: 3,
            ..VideoStorageSettings::default()
        };
        let candidates: Vec<VideoScanCandidate> = (1..=10).map(|i| candidate(i, 1)).collect();
        let plan = build_scan_plan(candidates, &settings, Utc::now());

        assert_eq!(plan.effective_limit, 3);
        assert_eq!(plan.selected_request_ids, vec![1, 2, 3]);
        assert_eq!(plan.deferred_count, 7);
        Ok(())
    }

    #[test]
    fn build_scan_plan_handles_empty_candidate_pool() -> VideoServiceResult<()> {
        let plan = build_scan_plan(vec![], &VideoStorageSettings::default(), Utc::now());
        assert!(plan.selected_request_ids.is_empty());
        assert_eq!(plan.deferred_count, 0);
        Ok(())
    }

    #[test]
    fn apply_save_outcome_marks_saved_on_success() -> VideoServiceResult<()> {
        // Go: SetContentSaved(true).SetContentStorageID(ds.ID)
        //     .SetContentStorageKey(storageKey).SetContentSavedAt(now)
        //     (worker.go L204-209).
        let saved_at = Utc::now();
        let outcome = SaveOutcome::Saved {
            data_storage_id: 42,
            storage_key: "/7/requests/99/video/foo.mp4".to_string(),
            saved_at,
        };
        let update = apply_save_outcome(99, &outcome);

        match update {
            RequestUpdate::MarkSaved {
                content_storage_id,
                content_storage_key,
                content_saved_at,
            } => {
                assert_eq!(content_storage_id, 42);
                assert_eq!(content_storage_key, "/7/requests/99/video/foo.mp4");
                assert_eq!(content_saved_at, saved_at);
            }
            RequestUpdate::LeaveUnchanged => {
                return Err(VideoServiceError::TaskNotFound(
                    "expected MarkSaved, got LeaveUnchanged".to_string(),
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn apply_save_outcome_leaves_request_unchanged_on_failure() -> VideoServiceResult<()> {
        // Go: `log.Warn(...); continue` (worker.go L151-155) — error does not
        // break the request; the next cycle can retry.
        let outcome = SaveOutcome::Failed {
            error: "HTTP 502".to_string(),
        };
        assert_eq!(
            apply_save_outcome(7, &outcome),
            RequestUpdate::LeaveUnchanged
        );
        Ok(())
    }

    #[test]
    fn apply_save_outcome_leaves_request_unchanged_when_not_ready() -> VideoServiceResult<()> {
        // Go: provider task still queued/running → early `return nil`
        //     (worker.go L173-178, L180-182).
        assert_eq!(
            apply_save_outcome(7, &SaveOutcome::NotReady),
            RequestUpdate::LeaveUnchanged
        );
        Ok(())
    }

    #[test]
    fn decide_delete_object_skips_by_default_matching_go() -> VideoServiceResult<()> {
        // Go's DeleteTask (biz/video.go L95-115) only cancels the local
        // request; it never touches the saved object in external storage.
        for (saved, deletable) in [(false, false), (false, true), (true, false), (true, true)] {
            assert_eq!(
                decide_delete_object(saved, deletable),
                DeleteObjectDecision::SkipObject,
                "saved={saved}, deletable={deletable}"
            );
        }
        Ok(())
    }

    #[test]
    fn generate_video_key_uses_default_filename_when_empty() -> VideoServiceResult<()> {
        // Go: `if name == "" { name = "video.mp4" }` (worker.go L225-227).
        assert_eq!(
            generate_video_key(7, 99, ""),
            "/7/requests/99/video/video.mp4"
        );
        assert_eq!(
            generate_video_key(7, 99, "   "),
            "/7/requests/99/video/video.mp4"
        );
        Ok(())
    }

    #[test]
    fn generate_video_key_strips_to_basename() -> VideoServiceResult<()> {
        // Go: `name = filepath.Base(name)` (worker.go L228).
        assert_eq!(
            generate_video_key(7, 99, "clip.mp4"),
            "/7/requests/99/video/clip.mp4"
        );
        assert_eq!(
            generate_video_key(7, 99, "/tmp/cache/clip.mp4"),
            "/7/requests/99/video/clip.mp4"
        );
        assert_eq!(
            generate_video_key(7, 99, "C:\\videos\\clip.mp4"),
            "/7/requests/99/video/clip.mp4"
        );
        Ok(())
    }

    #[test]
    fn video_storage_settings_defaults_match_go() -> VideoServiceResult<()> {
        // Go's effective-limit/interval defaults: 50 and 1 minute.
        let s = VideoStorageSettings::default();
        assert_eq!(s.effective_scan_limit(), 50);
        assert_eq!(s.effective_scan_interval_minutes(), 1);
        assert!(!s.enabled);
        assert_eq!(s.data_storage_id, 0);
        Ok(())
    }

    /// Wire-format parity guard: Go `biz.VideoStorageSettings` json tags are
    /// snake_case (`data_storage_id`/`scan_interval_minutes`/`scan_limit`,
    /// system.go:131-140). The Rust struct must serialize byte-identically —
    /// no `rename_all = "camelCase"` (which would emit `dataStorageId` etc.
    /// and break the frontend/persistence contract).
    #[test]
    fn video_storage_settings_serializes_snake_case_like_go() -> Result<(), serde_json::Error> {
        let s = VideoStorageSettings {
            enabled: true,
            data_storage_id: 7,
            scan_interval_minutes: 5,
            scan_limit: 50,
        };
        let json = serde_json::to_value(&s)?;
        assert_eq!(json["data_storage_id"], 7);
        assert_eq!(json["scan_interval_minutes"], 5);
        assert_eq!(json["scan_limit"], 50);
        // camelCase keys must NOT appear.
        assert!(json.get("dataStorageId").is_none());
        assert!(json.get("scanIntervalMinutes").is_none());
        assert!(json.get("scanLimit").is_none());
        Ok(())
    }

    // ========================================================================
    // RUST-P13-006 S07 edge cases — filepath.Base trailing-slash semantics.
    // Verified against Go 1.26 stdlib `filepath.Base` (Linux). The previous
    // implementation fell back to `video.mp4` for any trailing-slash input,
    // which diverged from Go's "strip trailing separators, return the last
    // non-empty segment" rule.
    // ========================================================================

    #[test]
    fn generate_video_key_strips_trailing_slash_to_last_segment() -> VideoServiceResult<()> {
        // Go: filepath.Base("clip/")   = "clip"
        //     filepath.Base("/foo/")   = "foo"
        //     filepath.Base("trailing/") = "trailing"
        // The previous impl returned "video.mp4" here — a real parity bug.
        assert_eq!(
            generate_video_key(7, 99, "clip/"),
            "/7/requests/99/video/clip"
        );
        assert_eq!(
            generate_video_key(7, 99, "/foo/"),
            "/7/requests/99/video/foo"
        );
        assert_eq!(
            generate_video_key(7, 99, "trailing/"),
            "/7/requests/99/video/trailing"
        );
        Ok(())
    }

    #[test]
    fn generate_video_key_handles_nested_paths() -> VideoServiceResult<()> {
        // Go: filepath.Base("a/b/c")     = "c"
        //     filepath.Base("./clip.mp4") = "clip.mp4"
        assert_eq!(generate_video_key(7, 99, "a/b/c"), "/7/requests/99/video/c");
        assert_eq!(
            generate_video_key(7, 99, "./clip.mp4"),
            "/7/requests/99/video/clip.mp4"
        );
        Ok(())
    }

    #[test]
    fn generate_video_key_diverges_safely_on_all_separator_input() -> VideoServiceResult<()> {
        // Go: filepath.Base("/") on Linux returns "/" — which would produce a
        // broken storage key ("/7/requests/99/video//"). The upstream Go
        // worker guards this in `filenameFromResponse` (worker.go L294-296)
        // so the path is unreachable in production; we keep the safer
        // `video.mp4` fallback and document the divergence.
        assert_eq!(
            generate_video_key(7, 99, "/"),
            "/7/requests/99/video/video.mp4"
        );
        assert_eq!(
            generate_video_key(7, 99, "///"),
            "/7/requests/99/video/video.mp4"
        );
        Ok(())
    }

    #[test]
    fn generate_video_key_normalizes_windows_separators() -> VideoServiceResult<()> {
        // Go on Linux treats '\' as a literal char, but the worker runs on
        // Linux where filenames from Content-Disposition never contain '\'.
        // We normalize unconditionally so Windows-sourced test fixtures don't
        // leak backslashes into keys.
        assert_eq!(
            generate_video_key(7, 99, "C:\\videos\\clip.mp4"),
            "/7/requests/99/video/clip.mp4"
        );
        assert_eq!(
            generate_video_key(7, 99, "dir\\sub\\file.mp4"),
            "/7/requests/99/video/file.mp4"
        );
        Ok(())
    }

    // ========================================================================
    // RUST-P13-006 S08 — scheduler tick plan (pure duration arithmetic).
    // Mirrors the cadence of Go's `ScheduleFuncAtFixRate` (scheduler.go
    // L173-174). No Go *_test.go covers these helpers; tests pin the fixed-
    // rate predicate and next-fire arithmetic against wall-clock examples.
    // ========================================================================

    #[test]
    fn due_for_scan_returns_false_before_interval_elapses() -> VideoServiceResult<()> {
        // Interval = 5 min; only 4 min elapsed -> not due.
        let last = Utc::now();
        let now = last + chrono::Duration::minutes(4);
        assert!(!due_for_scan(last, now, 5));
        Ok(())
    }

    #[test]
    fn due_for_scan_returns_true_when_interval_elapsed() -> VideoServiceResult<()> {
        // Interval = 5 min; exactly 5 min elapsed -> due (inclusive).
        let last = Utc::now();
        let now = last + chrono::Duration::minutes(5);
        assert!(due_for_scan(last, now, 5));
        Ok(())
    }

    #[test]
    fn due_for_scan_returns_true_when_overdue() -> VideoServiceResult<()> {
        // Interval = 5 min; 10 min elapsed -> due.
        let last = Utc::now();
        let now = last + chrono::Duration::minutes(10);
        assert!(due_for_scan(last, now, 5));
        Ok(())
    }

    #[test]
    fn due_for_scan_uses_effective_interval_default_on_non_positive() -> VideoServiceResult<()> {
        // Non-positive interval is clamped to 0 -> any non-zero elapsed time
        // is due. This mirrors VideoStorageSettings::effective_scan_interval_minutes
        // which resolves <=0 to 1 *before* calling this helper; the clamp
        // here is purely defensive against direct callers.
        let last = Utc::now();
        let now = last + chrono::Duration::seconds(1);
        assert!(due_for_scan(last, now, 0));
        assert!(due_for_scan(last, now, -5));
        Ok(())
    }

    #[test]
    fn next_scan_at_is_last_plus_interval() -> VideoServiceResult<()> {
        // Fixed-rate scheduling: next = last + interval, no drift compensation.
        let last = DateTime::parse_from_rfc3339("2026-06-28T12:00:00Z")
            .map_err(|e| VideoServiceError::TaskNotFound(e.to_string()))?
            .with_timezone(&Utc);
        let next = next_scan_at(last, 5);
        assert_eq!(next, last + chrono::Duration::minutes(5));
        // Wall-clock equality check.
        let expected = DateTime::parse_from_rfc3339("2026-06-28T12:05:00Z")
            .map_err(|e| VideoServiceError::TaskNotFound(e.to_string()))?
            .with_timezone(&Utc);
        assert_eq!(next, expected);
        Ok(())
    }

    #[test]
    fn next_scan_at_clamps_non_positive_interval_to_zero() -> VideoServiceResult<()> {
        // Defensive: non-positive -> next == last (no forward motion).
        let last = Utc::now();
        assert_eq!(next_scan_at(last, 0), last);
        assert_eq!(next_scan_at(last, -10), last);
        Ok(())
    }

    #[test]
    fn due_for_scan_and_next_scan_at_compose_for_full_cycle() -> VideoServiceResult<()> {
        // End-to-end fixed-rate cycle: last + interval == next_scan_at, and
        // due_for_scan flips to true exactly at that timestamp.
        let last = DateTime::parse_from_rfc3339("2026-06-28T00:00:00Z")
            .map_err(|e| VideoServiceError::TaskNotFound(e.to_string()))?
            .with_timezone(&Utc);
        let interval = 1; // default per VideoStorageSettings
        let next = next_scan_at(last, interval);
        // One second before next: not due.
        assert!(!due_for_scan(
            last,
            next - chrono::Duration::seconds(1),
            interval
        ));
        // Exactly at next: due.
        assert!(due_for_scan(last, next, interval));
        Ok(())
    }

    // ========================================================================
    // RUST-P13-006 S11 — failure-isolation reducer.
    // Mirrors Go's `scanAndSave` per-candidate loop (worker.go L150-157):
    // one failure MUST NOT abort the batch. No Go *_test.go covers this; the
    // tests pin the per-outcome tally and the no-short-circuit guarantee.
    // ========================================================================

    #[test]
    fn reduce_scan_outcomes_tallies_each_outcome_category() -> VideoServiceResult<()> {
        let outcomes = vec![
            SaveOutcome::Saved {
                data_storage_id: 1,
                storage_key: "/p/r/1/video/a.mp4".to_string(),
                saved_at: Utc::now(),
            },
            SaveOutcome::Failed {
                error: "HTTP 502".to_string(),
            },
            SaveOutcome::NotReady,
            SaveOutcome::Saved {
                data_storage_id: 1,
                storage_key: "/p/r/2/video/b.mp4".to_string(),
                saved_at: Utc::now(),
            },
            SaveOutcome::NotReady,
        ];
        let summary = reduce_scan_outcomes(&outcomes);
        assert_eq!(summary.succeeded, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.skipped, 2);
        assert_eq!(summary.total(), 5);
        Ok(())
    }

    #[test]
    fn reduce_scan_outcomes_does_not_short_circuit_on_failure() -> VideoServiceResult<()> {
        // Go: `log.Warn(...); continue` — a failure is recorded but the loop
        // keeps going. The reducer must visit every outcome.
        let outcomes: Vec<SaveOutcome> = (0..10)
            .map(|i| {
                if i % 3 == 0 {
                    SaveOutcome::Failed {
                        error: format!("err {i}"),
                    }
                } else {
                    SaveOutcome::Saved {
                        data_storage_id: 1,
                        storage_key: format!("/p/r/{i}/video/x.mp4"),
                        saved_at: Utc::now(),
                    }
                }
            })
            .collect();
        let summary = reduce_scan_outcomes(&outcomes);
        // i in {0,3,6,9} -> 4 failures; the rest -> 6 successes.
        assert_eq!(summary.failed, 4);
        assert_eq!(summary.succeeded, 6);
        assert_eq!(summary.skipped, 0);
        assert_eq!(summary.total(), 10);
        Ok(())
    }

    #[test]
    fn reduce_scan_outcomes_handles_empty_batch() -> VideoServiceResult<()> {
        let summary = reduce_scan_outcomes(std::iter::empty::<&SaveOutcome>());
        assert_eq!(
            summary,
            ScanSummary {
                succeeded: 0,
                failed: 0,
                skipped: 0
            }
        );
        assert_eq!(summary.total(), 0);
        Ok(())
    }

    #[test]
    fn reduce_scan_outcomes_all_failed_batch_still_summarizes() -> VideoServiceResult<()> {
        // Worst case: every candidate failed. Go would `continue` past all of
        // them and the next scan cycle would retry. The reducer reports the
        // full failure count without aborting.
        let outcomes: Vec<SaveOutcome> = (0..5)
            .map(|i| SaveOutcome::Failed {
                error: format!("timeout {i}"),
            })
            .collect();
        let summary = reduce_scan_outcomes(&outcomes);
        assert_eq!(summary.failed, 5);
        assert_eq!(summary.succeeded, 0);
        assert_eq!(summary.skipped, 0);
        Ok(())
    }

    #[test]
    fn scan_summary_total_is_sum_of_counters() -> VideoServiceResult<()> {
        let s = ScanSummary {
            succeeded: 3,
            failed: 2,
            skipped: 1,
        };
        assert_eq!(s.total(), 6);
        Ok(())
    }

    #[test]
    fn reduce_scan_outcomes_accepts_owned_and_ref_iterators() -> VideoServiceResult<()> {
        // The generic `IntoIterator<Item = AsRef<SaveOutcome>>` bound should
        // accept both `Vec<SaveOutcome>` (owned) and `Vec<&SaveOutcome>` (refs).
        let owned = [
            SaveOutcome::Saved {
                data_storage_id: 1,
                storage_key: "/a".to_string(),
                saved_at: Utc::now(),
            },
            SaveOutcome::NotReady,
        ];
        let summary_owned = reduce_scan_outcomes(owned.iter());
        assert_eq!(summary_owned.succeeded, 1);
        assert_eq!(summary_owned.skipped, 1);

        let failed = SaveOutcome::Failed {
            error: "e".to_string(),
        };
        let not_ready = SaveOutcome::NotReady;
        let refs: [&SaveOutcome; 2] = [&failed, &not_ready];
        let summary_refs = reduce_scan_outcomes(refs);
        assert_eq!(summary_refs.failed, 1);
        assert_eq!(summary_refs.skipped, 1);
        Ok(())
    }

    // ========================================================================
    // RUST-P13-006 S08/S10 — scan preflight (Go scanAndSave L115-130).
    // Mirrors the precondition checks at the top of Go's `scanAndSave`. There
    // is no Go `*_test.go` for worker.go; these tests pin the pure-logic
    // contract inferred from the production code paths cited inline.
    // ========================================================================

    fn enabled_settings() -> VideoStorageSettings {
        VideoStorageSettings {
            enabled: true,
            data_storage_id: 7,
            ..VideoStorageSettings::default()
        }
    }

    fn valid_storage() -> DataStorageProps {
        DataStorageProps {
            is_primary: false,
            is_database: false,
        }
    }

    #[test]
    fn preflight_returns_disabled_when_settings_disabled() -> VideoServiceResult<()> {
        // Go: `if !settings.Enabled { return nil }` (worker.go L115-117).
        // Disabled is NOT an error — Go returns nil silently.
        let settings = VideoStorageSettings {
            enabled: false,
            data_storage_id: 7,
            ..VideoStorageSettings::default()
        };
        let decision = evaluate_scan_preflight(&settings, Some(valid_storage()));

        assert_eq!(decision, ScanPreflight::Disabled);
        assert!(!decision.should_proceed());
        assert!(
            !decision.is_error(),
            "Disabled is a silent no-op in Go, not an error"
        );
        assert_eq!(decision.go_error_message(), None);
        Ok(())
    }

    #[test]
    fn preflight_returns_missing_data_storage_id_when_zero() -> VideoServiceResult<()> {
        // Go: `if settings.DataStorageID == 0 { return err }` (worker.go L119-121).
        // Checked *before* the storage row fetch — storage_props is irrelevant.
        let settings = VideoStorageSettings {
            enabled: true,
            data_storage_id: 0,
            ..VideoStorageSettings::default()
        };
        let decision = evaluate_scan_preflight(&settings, None);

        assert_eq!(decision, ScanPreflight::MissingDataStorageId);
        assert!(!decision.should_proceed());
        assert!(decision.is_error());
        assert_eq!(
            decision.go_error_message(),
            Some("video storage enabled but data_storage_id is not set")
        );
        Ok(())
    }

    #[test]
    fn preflight_short_circuits_on_disabled_before_checking_storage_id() -> VideoServiceResult<()> {
        // Go evaluation order: enabled check (L115) precedes data_storage_id
        // check (L119). A disabled config with data_storage_id==0 must return
        // Disabled, not MissingDataStorageId.
        let settings = VideoStorageSettings {
            enabled: false,
            data_storage_id: 0,
            ..VideoStorageSettings::default()
        };
        let decision = evaluate_scan_preflight(&settings, None);
        assert_eq!(decision, ScanPreflight::Disabled);
        Ok(())
    }

    #[test]
    fn preflight_returns_invalid_data_storage_when_primary() -> VideoServiceResult<()> {
        // Go: `if ds.Primary || ds.Type == datastorage.TypeDatabase` (L128).
        // Primary storage is rejected even if it is non-database.
        let decision = evaluate_scan_preflight(
            &enabled_settings(),
            Some(DataStorageProps {
                is_primary: true,
                is_database: false,
            }),
        );
        match decision {
            ScanPreflight::InvalidDataStorage {
                is_primary,
                is_database,
            } => {
                assert!(is_primary);
                assert!(!is_database);
            }
            other => {
                return Err(VideoServiceError::TaskNotFound(format!(
                    "expected InvalidDataStorage, got {other:?}"
                )));
            }
        }
        assert!(!decision.should_proceed());
        assert!(decision.is_error());
        assert_eq!(
            decision.go_error_message(),
            Some("video storage must be non-database storage")
        );
        Ok(())
    }

    #[test]
    fn preflight_returns_invalid_data_storage_when_database() -> VideoServiceResult<()> {
        // Go: database-backed storage is rejected (worker.go L128-129).
        let decision = evaluate_scan_preflight(
            &enabled_settings(),
            Some(DataStorageProps {
                is_primary: false,
                is_database: true,
            }),
        );
        match decision {
            ScanPreflight::InvalidDataStorage {
                is_primary,
                is_database,
            } => {
                assert!(!is_primary);
                assert!(is_database);
            }
            other => {
                return Err(VideoServiceError::TaskNotFound(format!(
                    "expected InvalidDataStorage, got {other:?}"
                )));
            }
        }
        assert!(decision.is_error());
        Ok(())
    }

    #[test]
    fn preflight_returns_proceed_for_valid_configuration() -> VideoServiceResult<()> {
        // Go: all guards pass -> falls through to the candidate query (L132+).
        let decision = evaluate_scan_preflight(&enabled_settings(), Some(valid_storage()));
        assert_eq!(decision, ScanPreflight::Proceed);
        assert!(decision.should_proceed());
        assert!(!decision.is_error());
        assert_eq!(decision.go_error_message(), None);
        Ok(())
    }

    #[test]
    fn preflight_proceed_with_storage_props_none_is_unchecked() -> VideoServiceResult<()> {
        // When storage_props is None the storage-type guard is skipped. This
        // mirrors a caller that has not yet fetched the row; Go would have
        // already errored on the fetch (L123-126) before reaching L128, so the
        // unchecked path is only reachable when the caller defers the fetch.
        // We document this by showing the decision is Proceed (not an error).
        let decision = evaluate_scan_preflight(&enabled_settings(), None);
        assert_eq!(decision, ScanPreflight::Proceed);
        assert!(!decision.is_error());
        Ok(())
    }

    #[test]
    fn preflight_and_build_scan_plan_compose_for_full_scan_cycle() -> VideoServiceResult<()> {
        // End-to-end pure-logic scan cycle:
        //   1. preflight (may we scan?) -> Proceed
        //   2. build_scan_plan (which rows?) -> limited selection
        //   3. reduce_scan_outcomes (what happened?) -> summary
        // Mirrors the top-to-bottom flow of Go's scanAndSave + loop.
        let settings = enabled_settings();
        let preflight = evaluate_scan_preflight(&settings, Some(valid_storage()));
        assert!(preflight.should_proceed());

        let candidates: Vec<VideoScanCandidate> = (1..=60).map(|i| candidate(i, 1)).collect();
        let plan = build_scan_plan(candidates, &settings, Utc::now());
        assert_eq!(plan.effective_limit, 50);
        assert_eq!(plan.selected_request_ids.len(), 50);
        assert_eq!(plan.deferred_count, 10);

        // Simulate every selected request saving successfully.
        let outcomes: Vec<SaveOutcome> = plan
            .selected_request_ids
            .iter()
            .map(|rid| SaveOutcome::Saved {
                data_storage_id: settings.data_storage_id,
                storage_key: generate_video_key(1, *rid, "clip.mp4"),
                saved_at: Utc::now(),
            })
            .collect();
        let summary = reduce_scan_outcomes(&outcomes);
        assert_eq!(summary.succeeded, 50);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.total(), 50);
        Ok(())
    }

    #[test]
    fn preflight_and_build_scan_plan_skips_entire_cycle_when_disabled() -> VideoServiceResult<()> {
        // When the worker is disabled, the caller should not even build a
        // plan. This test pins the short-circuit: preflight -> Disabled means
        // build_scan_plan is never called.
        let settings = VideoStorageSettings {
            enabled: false,
            ..VideoStorageSettings::default()
        };
        let preflight = evaluate_scan_preflight(&settings, Some(valid_storage()));
        assert_eq!(preflight, ScanPreflight::Disabled);
        assert!(!preflight.should_proceed());
        // Caller would `return` here; build_scan_plan is not invoked.
        Ok(())
    }

    // ========================================================================
    // RUST-P7-006 S08/S12 — external-id persistence + delete flow.
    // Go has no biz/video_test.go or api/doubao_test.go (only transformer /
    // integration tests exist); these parity tests pin the production
    // behavior of biz/video.go with inline line references.
    // ========================================================================

    /// Fake provider gateway. Records calls so tests can assert that the
    /// loadTask guards short-circuit BEFORE any provider round-trip
    /// (biz/video.go:128-134 run before BuildGet/DeleteVideoTaskRequest).
    #[derive(Default)]
    struct FakeVideoTaskGateway {
        /// `Some(task)` -> get succeeds with it; `None` -> Provider error.
        get_response: Option<ProviderVideoTask>,
        /// `false` -> delete fails with a Provider error.
        delete_ok: bool,
        get_calls: Mutex<Vec<(i64, String)>>,
        delete_calls: Mutex<Vec<(i64, String)>>,
    }

    impl FakeVideoTaskGateway {
        fn get_call_count(&self) -> VideoServiceResult<usize> {
            Ok(self
                .get_calls
                .lock()
                .map_err(|_| VideoServiceError::LockPoisoned)?
                .len())
        }

        fn delete_call_log(&self) -> VideoServiceResult<Vec<(i64, String)>> {
            Ok(self
                .delete_calls
                .lock()
                .map_err(|_| VideoServiceError::LockPoisoned)?
                .clone())
        }
    }

    #[async_trait]
    impl VideoTaskGateway for FakeVideoTaskGateway {
        async fn get_video_task(
            &self,
            _ctx: &RequestContext,
            channel_id: i64,
            external_id: &str,
        ) -> VideoServiceResult<ProviderVideoTask> {
            self.get_calls
                .lock()
                .map_err(|_| VideoServiceError::LockPoisoned)?
                .push((channel_id, external_id.to_string()));
            self.get_response
                .clone()
                .ok_or_else(|| VideoServiceError::Provider("get task failed".to_string()))
        }

        async fn delete_video_task(
            &self,
            _ctx: &RequestContext,
            channel_id: i64,
            external_id: &str,
        ) -> VideoServiceResult<()> {
            self.delete_calls
                .lock()
                .map_err(|_| VideoServiceError::LockPoisoned)?
                .push((channel_id, external_id.to_string()));
            if self.delete_ok {
                Ok(())
            } else {
                Err(VideoServiceError::Provider(
                    "delete task failed".to_string(),
                ))
            }
        }
    }

    /// Seed fixture: a video-task request row as the create flow leaves it —
    /// status "processing" with the provider task id in external_id
    /// (orchestrator/request.go:110-126) and the serving channel recorded.
    /// external_id/channel_id are seeded directly on the record's
    /// extra-column face (the in-memory stand-ins for the ent columns
    /// `requests.external_id` / `requests.channel_id`).
    fn video_request_record(
        request_id: &str,
        project_id: &str,
        external_id: &str,
        channel_id: i64,
    ) -> RequestRecord {
        let mut record =
            RequestRecord::new(request_id, request_id, project_id, "POST", "/v1/videos");
        record.status = RequestStatus::Running;
        if !external_id.is_empty() {
            record
                .extra
                .insert("external_id".to_string(), Value::from(external_id));
        }
        if channel_id != 0 {
            record
                .extra
                .insert("channel_id".to_string(), Value::from(channel_id));
        }
        record
    }

    fn provider_task(status: &str) -> ProviderVideoTask {
        ProviderVideoTask {
            status: status.to_string(),
            video: json!({
                "id": "cgt-1",
                "status": status,
                "video_url": "https://cdn.example.com/v.mp4"
            }),
        }
    }

    /// Golden table for `mapVideoStatusToRequestStatus` (biz/video.go:165-176):
    /// succeeded -> completed, failed -> failed, queued/running -> processing,
    /// default -> processing; matching is trimmed + case-insensitive
    /// (`strings.ToLower(strings.TrimSpace(status))`).
    #[test]
    fn s08_map_video_status_to_request_status_matches_go_table() -> VideoServiceResult<()> {
        let cases = [
            ("succeeded", RequestStatus::Succeeded),
            ("  SUCCEEDED ", RequestStatus::Succeeded),
            ("failed", RequestStatus::Failed),
            ("Failed", RequestStatus::Failed),
            ("queued", RequestStatus::Running),
            ("running", RequestStatus::Running),
            ("", RequestStatus::Running),
            ("something-else", RequestStatus::Running),
        ];
        for (input, expected) in cases {
            assert_eq!(
                map_video_status_to_request_status(input),
                expected,
                "input={input:?}"
            );
        }
        Ok(())
    }

    /// biz/video.go:145-150: only channel.TypeDoubao ("doubao") selects the
    /// Seedance format; every other type — including "doubao_anthropic"
    /// (ent/channel/channel.go:217) — falls to the OpenAI default arm.
    #[test]
    fn s12_video_api_format_for_channel_type_matches_go_switch() -> VideoServiceResult<()> {
        assert_eq!(
            video_api_format_for_channel_type("doubao"),
            "seedance/video"
        );
        assert_eq!(
            video_api_format_for_channel_type("doubao_anthropic"),
            "openai/video"
        );
        assert_eq!(video_api_format_for_channel_type("openai"), "openai/video");
        assert_eq!(video_api_format_for_channel_type(""), "openai/video");
        Ok(())
    }

    /// Guard 1 (biz/video.go:128-130): trimmed-empty external_id is rejected.
    #[test]
    fn s12_plan_delete_task_rejects_missing_external_id() -> VideoServiceResult<()> {
        for external_id in ["", "   "] {
            let row = VideoTaskRow {
                request_id: "req-1".to_string(),
                project_id: "project-a".to_string(),
                external_id: external_id.to_string(),
                channel_id: 7,
            };
            assert_eq!(
                plan_delete_task(&row),
                Err(VideoServiceError::MissingExternalId),
                "external_id={external_id:?}"
            );
        }
        Ok(())
    }

    /// Guard 2 (biz/video.go:132-134): channel_id zero is rejected. Guard
    /// ORDER also mirrors Go: with both fields missing, the external_id
    /// guard (video.go:128) fires first.
    #[test]
    fn s12_plan_delete_task_rejects_missing_channel_id_after_external_id() -> VideoServiceResult<()>
    {
        let row = VideoTaskRow {
            request_id: "req-1".to_string(),
            project_id: "project-a".to_string(),
            external_id: "cgt-1".to_string(),
            channel_id: 0,
        };
        assert_eq!(
            plan_delete_task(&row),
            Err(VideoServiceError::MissingChannelId)
        );

        let both_missing = VideoTaskRow {
            request_id: "req-1".to_string(),
            project_id: "project-a".to_string(),
            external_id: String::new(),
            channel_id: 0,
        };
        assert_eq!(
            plan_delete_task(&both_missing),
            Err(VideoServiceError::MissingExternalId)
        );
        Ok(())
    }

    /// Valid row -> ordered plan carrying the untrimmed external id (Go
    /// passes `task.ExternalID` raw to BuildDeleteVideoTaskRequest,
    /// video.go:101).
    #[test]
    fn s12_plan_delete_task_builds_plan_from_valid_row() -> VideoServiceResult<()> {
        let row = VideoTaskRow {
            request_id: "req-1".to_string(),
            project_id: "project-a".to_string(),
            external_id: " cgt-raw ".to_string(),
            channel_id: 7,
        };
        let plan = plan_delete_task(&row)?;
        assert_eq!(
            plan,
            DeleteTaskPlan {
                project_id: "project-a".to_string(),
                request_id: "req-1".to_string(),
                channel_id: 7,
                external_id: " cgt-raw ".to_string(),
            }
        );
        Ok(())
    }

    /// from_record defaults mirror the Go zero values ("" / 0) that the
    /// loadTask guards test against (video.go:128-134).
    #[test]
    fn s08_video_task_row_from_record_defaults_to_go_zero_values() -> VideoServiceResult<()> {
        let bare = RequestRecord::new("req-1", "req-1", "project-a", "POST", "/v1/videos");
        let row = VideoTaskRow::from_record(&bare);
        assert_eq!(row.external_id, "");
        assert_eq!(row.channel_id, 0);
        assert_eq!(row.request_id, "req-1");
        assert_eq!(row.project_id, "project-a");

        let seeded = video_request_record("req-2", "project-a", "cgt-2", 9);
        let row = VideoTaskRow::from_record(&seeded);
        assert_eq!(row.external_id, "cgt-2");
        assert_eq!(row.channel_id, 9);
        Ok(())
    }

    /// Wires a [`VideoTaskService`] over an in-memory repo pre-seeded with
    /// one request row; returns the request service too so tests can read
    /// the row back.
    async fn video_flow_fixture(
        gateway: Arc<FakeVideoTaskGateway>,
        record: RequestRecord,
    ) -> VideoServiceResult<(VideoTaskService, Arc<RequestService>)> {
        let repo = Arc::new(InMemoryRequestPersistenceRepo::new());
        let requests = Arc::new(RequestService::new(repo));
        requests.create_request(&ctx(), record).await?;
        let service = VideoTaskService::new(gateway, requests.clone());
        Ok((service, requests))
    }

    /// Repo wrapper that delegates reads/creates to an in-memory repo but
    /// FAILS the two row-status write methods. Exercises Go's best-effort
    /// persistence: GetTask swallows the snapshot-write error
    /// (biz/video.go:52-54) and DeleteTask ignores the local-cancel error
    /// (biz/video.go:111-112).
    struct FailingWriteRepo {
        inner: InMemoryRequestPersistenceRepo,
    }

    #[async_trait]
    impl RequestPersistenceRepo for FailingWriteRepo {
        async fn insert_request(
            &self,
            ctx: &RequestContext,
            request: RequestRecord,
        ) -> RequestServiceResult<RequestRecord> {
            self.inner.insert_request(ctx, request).await
        }

        async fn find_request(
            &self,
            ctx: &RequestContext,
            project_id: &str,
            request_id: &str,
        ) -> RequestServiceResult<Option<RequestRecord>> {
            self.inner.find_request(ctx, project_id, request_id).await
        }

        async fn find_requests_by_external_id(
            &self,
            ctx: &RequestContext,
            external_id: &str,
        ) -> RequestServiceResult<Vec<RequestRecord>> {
            self.inner
                .find_requests_by_external_id(ctx, external_id)
                .await
        }

        async fn insert_execution(
            &self,
            ctx: &RequestContext,
            execution: ExecutionRecord,
        ) -> RequestServiceResult<ExecutionRecord> {
            self.inner.insert_execution(ctx, execution).await
        }

        async fn list_executions(
            &self,
            ctx: &RequestContext,
            project_id: &str,
            request_id: &str,
        ) -> RequestServiceResult<Vec<ExecutionRecord>> {
            self.inner
                .list_executions(ctx, project_id, request_id)
                .await
        }

        async fn transition_request_status(
            &self,
            ctx: &RequestContext,
            project_id: &str,
            request_id: &str,
            expected_status: RequestStatus,
            next_status: RequestStatus,
        ) -> RequestServiceResult<Option<RequestRecord>> {
            self.inner
                .transition_request_status(
                    ctx,
                    project_id,
                    request_id,
                    expected_status,
                    next_status,
                )
                .await
        }

        async fn update_request_status_completed(
            &self,
            _ctx: &RequestContext,
            _project_id: &str,
            _request_id: &str,
            _next_status: RequestStatus,
            _patch: RequestCompletionPatch,
        ) -> RequestServiceResult<RequestRecord> {
            Err(RequestServiceError::LockPoisoned)
        }

        async fn update_request_status(
            &self,
            _ctx: &RequestContext,
            _project_id: &str,
            _request_id: &str,
            _next_status: RequestStatus,
        ) -> RequestServiceResult<RequestRecord> {
            Err(RequestServiceError::LockPoisoned)
        }

        async fn update_execution_completed(
            &self,
            ctx: &RequestContext,
            project_id: &str,
            request_id: &str,
            execution_id: &str,
            patch: ExecutionCompletionPatch,
        ) -> RequestServiceResult<ExecutionRecord> {
            self.inner
                .update_execution_completed(ctx, project_id, request_id, execution_id, patch)
                .await
        }

        async fn update_execution_status(
            &self,
            ctx: &RequestContext,
            project_id: &str,
            request_id: &str,
            execution_id: &str,
            patch: ExecutionStatusPatch,
        ) -> RequestServiceResult<ExecutionRecord> {
            self.inner
                .update_execution_status(ctx, project_id, request_id, execution_id, patch)
                .await
        }

        async fn set_execution_response_chunks(
            &self,
            ctx: &RequestContext,
            project_id: &str,
            request_id: &str,
            execution_id: &str,
            chunks: Value,
        ) -> RequestServiceResult<ExecutionRecord> {
            self.inner
                .set_execution_response_chunks(ctx, project_id, request_id, execution_id, chunks)
                .await
        }

        async fn set_request_response_chunks(
            &self,
            ctx: &RequestContext,
            project_id: &str,
            request_id: &str,
            chunks: Value,
        ) -> RequestServiceResult<RequestRecord> {
            self.inner
                .set_request_response_chunks(ctx, project_id, request_id, chunks)
                .await
        }
    }

    /// GetTask happy path (biz/video.go:27-57): provider polled with the
    /// row's channel/external id, snapshot persisted (mapped status + SAME
    /// external id + `video` payload as response body, nil metrics —
    /// video.go:48-52), provider data returned.
    #[tokio::test]
    async fn s08_get_task_persists_snapshot_and_returns_provider_data() -> VideoServiceResult<()> {
        let gateway = Arc::new(FakeVideoTaskGateway {
            get_response: Some(provider_task("succeeded")),
            delete_ok: true,
            ..FakeVideoTaskGateway::default()
        });
        let record = video_request_record("req-1", "project-a", "cgt-1", 7);
        let (service, requests) = video_flow_fixture(gateway.clone(), record).await?;

        let task = service.get_task(&ctx(), "project-a", "req-1").await?;
        assert_eq!(task, provider_task("succeeded"));

        // Snapshot persisted on the request row (video.go:49-52 ->
        // request.go:601-603/639: SetStatus + SetExternalID + response body).
        let row = requests.get_request(&ctx(), "project-a", "req-1").await?;
        assert_eq!(row.status, RequestStatus::Succeeded);
        assert_eq!(row.external_id(), Some("cgt-1"));
        assert_eq!(
            row.extra.get("response_body"),
            Some(&provider_task("succeeded").video)
        );

        // Provider was polled exactly once with the row's channel/external id.
        assert_eq!(gateway.get_call_count()?, 1);
        Ok(())
    }

    /// A still-running provider task keeps the local row in processing
    /// (video.go:171-175 "queued"/"running" -> StatusProcessing).
    #[tokio::test]
    async fn s08_get_task_running_status_keeps_row_processing() -> VideoServiceResult<()> {
        let gateway = Arc::new(FakeVideoTaskGateway {
            get_response: Some(provider_task("running")),
            delete_ok: true,
            ..FakeVideoTaskGateway::default()
        });
        let record = video_request_record("req-1", "project-a", "cgt-1", 7);
        let (service, requests) = video_flow_fixture(gateway, record).await?;

        service.get_task(&ctx(), "project-a", "req-1").await?;

        let row = requests.get_request(&ctx(), "project-a", "req-1").await?;
        assert_eq!(row.status, RequestStatus::Running);
        Ok(())
    }

    /// Snapshot-persist failure is NON-fatal: Go swallows the error and
    /// returns the provider data anyway ("non-fatal: return data anyway",
    /// biz/video.go:51-54).
    #[tokio::test]
    async fn s08_get_task_snapshot_persist_failure_is_non_fatal() -> VideoServiceResult<()> {
        let inner = InMemoryRequestPersistenceRepo::new();
        let ctx = ctx();
        inner
            .insert_request(&ctx, video_request_record("req-1", "project-a", "cgt-1", 7))
            .await?;
        let requests = Arc::new(RequestService::new(Arc::new(FailingWriteRepo { inner })));
        let gateway = Arc::new(FakeVideoTaskGateway {
            get_response: Some(provider_task("succeeded")),
            delete_ok: true,
            ..FakeVideoTaskGateway::default()
        });
        let service = VideoTaskService::new(gateway, requests);

        let task = service.get_task(&ctx, "project-a", "req-1").await?;
        assert_eq!(task, provider_task("succeeded"));
        Ok(())
    }

    /// loadTask guard order: a row without external_id errors BEFORE any
    /// provider round-trip (biz/video.go:128-130 precede video.go:33).
    #[tokio::test]
    async fn s08_get_task_missing_external_id_short_circuits_before_gateway()
    -> VideoServiceResult<()> {
        let gateway = Arc::new(FakeVideoTaskGateway {
            get_response: Some(provider_task("succeeded")),
            delete_ok: true,
            ..FakeVideoTaskGateway::default()
        });
        let record = video_request_record("req-1", "project-a", "", 7);
        let (service, _requests) = video_flow_fixture(gateway.clone(), record).await?;

        let err = service.get_task(&ctx(), "project-a", "req-1").await;
        assert_eq!(err, Err(VideoServiceError::MissingExternalId));
        assert_eq!(gateway.get_call_count()?, 0);
        Ok(())
    }

    /// GetTaskByExternalID (biz/video.go:59-75): resolve the unique row by
    /// provider task id, then poll + persist through GetTask (video.go:74).
    #[tokio::test]
    async fn s08_get_task_by_external_id_resolves_then_polls() -> VideoServiceResult<()> {
        let gateway = Arc::new(FakeVideoTaskGateway {
            get_response: Some(provider_task("succeeded")),
            delete_ok: true,
            ..FakeVideoTaskGateway::default()
        });
        let record = video_request_record("req-1", "project-a", "cgt-1", 7);
        let (service, requests) = video_flow_fixture(gateway, record).await?;

        let task = service.get_task_by_external_id(&ctx(), "cgt-1").await?;
        assert_eq!(task.status, "succeeded");

        let row = requests.get_request(&ctx(), "project-a", "req-1").await?;
        assert_eq!(row.status, RequestStatus::Succeeded);
        Ok(())
    }

    /// Unknown external id propagates the lookup error (ent NotFoundError
    /// from `.Only`, biz/video.go:70-72).
    #[tokio::test]
    async fn s08_get_task_by_external_id_not_found_propagates() -> VideoServiceResult<()> {
        let gateway = Arc::new(FakeVideoTaskGateway::default());
        let record = video_request_record("req-1", "project-a", "cgt-1", 7);
        let (service, _requests) = video_flow_fixture(gateway, record).await?;

        let err = service.get_task_by_external_id(&ctx(), "cgt-unknown").await;
        assert_eq!(
            err,
            Err(VideoServiceError::Request(
                RequestServiceError::RequestNotFound("cgt-unknown".to_string())
            ))
        );
        Ok(())
    }

    /// S12 core ordering: provider delete FIRST; on provider failure the
    /// error is returned and the local row is left UNTOUCHED — no status
    /// change, no deletion (biz/video.go:106-109 `return err` precedes the
    /// local update at 111-112).
    #[tokio::test]
    async fn s12_delete_task_provider_failure_leaves_local_row_untouched() -> VideoServiceResult<()>
    {
        let gateway = Arc::new(FakeVideoTaskGateway {
            get_response: None,
            delete_ok: false,
            ..FakeVideoTaskGateway::default()
        });
        let record = video_request_record("req-1", "project-a", "cgt-1", 7);
        let (service, requests) = video_flow_fixture(gateway.clone(), record).await?;

        let err = service.delete_task(&ctx(), "project-a", "req-1").await;
        assert_eq!(
            err,
            Err(VideoServiceError::Provider(
                "delete task failed".to_string()
            ))
        );

        // Local row untouched: still processing, external id intact.
        let row = requests.get_request(&ctx(), "project-a", "req-1").await?;
        assert_eq!(row.status, RequestStatus::Running);
        assert_eq!(row.external_id(), Some("cgt-1"));
        // The provider WAS attempted (order: provider before local).
        assert_eq!(gateway.delete_call_log()?, vec![(7, "cgt-1".to_string())]);
        Ok(())
    }

    /// S12 success path: after the provider delete succeeds, the local row
    /// is marked canceled — NOT hard-deleted (biz/video.go:111-114; Go flips
    /// status to request.StatusCanceled and keeps the row).
    #[tokio::test]
    async fn s12_delete_task_marks_local_row_canceled_not_deleted() -> VideoServiceResult<()> {
        let gateway = Arc::new(FakeVideoTaskGateway {
            get_response: None,
            delete_ok: true,
            ..FakeVideoTaskGateway::default()
        });
        let record = video_request_record("req-1", "project-a", "cgt-1", 7);
        let (service, requests) = video_flow_fixture(gateway.clone(), record).await?;

        service.delete_task(&ctx(), "project-a", "req-1").await?;

        // Row still exists (never hard-deleted) with canceled status.
        let row = requests.get_request(&ctx(), "project-a", "req-1").await?;
        assert_eq!(row.status, RequestStatus::Cancelled);
        assert_eq!(row.external_id(), Some("cgt-1"));
        assert_eq!(gateway.delete_call_log()?, vec![(7, "cgt-1".to_string())]);
        Ok(())
    }

    /// S12 fault tolerance: the local cancel is BEST-EFFORT — a local write
    /// failure after a successful provider delete is ignored and the call
    /// still succeeds (biz/video.go:111-112:
    /// `_ = s.RequestService.UpdateRequestStatus(...)`).
    #[tokio::test]
    async fn s12_delete_task_local_cancel_failure_is_swallowed() -> VideoServiceResult<()> {
        let inner = InMemoryRequestPersistenceRepo::new();
        let ctx = ctx();
        inner
            .insert_request(&ctx, video_request_record("req-1", "project-a", "cgt-1", 7))
            .await?;
        let requests = Arc::new(RequestService::new(Arc::new(FailingWriteRepo { inner })));
        let gateway = Arc::new(FakeVideoTaskGateway {
            get_response: None,
            delete_ok: true,
            ..FakeVideoTaskGateway::default()
        });
        let service = VideoTaskService::new(gateway.clone(), requests);

        // Provider delete succeeded, local cancel failed -> still Ok.
        service.delete_task(&ctx, "project-a", "req-1").await?;
        assert_eq!(gateway.delete_call_log()?, vec![(7, "cgt-1".to_string())]);
        Ok(())
    }

    /// S12 guard: a row without channel_id aborts BEFORE the provider call
    /// (biz/video.go:132-134 precede BuildDeleteVideoTaskRequest at 101).
    #[tokio::test]
    async fn s12_delete_task_guard_failure_never_calls_provider() -> VideoServiceResult<()> {
        let gateway = Arc::new(FakeVideoTaskGateway {
            get_response: None,
            delete_ok: true,
            ..FakeVideoTaskGateway::default()
        });
        let record = video_request_record("req-1", "project-a", "cgt-1", 0);
        let (service, requests) = video_flow_fixture(gateway.clone(), record).await?;

        let err = service.delete_task(&ctx(), "project-a", "req-1").await;
        assert_eq!(err, Err(VideoServiceError::MissingChannelId));
        assert!(gateway.delete_call_log()?.is_empty());

        // Row untouched.
        let row = requests.get_request(&ctx(), "project-a", "req-1").await?;
        assert_eq!(row.status, RequestStatus::Running);
        Ok(())
    }

    /// DeleteTaskByExternalID end-to-end (api/doubao.go:137-152 ->
    /// biz/video.go:77-93): resolve by provider task id, provider delete,
    /// local row canceled.
    #[tokio::test]
    async fn s12_delete_task_by_external_id_end_to_end() -> VideoServiceResult<()> {
        let gateway = Arc::new(FakeVideoTaskGateway {
            get_response: None,
            delete_ok: true,
            ..FakeVideoTaskGateway::default()
        });
        let record = video_request_record("req-1", "project-a", "cgt-1", 7);
        let (service, requests) = video_flow_fixture(gateway.clone(), record).await?;

        service.delete_task_by_external_id(&ctx(), "cgt-1").await?;

        let row = requests.get_request(&ctx(), "project-a", "req-1").await?;
        assert_eq!(row.status, RequestStatus::Cancelled);
        assert_eq!(gateway.delete_call_log()?, vec![(7, "cgt-1".to_string())]);
        Ok(())
    }

    /// Unknown external id on delete propagates the lookup error unchanged
    /// (biz/video.go:88-90 `return err`); nothing reaches the provider.
    #[tokio::test]
    async fn s12_delete_task_by_external_id_not_found_propagates() -> VideoServiceResult<()> {
        let gateway = Arc::new(FakeVideoTaskGateway::default());
        let record = video_request_record("req-1", "project-a", "cgt-1", 7);
        let (service, _requests) = video_flow_fixture(gateway.clone(), record).await?;

        let err = service
            .delete_task_by_external_id(&ctx(), "cgt-unknown")
            .await;
        assert_eq!(
            err,
            Err(VideoServiceError::Request(
                RequestServiceError::RequestNotFound("cgt-unknown".to_string())
            ))
        );
        assert!(gateway.delete_call_log()?.is_empty());
        Ok(())
    }

    // ========================================================================
    // RUST-P13-006 A01 — worker.go pure helpers (worker_test parity).
    // Go has no `*_test.go` for `internal/server/video_storage/worker.go`;
    // these tests pin its side-effect-free decision logic with inline line
    // references to the Go production source.
    // ========================================================================

    /// Go: `const maxBytes = 512 * 1024 * 1024` (worker.go L193).
    #[test]
    fn max_video_download_bytes_matches_go_ceiling() -> VideoServiceResult<()> {
        assert_eq!(MAX_VIDEO_DOWNLOAD_BYTES, 512 * 1024 * 1024);
        assert_eq!(MAX_VIDEO_DOWNLOAD_BYTES, 536_870_912);
        Ok(())
    }

    /// Go: `extractVideoURLFromResponseBody` empty-body short-circuit
    /// (worker.go L234-236 — `if len(raw) == 0 { return "", nil }`).
    #[test]
    fn extract_video_url_returns_none_for_empty_body() -> VideoServiceResult<()> {
        assert_eq!(extract_video_url_from_response_body(b""), None);
        assert_eq!(extract_video_url_from_response_body(&[]), None);
        Ok(())
    }

    /// Go: valid JSON with a non-empty `video_url` returns it trimmed
    /// (worker.go L242-243 unmarshals into `llm.VideoResponse.VideoURL`).
    #[test]
    fn extract_video_url_returns_url_when_present() -> VideoServiceResult<()> {
        let body =
            br#"{"id":"vid_1","status":"succeeded","video_url":"https://cdn.example.com/v.mp4"}"#;
        assert_eq!(
            extract_video_url_from_response_body(body),
            Some("https://cdn.example.com/v.mp4".to_string())
        );
        // Whitespace around the URL is trimmed by the helper to match Go's
        // `strings.TrimSpace(v) != ""` consumer-side guard (worker.go L163).
        let with_ws = br#"{"video_url":"  https://cdn.example.com/v.mp4  "}"#;
        assert_eq!(
            extract_video_url_from_response_body(with_ws),
            Some("https://cdn.example.com/v.mp4".to_string())
        );
        Ok(())
    }

    /// Go: valid JSON with an empty / missing `video_url` returns `""`, which
    /// the caller treats as "no URL" (worker.go L167). We surface `None` so
    /// the caller can `?`-chain straight into a GetTask fallback.
    #[test]
    fn extract_video_url_returns_none_when_field_empty_or_missing() -> VideoServiceResult<()> {
        // Missing field entirely.
        assert_eq!(
            extract_video_url_from_response_body(br#"{"id":"v","status":"queued"}"#),
            None
        );
        // Explicit empty string.
        assert_eq!(
            extract_video_url_from_response_body(br#"{"video_url":""}"#),
            None
        );
        // Whitespace-only string.
        assert_eq!(
            extract_video_url_from_response_body(br#"{"video_url":"   "}"#),
            None
        );
        Ok(())
    }

    /// Go: invalid JSON returns an `json.Unmarshal` error (worker.go L239-241).
    /// The only caller discards the error via `if ... err == nil && ...`
    /// (worker.go L163), so the observable behavior is "no cached URL". The
    /// Rust helper folds this into `None`.
    #[test]
    fn extract_video_url_returns_none_on_invalid_json() -> VideoServiceResult<()> {
        assert_eq!(extract_video_url_from_response_body(b"not json"), None);
        assert_eq!(extract_video_url_from_response_body(b"{"), None);
        assert_eq!(
            extract_video_url_from_response_body(b"{\"video_url\": 42}"),
            None
        );
        Ok(())
    }

    /// Go: `openVideoStream` accepts only `http` and `https` schemes
    /// (worker.go L252-253).
    #[test]
    fn is_valid_video_download_url_accepts_only_http_https() -> VideoServiceResult<()> {
        assert!(is_valid_video_download_url("https://cdn.example.com/v.mp4"));
        assert!(is_valid_video_download_url("http://example.com/v.mp4"));
        // Rejected schemes (worker.go L252-253 returns "invalid URL scheme").
        assert!(!is_valid_video_download_url("file:///etc/passwd"));
        assert!(!is_valid_video_download_url("ftp://example.com/v.mp4"));
        assert!(!is_valid_video_download_url("data:text/plain,hello"));
        // Malformed URL (Go: url.Parse error, worker.go L247-250).
        assert!(!is_valid_video_download_url("ht!tp://not a url"));
        Ok(())
    }

    /// Go: `filenameFromResponse` parses Content-Disposition when present and
    /// unquotes the filename (worker.go L278-286).
    #[test]
    fn filename_from_response_parses_content_disposition() -> VideoServiceResult<()> {
        // Plain filename.
        assert_eq!(
            filename_from_response(Some("attachment; filename=clip.mp4"), "https://x/v.mp4", 0),
            "clip.mp4"
        );
        // Quoted filename (Go: Trim(after, "\"") — worker.go L281).
        assert_eq!(
            filename_from_response(
                Some("attachment; filename=\"my clip.mp4\""),
                "https://x/v.mp4",
                0
            ),
            "my clip.mp4"
        );
        // Surrounding whitespace trimmed (Go: TrimSpace, worker.go L280).
        assert_eq!(
            filename_from_response(Some("filename=   clip.mp4   "), "https://x/v.mp4", 0),
            "clip.mp4"
        );
        Ok(())
    }

    /// Go: when Content-Disposition has `filename=` but the value is empty,
    /// the parser falls through to the URL fallback (worker.go L282-285 —
    /// `if after != "" { return after }`).
    #[test]
    fn filename_from_response_ignores_empty_content_disposition_value() -> VideoServiceResult<()> {
        assert_eq!(
            filename_from_response(Some("filename="), "https://x/path/clip.mp4", 0),
            "clip.mp4"
        );
        assert_eq!(
            filename_from_response(Some("filename=\"\""), "https://x/clip.mp4", 0),
            "clip.mp4"
        );
        Ok(())
    }

    /// Go: with no Content-Disposition, falls back to `filepath.Base` of the
    /// URL with query stripped (worker.go L289-293).
    #[test]
    fn filename_from_response_falls_back_to_url_basename() -> VideoServiceResult<()> {
        assert_eq!(
            filename_from_response(None, "https://cdn.example.com/path/clip.mp4?token=abc", 0),
            "clip.mp4"
        );
        assert_eq!(
            filename_from_response(None, "https://cdn.example.com/clip.mp4", 0),
            "clip.mp4"
        );
        Ok(())
    }

    /// Go: when the URL basename is `.`, `/`, or empty, falls back to
    /// `video-<unix>.mp4` (worker.go L294-296). These are the only cases Go
    /// treats as pathological — a URL like `https://host/path/` still yields
    /// `path` because `filepath.Base` strips the trailing separator first.
    #[test]
    fn filename_from_response_uses_timestamped_default_when_url_has_no_basename()
    -> VideoServiceResult<()> {
        // Empty URL: Go filepath.Base("") = "." -> fallback (worker.go L294).
        assert_eq!(filename_from_response(None, "", 42), "video-42.mp4");
        // Bare root: Go filepath.Base("/") = "/" -> fallback (worker.go L294).
        assert_eq!(filename_from_response(None, "/", 7), "video-7.mp4");
        // Sanity check: a URL with a trailing slash still yields the preceding
        // segment (NOT the timestamped default) — Go strips the trailing
        // separator before taking the basename.
        assert_eq!(
            filename_from_response(None, "https://cdn.example.com/", 0),
            "cdn.example.com"
        );
        Ok(())
    }

    /// End-to-end: Content-Disposition takes precedence over the URL fallback,
    /// and the URL fallback takes precedence over the timestamped default
    /// (worker.go L276-298 evaluation order).
    #[test]
    fn filename_from_response_precedence_is_content_disposition_then_url_then_default()
    -> VideoServiceResult<()> {
        // 1. Content-Disposition wins over a valid URL basename.
        assert_eq!(
            filename_from_response(Some("filename=from_cd.mp4"), "https://x/from_url.mp4", 0),
            "from_cd.mp4"
        );
        // 2. Empty CD value -> URL basename wins.
        assert_eq!(
            filename_from_response(Some("filename="), "https://x/from_url.mp4", 0),
            "from_url.mp4"
        );
        // 3. No CD, pathological URL (empty basename) -> timestamped default.
        assert_eq!(filename_from_response(None, "/", 7), "video-7.mp4");
        Ok(())
    }

    /// Go: scan candidate `StatusIn(StatusProcessing, StatusCompleted)`
    /// (worker.go L139). Rust mapping: Running + Succeeded.
    #[test]
    fn passes_scan_status_filter_admits_only_in_flight_statuses() -> VideoServiceResult<()> {
        assert!(passes_scan_status_filter(RequestStatus::Running));
        assert!(passes_scan_status_filter(RequestStatus::Succeeded));
        // Terminal / pre-flight statuses are excluded.
        assert!(!passes_scan_status_filter(RequestStatus::Pending));
        assert!(!passes_scan_status_filter(RequestStatus::Failed));
        assert!(!passes_scan_status_filter(RequestStatus::Cancelled));
        Ok(())
    }

    /// Go: scan candidate `FormatIn("openai/video", "seedance/video")`
    /// (worker.go L140, constants.go L37+L53).
    #[test]
    fn passes_scan_format_filter_admits_only_video_formats() -> VideoServiceResult<()> {
        assert!(passes_scan_format_filter("openai/video"));
        assert!(passes_scan_format_filter("seedance/video"));
        // Non-video formats are excluded.
        assert!(!passes_scan_format_filter("openai/chat"));
        assert!(!passes_scan_format_filter("anthropic/messages"));
        assert!(!passes_scan_format_filter(""));
        Ok(())
    }

    /// Combined predicate: a row must pass BOTH the status and the format
    /// filter to be a scan candidate (worker.go L139-141 AND chain).
    #[test]
    fn scan_candidate_filters_compose_for_query() -> VideoServiceResult<()> {
        // Happy path: a Running request with openai/video format is selected.
        let happy = passes_scan_status_filter(RequestStatus::Running)
            && passes_scan_format_filter("openai/video");
        assert!(happy);

        // Wrong status: a Cancelled request is skipped even with a video format.
        let wrong_status = passes_scan_status_filter(RequestStatus::Cancelled)
            && passes_scan_format_filter("openai/video");
        assert!(!wrong_status);

        // Wrong format: a Running request with non-video format is skipped.
        let wrong_format = passes_scan_status_filter(RequestStatus::Running)
            && passes_scan_format_filter("openai/chat");
        assert!(!wrong_format);

        // Both wrong: skipped.
        let both_wrong = passes_scan_status_filter(RequestStatus::Cancelled)
            && passes_scan_format_filter("openai/chat");
        assert!(!both_wrong);
        Ok(())
    }

    /// Round-trip guard: the api-format strings the filter accepts are exactly
    /// those produced by `video_api_format_for_channel_type` (biz/video.go
    /// L145-150). A scan candidate's format is set by the request-row
    /// persist middleware from the same constant set, so the two helpers must
    /// agree on the canonical strings.
    #[test]
    fn scan_format_filter_matches_video_api_format_for_channel_type_output()
    -> VideoServiceResult<()> {
        let doubao = video_api_format_for_channel_type("doubao");
        let openai = video_api_format_for_channel_type("openai");
        assert!(passes_scan_format_filter(doubao));
        assert!(passes_scan_format_filter(openai));
        Ok(())
    }
}
