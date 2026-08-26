//! Doubao/Seedance video task transformer — pure-logic primitives for the
//! Seedance-compatible inbound + outbound video-task surface.
//!
//! Mirrors Go `conduit/llm/transformer/doubao/{video_inbound,video_outbound}.go`
//! plus `conduit/internal/server/api/doubao.go` and the route table at
//! `conduit/internal/server/routes.go` lines 179-181 (OpenAI `/v1/videos`) and
//! 208-211 (Doubao `/doubao/v3/contents/generations/tasks`).
//!
//! Implements four pure primitives:
//! - [`parse_doubao_route`]   — S04/S05/S09 route classifier (create/get/delete + native vs OpenAI-like)
//! - [`validate_size`]        — S06 size→ratio/resolution mapping with Go's exact error message
//! - [`map_task_status`]      — S10/S11 provider status → unified VideoTaskStatus
//! - [`DoubaoTaskView`] + [`to_unified_external_id`] — S09 external-id + task shape
//!
//! No I/O, no HTTP wiring. Full request/response body transformation (S07
//! outbound trait wiring, S08 persistence, S12 provider+local delete) are out
//! of scope here — `[Lovelace-the-3rd ?]`.

use conduit_core::ConduitError;
use serde::{Deserialize, Serialize};

use crate::TransformerResult;

// ---------------------------------------------------------------------------
// S04/S05/S09 — route classifier
// ---------------------------------------------------------------------------

/// Which Doubao/Seedance video-task route an inbound request targets.
///
/// Mirrors the Go route registration at `conduit/internal/server/routes.go`:
/// ```text
/// openaiGroup  := apiGroup.Group("/v1")                            // line 169
/// openaiGroup.POST  ("/videos",       handlers.OpenAI.CreateVideo) // line 179
/// openaiGroup.GET   ("/videos/:id",   handlers.OpenAI.GetVideo)    // line 180
/// openaiGroup.DELETE("/videos/:id",   handlers.OpenAI.DeleteVideo) // line 181
///
/// doubaoGroup := apiGroup.Group("/doubao/v3")                                  // line 208
/// doubaoGroup.POST  ("/contents/generations/tasks",    handlers.Doubao.CreateTask) // line 209
/// doubaoGroup.GET   ("/contents/generations/tasks/:id", handlers.Doubao.GetTask)    // line 210
/// doubaoGroup.DELETE("/contents/generations/tasks/:id", handlers.Doubao.DeleteTask) // line 211
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoubaoRoute {
    pub action: DoubaoRouteAction,
    /// `false` for the native Doubao `/doubao/v3/contents/generations/tasks`
    /// mount (served by `handlers.Doubao` in Go). `true` for the OpenAI-like
    /// `/v1/videos` mount (served by `handlers.OpenAI` — line 179-181 — which
    /// re-routes to a Seedance outbound when the channel is configured as
    /// Doubao/Seedance).
    pub openai_like: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoubaoRouteAction {
    Create,
    Get,
    Delete,
}

impl DoubaoRoute {
    fn new(action: DoubaoRouteAction, openai_like: bool) -> Self {
        Self {
            action,
            openai_like,
        }
    }
}

/// Classify an inbound `(method, path)` into a Doubao/Seedance video-task
/// route. Returns `None` for anything that is not one of the six Seedance
/// video-task entries, so the caller can fall through to other routers.
///
/// `path` is the absolute path (with optional `:id` / `{id}` tail already
/// substituted by the router); we accept either Gin-style `:id` placeholders
/// (as written in `routes.go`) or concrete ids.
pub fn parse_doubao_route(method: &str, path: &str) -> Option<DoubaoRoute> {
    let method = method.trim().to_ascii_uppercase();
    // Normalize a trailing placeholder (`:id` / `{id}`) to a single sentinel so
    // callers can pass either the route template or a real request path.
    let normalized = path.trim_end_matches('/');

    // Native Doubao mount: /doubao/v3/contents/generations/tasks[/:id]
    const NATIVE_BASE: &str = "/doubao/v3/contents/generations/tasks";
    // OpenAI-like mount: /v1/videos[/:id]
    const OAI_BASE: &str = "/v1/videos";

    let (base, tail) = split_id_tail(normalized);
    let openai_like = match base {
        OAI_BASE => true,
        NATIVE_BASE => false,
        _ => return None,
    };

    match (method.as_str(), tail) {
        // POST /<base> — create task
        ("POST", None) => Some(DoubaoRoute::new(DoubaoRouteAction::Create, openai_like)),
        // GET /<base>/:id — get task
        ("GET", Some(_)) => Some(DoubaoRoute::new(DoubaoRouteAction::Get, openai_like)),
        // DELETE /<base>/:id — delete task
        ("DELETE", Some(_)) => Some(DoubaoRoute::new(DoubaoRouteAction::Delete, openai_like)),
        _ => None,
    }
}

/// Split `path` into `(base, Some(id))` when it has a trailing id segment,
/// or `(path, None)` otherwise. Accepts both Gin placeholders (`:id`,
/// `*id`) and concrete ids.
fn split_id_tail(path: &str) -> (&str, Option<&str>) {
    // Strip a Gin-style wildcard/param placeholder or a concrete id after the
    // last `/`. Both `/tasks/:id` and `/videos/c1723...` reduce to the base.
    let last_slash = match path.rfind('/') {
        Some(idx) => idx,
        None => return (path, None),
    };
    let tail = &path[last_slash + 1..];

    // No tail (trailing slash already trimmed) → base only.
    if tail.is_empty() {
        return (path, None);
    }

    // If the tail is a Gin param/wildcard placeholder, drop it.
    if tail.starts_with(':') || tail.starts_with('*') {
        return (&path[..last_slash], Some(tail));
    }

    // For a concrete id we need to make sure the base is one of the known
    // collection paths. Otherwise the path is just a non-task URL that happens
    // to have a final segment (e.g. `/v1/models/gpt-4o`).
    let base = &path[..last_slash];
    if base == "/doubao/v3/contents/generations/tasks" || base == "/v1/videos" {
        (base, Some(tail))
    } else {
        (path, None)
    }
}

// ---------------------------------------------------------------------------
// S06 — size → ratio/resolution mapping
// ---------------------------------------------------------------------------

/// A validated Seedance aspect-ratio + resolution pair, derived from an
/// OpenAI-style `size` string (e.g. `"1280x720"` → `16:9` / `720p`).
///
/// Mirrors Go `inferSeedanceRatioResolution` at
/// `conduit/llm/transformer/doubao/video_outbound.go` lines 138-160.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AspectRatio {
    pub ratio: &'static str,
    pub resolution: &'static str,
}

/// The six size→ratio/resolution pairs Seedance accepts. Order matches the Go
/// `switch` arms exactly.
const KNOWN_SIZES: [(u32, u32, &str, &str); 6] = [
    (1280, 720, "16:9", "720p"),
    (720, 1280, "9:16", "720p"),
    (1920, 1080, "16:9", "1080p"),
    (1080, 1920, "9:16", "1080p"),
    (640, 480, "4:3", "480p"),
    (480, 640, "3:4", "480p"),
];

/// Validate an OpenAI-style `size` string and map it to a Seedance
/// `ratio`+`resolution` pair.
///
/// Mirrors Go `inferSeedanceRatioResolution` + `parseSize` at
/// `conduit/llm/transformer/doubao/video_outbound.go` lines 138-181. On
/// failure returns [`ConduitError::invalid_request`] with Go's exact wrapping
/// message (line 44):
/// ```text
/// size %q cannot be mapped to ratio/resolution, please set ratio and resolution
/// ```
/// where `%q` becomes the original `size` string.
pub fn validate_size(size: &str) -> TransformerResult<AspectRatio> {
    let (w, h) = parse_size(size).ok_or_else(|| size_mapping_error(size))?;

    for (kw, kh, ratio, resolution) in KNOWN_SIZES {
        if (w, h) == (kw, kh) {
            return Ok(AspectRatio { ratio, resolution });
        }
    }
    Err(size_mapping_error(size))
}

fn size_mapping_error(size: &str) -> ConduitError {
    ConduitError::invalid_request(format!(
        "size {size:?} cannot be mapped to ratio/resolution, please set ratio and resolution"
    ))
}

/// Parse an OpenAI-style `"WxH"` size. Mirrors Go `parseSize` (lines 162-181):
/// lowercases, trims, splits on `'x'`, requires both halves to be positive
/// integers.
fn parse_size(size: &str) -> Option<(u32, u32)> {
    let s = size.trim().to_ascii_lowercase();
    let (before, after) = s.split_once('x')?;
    let w: u32 = before.trim().parse().ok()?;
    let h: u32 = after.trim().parse().ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

// ---------------------------------------------------------------------------
// S10/S11 — status mapping
// ---------------------------------------------------------------------------

/// Unified video-task status, mirroring the `request.Status` enum the Go biz
/// layer (`internal/server/biz/video.go` lines 165-176) derives from the
/// Seedance provider status, plus the explicit `Canceled` terminal the
/// `DeleteTask` path can leave behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoTaskStatus {
    Pending,
    Processing,
    Completed,
    Failed,
    Canceled,
}

impl VideoTaskStatus {
    /// The lowercase string token the unified `VideoResponse.Status` carries
    /// (Go `llm/video.go` line 74 documents the four Seedance values
    /// `queued`/`running`/`succeeded`/`failed`).
    pub fn as_provider_str(self) -> &'static str {
        match self {
            Self::Pending => "queued",
            Self::Processing => "running",
            Self::Completed => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

/// Map a Seedance provider status string to the unified
/// [`VideoTaskStatus`].
///
/// Mirrors two Go sites together:
/// - `video_outbound.go` `ParseGetVideoTaskResponse` (lines 244-247): trims +
///   lowercases the provider status; an empty string defaults to `"queued"`.
/// - `biz/video.go` `mapVideoStatusToRequestStatus` (lines 165-176):
///   `succeeded`→Completed, `failed`→Failed, `queued`/`running`/default→
///   Processing.
///
/// `Canceled` is never produced by the Seedance provider; it is the local
/// state the Go `DeleteTask` path leaves the request row in (see
/// `biz/video.go` line 112). Callers pass `"canceled"` explicitly to record
/// that terminal.
pub fn map_task_status(provider_status: &str) -> VideoTaskStatus {
    let s = provider_status.trim().to_ascii_lowercase();
    match s.as_str() {
        "" | "queued" => VideoTaskStatus::Pending,
        "running" => VideoTaskStatus::Processing,
        "succeeded" => VideoTaskStatus::Completed,
        "failed" => VideoTaskStatus::Failed,
        "canceled" | "cancelled" => VideoTaskStatus::Canceled,
        // Go biz default arm returns StatusProcessing for any unrecognized
        // non-empty provider status (biz/video.go line 174).
        _ => VideoTaskStatus::Processing,
    }
}

// ---------------------------------------------------------------------------
// S09 — DoubaoTaskView + external id
// ---------------------------------------------------------------------------

/// Typed view of a Doubao/Seedance task as seen on the create/get/get-by-
/// external-id surface. Mirrors the Go `seedanceCreateResponse` (create →
/// `{id}`) and `seedanceGetResponse`/`seedanceGetResponseInbound` shapes
/// (`video_outbound.go` lines 18-20 & 205-227, `video_inbound.go` lines
/// 119-141).
///
/// `content_saved` is not a Seedance field — it is the local Request row flag
/// the Go `VideoService.GetTaskByExternalID` reads to decide whether the task
/// body was persisted. We carry it here so the Rust handler can do the same
/// association without a second struct (S09 requirement).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoubaoTaskView {
    /// Seedance task id (provider-side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Seedance nests the video URL under `content.video_url` (Go
    /// `video_outbound.go` lines 210-212).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<DoubaoTaskContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<DoubaoTaskUsage>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    /// Whether the local Request row backing this external id still has its
    /// task body saved (S09). Not serialized to the wire.
    #[serde(default, skip)]
    pub content_saved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoubaoTaskContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoubaoTaskUsage {
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

/// Build the unified external-id string the Go handlers thread through
/// `/doubao/v3/contents/generations/tasks/:id` and `/v1/videos/:id`
/// (`internal/server/api/doubao.go` lines 108-114 and
/// `internal/server/api/openai.go` lines 424-430) into
/// `VideoService.GetTaskByExternalID`.
///
/// The Go handler passes `c.Param("id")` through unchanged; the
/// `external_id` is exactly the Seedance task id echoed back on create
/// (`seedanceCreateResponse.ID`). So the unified form is the task id itself,
/// surfaced here as an explicit function so callers don't have to grep for
/// the convention.
pub fn to_unified_external_id(task: &DoubaoTaskView) -> String {
    task.id.clone().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// S07 — provider task URL builders (BuildGetVideoTaskRequest /
//       BuildDeleteVideoTaskRequest in video_outbound.go)
// ---------------------------------------------------------------------------

/// Build the `baseURL + "/contents/generations/tasks/<id>"` URL used by the
/// Seedance get/delete video-task endpoints.
///
/// Mirrors Go `(*OutboundTransformer).BuildGetVideoTaskRequest`
/// (`conduit/llm/transformer/doubao/video_outbound.go:184-200`) and
/// `BuildDeleteVideoTaskRequest` (`:296-312`): both concatenate
/// `t.BaseURL + "/contents/generations/tasks/" + providerTaskID` and reject an
/// empty `providerTaskID` with `transformer.ErrInvalidRequest`.
///
/// A trailing slash on `base_url` is normalized away so callers can pass
/// either `"https://ark.cn-beijing.volces.com/api/v3"` or the same with a
/// trailing `/` and get the identical URL. (Go's plain string concat would
/// yield a double slash for the latter; the channel table never stores a
/// trailing slash, so this is a strict superset of Go behavior and never
/// diverges on the happy path.)
pub fn video_task_url(base_url: &str, task_id: &str) -> TransformerResult<String> {
    if task_id.trim().is_empty() {
        // Go: `fmt.Errorf("%w: providerTaskID is required", transformer.ErrInvalidRequest)`
        // (video_outbound.go:186-187 and :298-299).
        return Err(ConduitError::invalid_request("providerTaskID is required"));
    }
    let trimmed_base = base_url.trim_end_matches('/');
    Ok(format!(
        "{base}/contents/generations/tasks/{id}",
        base = trimmed_base,
        id = task_id
    ))
}

/// Thin wrapper over [`video_task_url`] for the GET endpoint.
///
/// Mirrors Go `BuildGetVideoTaskRequest` (video_outbound.go:184-200).
pub fn build_get_video_task_url(base_url: &str, task_id: &str) -> TransformerResult<String> {
    video_task_url(base_url, task_id)
}

/// Thin wrapper over [`video_task_url`] for the DELETE endpoint.
///
/// Mirrors Go `BuildDeleteVideoTaskRequest` (video_outbound.go:296-312).
pub fn build_delete_video_task_url(base_url: &str, task_id: &str) -> TransformerResult<String> {
    video_task_url(base_url, task_id)
}

// ---------------------------------------------------------------------------
// S05 — Seedance inbound request body normalization + response shaping
//       (TransformRequest / TransformResponse in video_inbound.go)
// ---------------------------------------------------------------------------

/// Typed view of the Seedance native create-task request body, mirroring Go
/// `seedanceCreateRequest` (`conduit/llm/transformer/doubao/video_inbound.go:33-49`).
///
/// Field names are the Seedance snake_case wire tags (the Go struct uses
/// camelCase json tags that, by coincidence, match snake_case here because
/// none of these names have word boundaries). All optional fields carry Go's
/// `omitempty` semantics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedanceCreateRequest {
    // `default` so a missing `model` survives deserialization and is then
    // rejected by `normalize_seedance_create_request` with Go's exact error
    // message (video_inbound.go:71-73), instead of a serde "missing field".
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub content: Vec<SeedanceVideoContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate_audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_fixed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_expires_after: Option<i64>,
}

/// `content[]` entry, mirroring Go `llm.VideoContent`
/// (`conduit/llm/video.go:51-65`). `image_url` is nested; `role` is the
/// Seedance frame-role (`first_frame`/`last_frame`/`reference_image`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedanceVideoContent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<SeedanceVideoImage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedanceVideoImage {
    pub url: String,
}

/// The post-create, pre-task-completion unified request view produced by the
/// inbound transformer. Because the unified `llm.Request`/`llm.VideoRequest`
/// Rust types do not yet exist (the Go `llm.VideoRequest` struct at
/// `conduit/llm/video.go:6-49` has no Rust counterpart), this struct carries
/// exactly the fields Go's `TransformRequest` (`video_inbound.go:51-107`)
/// forwards, plus the duration-to-string conversion Go does at line 92-95.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeedanceUnifiedRequest {
    pub model: String,
    pub content: Vec<SeedanceVideoContent>,
    /// Seedance sends duration as an int64; Go converts it to a string
    /// (`video_inbound.go:92-95`). We preserve that string form here.
    pub duration: Option<String>,
    pub ratio: Option<String>,
    pub resolution: Option<String>,
    pub frames: Option<i64>,
    pub seed: Option<i64>,
    pub generate_audio: Option<bool>,
    pub camera_fixed: Option<bool>,
    pub watermark: Option<bool>,
    pub draft: Option<bool>,
    pub service_tier: Option<String>,
    pub execution_expires_after: Option<i64>,
}

/// Validate and normalize a Seedance native create-task request body.
///
/// Mirrors Go `(*VideoInboundTransformer).TransformRequest`
/// (`conduit/llm/transformer/doubao/video_inbound.go:51-107`):
/// - require non-empty `model` (line 71-73)
/// - require non-empty `content` (line 75-77)
/// - convert `duration` int64 → string (line 92-95)
/// - forward all other optional fields verbatim
///
/// HTTP-level guards performed by Go (nil request, empty body, content-type
/// check at lines 53-69) are handled by the HTTP layer in the Rust port and
/// are intentionally NOT re-checked here.
pub fn normalize_seedance_create_request(
    req: SeedanceCreateRequest,
) -> TransformerResult<SeedanceUnifiedRequest> {
    if req.model.trim().is_empty() {
        return Err(ConduitError::invalid_request("model is required"));
    }
    if req.content.is_empty() {
        return Err(ConduitError::invalid_request("content is required"));
    }

    Ok(SeedanceUnifiedRequest {
        model: req.model,
        content: req.content,
        // Go: `strconv.FormatInt(*req.Duration, 10)` (video_inbound.go:94).
        duration: req.duration.map(|d| d.to_string()),
        ratio: req.ratio,
        resolution: req.resolution,
        frames: req.frames,
        seed: req.seed,
        generate_audio: req.generate_audio,
        camera_fixed: req.camera_fixed,
        watermark: req.watermark,
        draft: req.draft,
        service_tier: req.service_tier,
        execution_expires_after: req.execution_expires_after,
    })
}

/// Typed view of the create-task response body, mirroring Go
/// `seedanceCreateResponseInbound` (`video_inbound.go:115-117`): just `{id}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedanceCreateResponse {
    pub id: String,
}

/// Typed view of the get-task response body, mirroring Go
/// `seedanceGetResponseInbound` (`video_inbound.go:119-141`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedanceGetResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<SeedanceGetContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<SeedanceGetUsage>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<i64>,
    /// Go tag is `framespersecond` (one word, no underscore) — must rename
    /// explicitly; `rename_all="snake_case"` would mis-convert `fps`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "framespersecond"
    )]
    pub frames_per_second: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedanceGetContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedanceGetUsage {
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
}

/// Unified video response view produced by the inbound transformer. Mirrors
/// the subset of Go `llm.Response{Video: &llm.VideoResponse{...}}` fields
/// (`conduit/llm/video.go:71-103`) that Go's `TransformResponse`
/// (`video_inbound.go:143-218`) reads. The unified `llm.Response` Rust type
/// does not yet exist, so this carries just what is needed for shaping.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SeedanceUnifiedResponse {
    /// Go `Response.Object`. When `"video.create"`, `TransformResponse`
    /// emits the minimal `{"id": ...}` create-ack body (video_inbound.go:151).
    pub object: String,
    pub id: String,
    pub status: String,
    pub model: String,
    pub created_at: i64,
    pub completed_at: i64,
    pub ratio: String,
    pub resolution: String,
    pub fps: Option<i64>,
    pub seed: Option<i64>,
    /// Duration as a string (the form Go carries it in `llm.VideoResponse`).
    pub duration: Option<String>,
    pub video_url: String,
    pub usage: Option<SeedanceGetUsage>,
}

/// Shape a create-task response body `{"id": ...}`.
///
/// Mirrors Go `TransformResponse` for the `object == "video.create"` arm
/// (`video_inbound.go:149-162`).
pub fn shape_seedance_create_response(
    resp: &SeedanceUnifiedResponse,
) -> TransformerResult<Vec<u8>> {
    let body = SeedanceCreateResponse {
        id: resp.id.clone(),
    };
    serde_json::to_vec(&body).map_err(|e| {
        ConduitError::internal(format!("failed to marshal seedance create response: {e}"))
    })
}

/// Mirror Go's `math.Round` (round half away from zero) for the duration
/// string→int64 conversion at `video_inbound.go:185-189`. Go's `math.Round`
/// rounds ties away from zero (`0.5 → 1`, `-0.5 → -1`), unlike Rust's `f64::round`.
fn go_math_round(x: f64) -> i64 {
    // Go: `math.Round(x)` returns the nearest integer, rounding half away from
    // zero. Rust's `f64::round()` already rounds half away from zero, matching
    // Go's behavior for finite inputs.
    x.round() as i64
}

/// Shape a get-task response body, mirroring Go `TransformResponse` for the
/// get arm (`video_inbound.go:164-218`).
///
/// Field mapping follows the Go code exactly:
/// - `updated_at` = `completed_at` if non-zero, else `time.Now().Unix()` —
///   callers should pass a non-zero `now` to keep the test deterministic
///   (video_inbound.go:172).
/// - `duration` is parsed from the string form back to int64 seconds, matching
///   Go `strconv.ParseFloat` + `math.Round` (video_inbound.go:185-189).
/// - `service_tier` is intentionally blanked (video_inbound.go:170).
pub fn shape_seedance_get_response(
    resp: &SeedanceUnifiedResponse,
    now_unix: i64,
) -> TransformerResult<Vec<u8>> {
    let duration_int = resp.duration.as_ref().and_then(|d| {
        let trimmed = d.trim();
        trimmed.parse::<f64>().ok().map(go_math_round)
    });

    let body = SeedanceGetResponse {
        id: Some(resp.id.clone()),
        model: if resp.model.is_empty() {
            None
        } else {
            Some(resp.model.clone())
        },
        status: if resp.status.is_empty() {
            None
        } else {
            Some(resp.status.clone())
        },
        content: if resp.video_url.is_empty() {
            None
        } else {
            Some(SeedanceGetContent {
                video_url: Some(resp.video_url.clone()),
            })
        },
        usage: resp.usage.clone(),
        created_at: resp.created_at,
        // Go: `lo.Ternary(v.CompletedAt != 0, v.CompletedAt, time.Now().Unix())`
        // (video_inbound.go:172).
        updated_at: if resp.completed_at != 0 {
            resp.completed_at
        } else {
            now_unix
        },
        seed: resp.seed,
        resolution: if resp.resolution.is_empty() {
            None
        } else {
            Some(resp.resolution.clone())
        },
        ratio: if resp.ratio.is_empty() {
            None
        } else {
            Some(resp.ratio.clone())
        },
        duration: duration_int,
        frames_per_second: resp.fps,
        // Go hard-codes ServiceTier to "" (video_inbound.go:170).
        service_tier: None,
    };

    serde_json::to_vec(&body).map_err(|e| {
        ConduitError::internal(format!("failed to marshal seedance get response: {e}"))
    })
}

// ---------------------------------------------------------------------------
// RUST-P7-008 S14 — Doubao provider-specific image helpers
// (conduit/llm/transformer/doubao/outbound.go:189-302)
//
// Three pure-logic primitives the Go `buildImageGenerationAPIRequest`
// constructor uses to shape the Doubao `/images/generations` request body.
// Doubao is unusual: it routes BOTH image generation AND image edit through
// the same `/images/generations` endpoint (Go outbound.go:189-191), encoding
// any input images as base64 data-URLs under the `image` field rather than
// multipart upload. These helpers capture the provider-specific quirks
// (data-URL encoding, quality→guidance_scale mapping, body shape) so the
// future Doubao outbound port can compose them without re-reading the Go
// source.
// ---------------------------------------------------------------------------

/// Encode raw image bytes as a `data:` URL, mirroring Go's
/// `encodeImageBytesToDataURL` (outbound.go:291-302).
///
/// Go sniffs the MIME via `http.DetectContentType` and falls back to
/// `image/png` when the sniff does not yield an `image/*` type. The Rust
/// side mirrors that exactly: we re-implement the same 512-byte sniff
/// signature check `net/http.DetectContentType` uses for the common image
/// types, then base64-encode the bytes into the
/// `data:<media-type>;base64,<payload>` form Go's `xurl.BuildDataURL`
/// produces.
///
/// Empty input returns the empty string (Go: `if len(b) == 0 { return "" }`).
pub fn encode_image_bytes_to_data_url(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let media_type = sniff_image_media_type(bytes);
    let encoded = base64_std_encode(bytes);
    format!("data:{media_type};base64,{encoded}")
}

/// Sniff the MIME media type of image bytes, falling back to `image/png`
/// when the sniff does not yield an `image/*` type. Mirrors Go's
/// `http.DetectContentType(b)` followed by the `strings.HasPrefix(...,
/// "image/")` check at outbound.go:297-299.
///
/// The signatures here cover the cases `net/http.DetectContentType`
/// classifies as image/* in its `sniffSig` table
/// (https://cs.opensource.google/go/go/+/refs/tags/go1.26:src/net/http/sniff.go):
/// PNG, JPEG, GIF, WebP, BMP, ICO, TIFF. Anything else falls through to the
/// `image/png` default Go stamps at outbound.go:299.
fn sniff_image_media_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "image/png";
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if bytes.starts_with(b"RIFF") && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    if bytes.starts_with(b"BM") {
        return "image/bmp";
    }
    if bytes.len() >= 6 && &bytes[2..6] == b"\x00\x00\x01\x00" {
        return "image/x-icon";
    }
    if bytes.starts_with(b"II*\x00") || bytes.starts_with(b"MM\x00*") {
        return "image/tiff";
    }
    "image/png"
}

/// Base64-encode bytes using the standard alphabet with padding, mirroring
/// Go's `base64.StdEncoding.EncodeToString`.
fn base64_std_encode(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        let n = (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | (chunk[2] as u32);
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHA[(n & 0x3F) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = (rem[0] as u32) << 16 | (rem[1] as u32) << 8;
            out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

/// Map an OpenAI-style image-request `quality` string ("hd" / "standard",
/// optionally with any case) to Doubao's provider-specific
/// `guidance_scale` value. Mirrors Go's switch at outbound.go:235-240:
///
/// ```text
/// switch llmReq.Image.Quality {
/// case "hd":       reqBody["guidance_scale"] = 7.5
/// case "standard": reqBody["guidance_scale"] = 2.5
/// }
/// ```
///
/// Returns `None` for empty / unrecognized quality values — matching Go's
/// switch which silently omits the field in that case (no default branch).
pub fn doubao_guidance_scale_for_quality(quality: &str) -> Option<f64> {
    match quality {
        "hd" => Some(7.5_f64),
        "standard" => Some(2.5_f64),
        _ => None,
    }
}

/// Build the JSON body for Doubao's `/images/generations` endpoint, mirroring
/// Go's `buildImageGenerationAPIRequest` body assembly (outbound.go:210-254).
///
/// Doubao reuses `/images/generations` for BOTH generation and edit: input
/// images (when present for edit) are encoded as base64 data-URLs and placed
/// under the `image` field — single image becomes a string, multiple images
/// become an array (Go outbound.go:218-225). The body always carries
/// `response_format: "b64_json"` and `stream: false` defaults; `n`, `size`,
/// `response_format` (override), and `user` are optional and only included
/// when set, matching Go's `if ... { reqBody[...] = ... }` shape.
///
/// This helper produces the canonical JSON value; the surrounding outbound
/// transformer is responsible for HTTP wiring (URL, auth, headers).
//
// The 8-parameter signature mirrors Go's `*llm.Request` field set the body
// is built from; collapsing it into a config struct would diverge from the
// Go shape we're holding parity with.
#[allow(clippy::too_many_arguments)]
pub fn build_doubao_image_request_body(
    model: &str,
    prompt: &str,
    images_base64_data_urls: &[String],
    n: Option<i64>,
    size: Option<&str>,
    quality: &str,
    response_format_override: Option<&str>,
    user: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "response_format": "b64_json",
        "stream": false,
    });

    // Go outbound.go:218-225: single image -> string, multiple -> array.
    match images_base64_data_urls {
        [] => {}
        [single] => {
            body["image"] = serde_json::Value::String(single.clone());
        }
        many => {
            body["image"] = serde_json::Value::Array(
                many.iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            );
        }
    }

    if let Some(n_val) = n {
        body["n"] = serde_json::Value::from(n_val);
    }
    if let Some(size_val) = size.filter(|s| !s.is_empty()) {
        body["size"] = serde_json::Value::String(size_val.to_string());
    }
    if let Some(gs) = doubao_guidance_scale_for_quality(quality) {
        body["guidance_scale"] = serde_json::json!(gs);
    }
    if let Some(rf) = response_format_override.filter(|s| !s.is_empty()) {
        body["response_format"] = serde_json::Value::String(rf.to_string());
    }
    if let Some(user_val) = user.filter(|s| !s.is_empty()) {
        body["user"] = serde_json::Value::String(user_val.to_string());
    }

    body
}

// ---------------------------------------------------------------------------
// RUST-P15-001 — Doubao outbound chat/image URL builders + metadata helpers
// (conduit/llm/transformer/doubao/outbound.go:102-188, 191-289)
//
// Pure-logic primitives the Go OutboundTransformer.TransformRequest uses to
// build the /chat/completions and /images/generations request URLs, extract
// user_id/request_id from the request metadata, auto-generate request IDs,
// and validate chat request fields. The full HTTP wiring (auth, headers,
// openai.RequestFromLLM, body marshaling, OutboundTransformer struct) is out
// of scope for this pure-logic module — [Lovelace-the-3rd ?].
// ---------------------------------------------------------------------------

/// Build the `baseURL + "/chat/completions"` URL used by the Doubao chat
/// endpoint.
///
/// Mirrors Go outbound.go:177 (`url := t.BaseURL + "/chat/completions"`).
/// A trailing slash on `base_url` is normalized away, matching what Go's
/// `NormalizeBaseURL` (transformer/url.go:16-44) does before the transformer
/// is constructed (outbound.go:66).
pub fn build_chat_completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    format!("{trimmed}/chat/completions")
}

/// Build the `baseURL + "/images/generations"` URL used by the Doubao image
/// endpoint (shared by both generation and editing).
///
/// Mirrors Go outbound.go:260 (`url := t.BaseURL + "/images/generations"`).
/// A trailing slash on `base_url` is normalized away, matching what Go's
/// `NormalizeBaseURL` does before the transformer is constructed.
pub fn build_image_generations_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    format!("{trimmed}/images/generations")
}

/// The user_id and request_id extracted from an `llm.Request.Metadata` map,
/// mirroring Go outbound.go:139-157. `Metadata` is cleared after extraction
/// (the Go code sets `doubaoReq.Metadata = nil` at line 157); callers should
/// not re-emit the metadata map on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoubaoChatMetadata {
    pub user_id: String,
    pub request_id: String,
}

/// Format the auto-generated request_id string.
///
/// Mirrors Go outbound.go:153: `fmt.Sprintf("req_%d", time.Now().Unix())`.
pub fn format_doubao_request_id(now_unix: i64) -> String {
    format!("req_{now_unix}")
}

/// Extract user_id and request_id from the request metadata map, auto-
/// generating a request_id when none is provided.
///
/// Mirrors Go outbound.go:145-154. The caller passes the current Unix
/// timestamp for deterministic request_id generation.
pub fn extract_doubao_chat_metadata(
    metadata: Option<&std::collections::HashMap<String, String>>,
    now_unix: i64,
) -> DoubaoChatMetadata {
    let (user_id, request_id) = match metadata {
        Some(m) => (
            m.get("user_id").cloned().unwrap_or_default(),
            m.get("request_id").cloned().unwrap_or_default(),
        ),
        None => (String::new(), String::new()),
    };
    let request_id = if request_id.is_empty() {
        format_doubao_request_id(now_unix)
    } else {
        request_id
    };
    DoubaoChatMetadata {
        user_id,
        request_id,
    }
}

/// Validate the chat request fields, returning Go's exact error messages.
///
/// Mirrors Go outbound.go:111 and 131-133:
/// ```text
/// if llmReq.Model == "" {
///     return nil, fmt.Errorf("%w: model is required", transformer.ErrInvalidRequest)
/// }
/// if len(llmReq.Messages) == 0 {
///     return nil, fmt.Errorf("%w: messages are required", transformer.ErrInvalidRequest)
/// }
/// ```
pub fn validate_doubao_chat_fields(model: &str, messages_len: usize) -> TransformerResult<()> {
    if model.is_empty() {
        return Err(ConduitError::invalid_request("model is required"));
    }
    if messages_len == 0 {
        return Err(ConduitError::invalid_request("messages are required"));
    }
    Ok(())
}

/// The error message Go's `TransformRequest` returns for a nil request
/// (outbound.go:107). Unlike the model/messages errors, this is NOT wrapped
/// with `transformer.ErrInvalidRequest` — it's a plain `fmt.Errorf`.
pub const CHAT_REQUEST_NIL_ERROR: &str = "chat completion request is nil";

#[cfg(test)]
mod tests {
    use super::*;

    // ---- S04/S05/S09 parse_doubao_route --------------------------------

    #[test]
    fn parse_route_native_create_post() {
        let r = parse_doubao_route("POST", "/doubao/v3/contents/generations/tasks");
        assert!(matches!(
            r,
            Some(DoubaoRoute {
                action: DoubaoRouteAction::Create,
                openai_like: false
            })
        ));
    }

    #[test]
    fn parse_route_native_get_with_concrete_id() {
        let r = parse_doubao_route("GET", "/doubao/v3/contents/generations/tasks/c1723abc");
        assert!(matches!(
            r,
            Some(DoubaoRoute {
                action: DoubaoRouteAction::Get,
                openai_like: false
            })
        ));
    }

    #[test]
    fn parse_route_native_delete_with_gin_placeholder() {
        let r = parse_doubao_route("DELETE", "/doubao/v3/contents/generations/tasks/:id");
        assert!(matches!(
            r,
            Some(DoubaoRoute {
                action: DoubaoRouteAction::Delete,
                openai_like: false
            })
        ));
    }

    #[test]
    fn parse_route_openai_like_create() {
        let r = parse_doubao_route("POST", "/v1/videos");
        assert!(matches!(
            r,
            Some(DoubaoRoute {
                action: DoubaoRouteAction::Create,
                openai_like: true
            })
        ));
    }

    #[test]
    fn parse_route_openai_like_get_and_delete() {
        let get = parse_doubao_route("GET", "/v1/videos/v_42");
        assert!(matches!(
            get,
            Some(DoubaoRoute {
                action: DoubaoRouteAction::Get,
                openai_like: true
            })
        ));
        let del = parse_doubao_route("DELETE", "/v1/videos/v_42");
        assert!(matches!(
            del,
            Some(DoubaoRoute {
                action: DoubaoRouteAction::Delete,
                openai_like: true
            })
        ));
    }

    #[test]
    fn parse_route_rejects_non_task_paths_and_wrong_methods() {
        // Non-task URL with a final segment — must NOT be classified as a task
        // get/delete.
        assert!(parse_doubao_route("GET", "/v1/models/gpt-4o").is_none());
        // Wrong method on a known collection path.
        assert!(parse_doubao_route("PUT", "/v1/videos").is_none());
        // GET without an id on the collection path is not a defined route.
        assert!(parse_doubao_route("GET", "/v1/videos").is_none());
        // Completely unrelated path.
        assert!(parse_doubao_route("POST", "/v1/chat/completions").is_none());
    }

    // ---- S06 validate_size ---------------------------------------------

    #[test]
    fn validate_size_maps_all_six_known_pairs() -> TransformerResult<()> {
        assert_eq!(
            validate_size("1280x720")?,
            AspectRatio {
                ratio: "16:9",
                resolution: "720p"
            }
        );
        assert_eq!(
            validate_size("720x1280")?,
            AspectRatio {
                ratio: "9:16",
                resolution: "720p"
            }
        );
        assert_eq!(
            validate_size("1920x1080")?,
            AspectRatio {
                ratio: "16:9",
                resolution: "1080p"
            }
        );
        assert_eq!(
            validate_size("1080x1920")?,
            AspectRatio {
                ratio: "9:16",
                resolution: "1080p"
            }
        );
        assert_eq!(
            validate_size("640x480")?,
            AspectRatio {
                ratio: "4:3",
                resolution: "480p"
            }
        );
        assert_eq!(
            validate_size("480x640")?,
            AspectRatio {
                ratio: "3:4",
                resolution: "480p"
            }
        );
        Ok(())
    }

    #[test]
    fn validate_size_case_insensitive_and_trimmed() -> TransformerResult<()> {
        assert_eq!(
            validate_size("  1280X720  ")?,
            AspectRatio {
                ratio: "16:9",
                resolution: "720p"
            }
        );
        Ok(())
    }

    #[test]
    fn validate_size_unknown_pair_returns_go_error_message() -> TransformerResult<()> {
        match validate_size("1024x1024") {
            Err(err) => {
                // Go's exact wrapping message (video_outbound.go line 44).
                assert_eq!(
                    err.message,
                    r#"size "1024x1024" cannot be mapped to ratio/resolution, please set ratio and resolution"#
                );
            }
            Ok(ar) => {
                return Err(ConduitError::internal(format!(
                    "expected error for 1024x1024, got {ar:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn validate_size_malformed_returns_go_error_message() -> TransformerResult<()> {
        match validate_size("not-a-size") {
            Err(err) => {
                assert_eq!(
                    err.message,
                    r#"size "not-a-size" cannot be mapped to ratio/resolution, please set ratio and resolution"#
                );
            }
            Ok(ar) => {
                return Err(ConduitError::internal(format!(
                    "expected error for not-a-size, got {ar:?}"
                )));
            }
        }
        Ok(())
    }

    // ---- S10/S11 map_task_status ---------------------------------------

    #[test]
    fn map_status_empty_defaults_to_pending_like_go_default_queued() {
        assert_eq!(map_task_status(""), VideoTaskStatus::Pending);
        assert_eq!(map_task_status("queued"), VideoTaskStatus::Pending);
    }

    #[test]
    fn map_status_running_to_processing() {
        assert_eq!(map_task_status("running"), VideoTaskStatus::Processing);
        assert_eq!(map_task_status("RUNNING"), VideoTaskStatus::Processing);
    }

    #[test]
    fn map_status_succeeded_to_completed() {
        // Go `biz/video.go:167` only treats `"succeeded"` as Completed.
        assert_eq!(map_task_status("succeeded"), VideoTaskStatus::Completed);
        // `"completed"` is NOT a Go provider terminal — it falls through to
        // the biz default arm (Processing), matching `biz/video.go:174`.
        assert_eq!(map_task_status("completed"), VideoTaskStatus::Processing);
    }

    #[test]
    fn map_status_failed_and_canceled_terminals() {
        assert_eq!(map_task_status("failed"), VideoTaskStatus::Failed);
        assert_eq!(map_task_status("canceled"), VideoTaskStatus::Canceled);
        assert_eq!(map_task_status("cancelled"), VideoTaskStatus::Canceled);
    }

    #[test]
    fn map_status_unknown_falls_back_to_processing_like_go_biz_default() {
        // biz/video.go line 174 default arm.
        assert_eq!(map_task_status("weird-state"), VideoTaskStatus::Processing);
    }

    // ---- S09 DoubaoTaskView + to_unified_external_id -------------------

    #[test]
    fn external_id_is_the_seedance_task_id() {
        let task = DoubaoTaskView {
            id: Some("c1723abc".to_string()),
            model: None,
            status: Some("queued".to_string()),
            content: None,
            usage: None,
            created_at: 0,
            updated_at: 0,
            content_saved: true,
        };
        assert_eq!(to_unified_external_id(&task), "c1723abc");
    }

    #[test]
    fn external_id_empty_when_task_has_no_id() {
        let task = DoubaoTaskView {
            id: None,
            model: None,
            status: None,
            content: None,
            usage: None,
            created_at: 0,
            updated_at: 0,
            content_saved: false,
        };
        assert_eq!(to_unified_external_id(&task), "");
    }

    #[test]
    fn task_view_serializes_with_seedance_camel_case_and_nested_video_url()
    -> Result<(), Box<dyn std::error::Error>> {
        let task = DoubaoTaskView {
            id: Some("t1".to_string()),
            model: Some("seedance-1-0".to_string()),
            status: Some("succeeded".to_string()),
            content: Some(DoubaoTaskContent {
                video_url: Some("https://example.com/v.mp4".to_string()),
            }),
            usage: Some(DoubaoTaskUsage {
                completion_tokens: 100,
                total_tokens: 120,
            }),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_100,
            // Not serialized (skip).
            content_saved: true,
        };
        let json = serde_json::to_value(&task)?;
        // Go tags are all snake_case (video_outbound.go lines 206-226):
        // `id`, `created_at`, `updated_at`, `content.video_url`.
        assert_eq!(json["id"], "t1");
        assert_eq!(json["created_at"], 1_700_000_000);
        assert_eq!(json["updated_at"], 1_700_000_100);
        assert_eq!(json["content"]["video_url"], "https://example.com/v.mp4");
        // content_saved must NOT appear on the wire.
        assert!(json.get("content_saved").is_none());
        assert!(json.get("contentSaved").is_none());
        Ok(())
    }

    // ---- S07 video_task_url / build_get / build_delete ----------------

    #[test]
    fn video_task_url_happy_path() -> TransformerResult<()> {
        let u = video_task_url("https://ark.cn-beijing.volces.com/api/v3", "c1723abc")?;
        assert_eq!(
            u,
            "https://ark.cn-beijing.volces.com/api/v3/contents/generations/tasks/c1723abc"
        );
        Ok(())
    }

    #[test]
    fn video_task_url_normalizes_trailing_slash_on_base() -> TransformerResult<()> {
        // A trailing slash on the base must not yield a double slash. Go's raw
        // string concat would produce one; we normalize to the same shape the
        // happy path produces (strict superset of Go behavior; never diverges
        // on the happy path where the channel table stores no trailing slash).
        let with_slash = video_task_url("https://ark.example/api/v3/", "t1")?;
        let without_slash = video_task_url("https://ark.example/api/v3", "t1")?;
        assert_eq!(with_slash, without_slash);
        assert!(with_slash.contains("/api/v3/contents/generations/tasks/t1"));
        assert!(!with_slash.contains("v3//contents"));
        Ok(())
    }

    #[test]
    fn video_task_url_rejects_empty_task_id() {
        match video_task_url("https://ark.example/api/v3", "") {
            Err(err) => {
                // Go: `providerTaskID is required` (video_outbound.go:186).
                assert_eq!(err.message, "providerTaskID is required");
            }
            Ok(u) => panic!("expected error, got url {u}"),
        }
    }

    #[test]
    fn video_task_url_rejects_whitespace_only_task_id() {
        match video_task_url("https://ark.example/api/v3", "   ") {
            Err(err) => assert_eq!(err.message, "providerTaskID is required"),
            Ok(u) => panic!("expected error, got url {u}"),
        }
    }

    #[test]
    fn build_get_and_build_delete_share_url_shape() -> TransformerResult<()> {
        // Go's BuildGet and BuildDelete use the identical URL builder
        // (video_outbound.go:195 & :307) — only the HTTP method differs, which
        // the URL builder does not encode.
        let g = build_get_video_task_url("https://x", "id1")?;
        let d = build_delete_video_task_url("https://x", "id1")?;
        assert_eq!(g, d);
        Ok(())
    }

    // ---- S05 normalize_seedance_create_request ------------------------

    #[test]
    fn seedance_create_request_parses_full_body_and_converts_duration()
    -> Result<(), Box<dyn std::error::Error>> {
        let raw = serde_json::json!({
            "model": "seedance-1-0-pro-250528",
            "content": [
                {"type": "text", "text": "a cat walking"},
                {"type": "image_url", "image_url": {"url": "https://e/i.png"}, "role": "first_frame"}
            ],
            "duration": 5,
            "ratio": "16:9",
            "resolution": "1080p",
            "frames": 120,
            "seed": 42,
            "generate_audio": true,
            "camera_fixed": false,
            "watermark": false,
            "draft": false,
            "service_tier": "default",
            "execution_expires_after": 3600
        });
        let req: SeedanceCreateRequest = serde_json::from_value(raw)?;

        // model required + content required.
        let unified = normalize_seedance_create_request(req)?;

        assert_eq!(unified.model, "seedance-1-0-pro-250528");
        assert_eq!(unified.content.len(), 2);
        assert_eq!(unified.content[0].kind, "text");
        assert_eq!(unified.content[1].kind, "image_url");
        assert_eq!(
            unified.content[1]
                .image_url
                .as_ref()
                .map(|i| i.url.as_str()),
            Some("https://e/i.png")
        );
        // Go duration int64 → string conversion (video_inbound.go:92-95).
        assert_eq!(unified.duration.as_deref(), Some("5"));
        assert_eq!(unified.ratio.as_deref(), Some("16:9"));
        assert_eq!(unified.resolution.as_deref(), Some("1080p"));
        assert_eq!(unified.frames, Some(120));
        assert_eq!(unified.seed, Some(42));
        assert_eq!(unified.generate_audio, Some(true));
        assert_eq!(unified.camera_fixed, Some(false));
        assert_eq!(unified.watermark, Some(false));
        assert_eq!(unified.draft, Some(false));
        assert_eq!(unified.service_tier.as_deref(), Some("default"));
        assert_eq!(unified.execution_expires_after, Some(3600));
        Ok(())
    }

    #[test]
    fn seedance_create_request_rejects_missing_model() -> Result<(), serde_json::Error> {
        let req: SeedanceCreateRequest = serde_json::from_value(serde_json::json!({
            "content": [{"type": "text", "text": "x"}]
        }))?;
        match normalize_seedance_create_request(req) {
            Err(err) => assert_eq!(err.message, "model is required"),
            Ok(_) => panic!("expected model-required error"),
        }
        Ok(())
    }

    #[test]
    fn seedance_create_request_rejects_blank_model() {
        // Go trims before checking (video_inbound.go:71).
        let req = SeedanceCreateRequest {
            model: "   ".to_string(),
            content: vec![SeedanceVideoContent {
                kind: "text".to_string(),
                text: Some("x".to_string()),
                image_url: None,
                role: None,
            }],
            ..Default::default()
        };
        match normalize_seedance_create_request(req) {
            Err(err) => assert_eq!(err.message, "model is required"),
            Ok(_) => panic!("expected model-required error"),
        }
    }

    #[test]
    fn seedance_create_request_rejects_empty_content() {
        let req = SeedanceCreateRequest {
            model: "seedance".to_string(),
            content: vec![],
            ..Default::default()
        };
        match normalize_seedance_create_request(req) {
            Err(err) => assert_eq!(err.message, "content is required"),
            Ok(_) => panic!("expected content-required error"),
        }
    }

    #[test]
    fn seedance_create_request_duration_none_when_absent() -> Result<(), Box<dyn std::error::Error>>
    {
        let req: SeedanceCreateRequest = serde_json::from_value(serde_json::json!({
            "model": "m",
            "content": [{"type": "text", "text": "p"}]
        }))?;
        let unified = normalize_seedance_create_request(req)?;
        assert!(unified.duration.is_none());
        Ok(())
    }

    // ---- S05 shape_seedance_create_response ---------------------------

    #[test]
    fn shape_create_response_emits_only_id() -> Result<(), Box<dyn std::error::Error>> {
        let resp = SeedanceUnifiedResponse {
            object: "video.create".to_string(),
            id: "c1723abc".to_string(),
            ..Default::default()
        };
        let body = shape_seedance_create_response(&resp)?;
        let v: serde_json::Value = serde_json::from_slice(&body)?;
        // Go create-ack body is exactly {"id": "..."} (video_inbound.go:151-160).
        assert_eq!(v["id"], "c1723abc");
        assert_eq!(v.as_object().map(|o| o.len()), Some(1));
        Ok(())
    }

    // ---- S05 shape_seedance_get_response ------------------------------

    #[test]
    fn shape_get_response_maps_all_fields_and_rounds_duration()
    -> Result<(), Box<dyn std::error::Error>> {
        let resp = SeedanceUnifiedResponse {
            id: "t1".to_string(),
            status: "succeeded".to_string(),
            model: "seedance-1".to_string(),
            created_at: 1_700_000_000,
            completed_at: 1_700_000_100,
            ratio: "16:9".to_string(),
            resolution: "1080p".to_string(),
            fps: Some(30),
            seed: Some(7),
            // Go: ParseFloat + math.Round (video_inbound.go:185-189) → 4.
            duration: Some("3.6".to_string()),
            video_url: "https://e/v.mp4".to_string(),
            usage: Some(SeedanceGetUsage {
                completion_tokens: 100,
                total_tokens: 120,
            }),
            ..Default::default()
        };
        let body = shape_seedance_get_response(&resp, 1_700_000_999)?;
        let v: serde_json::Value = serde_json::from_slice(&body)?;

        assert_eq!(v["id"], "t1");
        assert_eq!(v["model"], "seedance-1");
        assert_eq!(v["status"], "succeeded");
        assert_eq!(v["created_at"], 1_700_000_000);
        assert_eq!(v["updated_at"], 1_700_000_100);
        assert_eq!(v["ratio"], "16:9");
        assert_eq!(v["resolution"], "1080p");
        assert_eq!(v["framespersecond"], 30);
        assert_eq!(v["seed"], 7);
        assert_eq!(v["duration"], 4);
        assert_eq!(v["content"]["video_url"], "https://e/v.mp4");
        assert_eq!(v["usage"]["completion_tokens"], 100);
        assert_eq!(v["usage"]["total_tokens"], 120);
        // service_tier always blanked by Go (video_inbound.go:170).
        assert!(v.get("service_tier").map(|s| s.is_null()).unwrap_or(true));
        Ok(())
    }

    #[test]
    fn shape_get_response_uses_now_when_completed_at_zero() -> Result<(), Box<dyn std::error::Error>>
    {
        let resp = SeedanceUnifiedResponse {
            id: "t2".to_string(),
            ..Default::default()
        };
        let body = shape_seedance_get_response(&resp, 1_700_000_555)?;
        let v: serde_json::Value = serde_json::from_slice(&body)?;
        // Go: `lo.Ternary(v.CompletedAt != 0, ..., time.Now().Unix())`
        // (video_inbound.go:172).
        assert_eq!(v["updated_at"], 1_700_000_555);
        Ok(())
    }

    #[test]
    fn shape_get_response_omits_content_when_no_video_url() -> Result<(), Box<dyn std::error::Error>>
    {
        let resp = SeedanceUnifiedResponse {
            id: "t3".to_string(),
            ..Default::default()
        };
        let body = shape_seedance_get_response(&resp, 0)?;
        let v: serde_json::Value = serde_json::from_slice(&body)?;
        assert!(v.get("content").map(|c| c.is_null()).unwrap_or(true));
        Ok(())
    }

    #[test]
    fn seedance_get_response_round_trips_with_framespersecond_rename()
    -> Result<(), Box<dyn std::error::Error>> {
        // The Go wire tag is the single word "framespersecond" — verify our
        // explicit rename survives a serde round-trip (CLAUDE.md gotcha).
        let json = serde_json::json!({
            "id": "t4",
            "framespersecond": 24
        });
        let resp: SeedanceGetResponse = serde_json::from_value(json)?;
        assert_eq!(resp.frames_per_second, Some(24));
        // Re-serialize and the key must come back verbatim.
        let back = serde_json::to_value(&resp)?;
        assert_eq!(back["framespersecond"], 24);
        assert!(back.get("frames_per_second").is_none());
        Ok(())
    }

    #[test]
    fn go_math_round_rounds_half_away_from_zero() {
        // Match Go's math.Round behavior at the boundary values exercised by
        // video_inbound.go:187-189.
        assert_eq!(go_math_round(3.5), 4);
        assert_eq!(go_math_round(3.4), 3);
        assert_eq!(go_math_round(-3.5), -4);
    }

    // -----------------------------------------------------------------
    // RUST-P7-008 S14 — Doubao provider-specific image helpers
    // (outbound.go:189-302)
    // -----------------------------------------------------------------

    #[test]
    fn encode_image_bytes_to_data_url_returns_empty_for_empty_input() {
        // Go outbound.go:293-294: `if len(b) == 0 { return "" }`.
        assert_eq!(encode_image_bytes_to_data_url(&[]), "");
    }

    #[test]
    fn encode_image_bytes_to_data_url_sniff_png_and_wraps_as_data_url() {
        // PNG magic bytes are detected as image/png; the payload is the
        // base64 encoding of the input wrapped as data:<mime>;base64,<b64>.
        let png = [0x89_u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let url = encode_image_bytes_to_data_url(&png);
        assert!(url.starts_with("data:image/png;base64,"), "got {url}");
        // The payload after the prefix is the base64 of the input bytes.
        let b64 = &url["data:image/png;base64,".len()..];
        // The standard base64 of these 8 bytes is iVBORw0KGgo=.
        assert_eq!(b64, "iVBORw0KGgo=");
    }

    #[test]
    fn encode_image_bytes_to_data_url_sniff_jpeg_gif_webp() {
        // JPEG: FF D8 FF.
        assert!(
            encode_image_bytes_to_data_url(&[0xFF_u8, 0xD8, 0xFF, 0xE0])
                .starts_with("data:image/jpeg;base64,")
        );
        // GIF89a.
        assert!(
            encode_image_bytes_to_data_url(b"GIF89a trailer").starts_with("data:image/gif;base64,")
        );
        // RIFF....WEBP.
        let webp = b"RIFF\x00\x00\x00\x00WEBPVP8 ";
        assert!(encode_image_bytes_to_data_url(webp).starts_with("data:image/webp;base64,"));
    }

    #[test]
    fn encode_image_bytes_to_data_url_falls_back_to_png_for_unknown_magic() {
        // Bytes that don't match any image/* sniff signature fall through
        // to the image/png default (Go outbound.go:298-299).
        let url = encode_image_bytes_to_data_url(b"plain text not an image");
        assert!(url.starts_with("data:image/png;base64,"), "got {url}");
    }

    #[test]
    fn guidance_scale_for_quality_maps_hd_and_standard_only() {
        // Go outbound.go:235-240 switch covers exactly two cases.
        assert_eq!(doubao_guidance_scale_for_quality("hd"), Some(7.5_f64));
        assert_eq!(doubao_guidance_scale_for_quality("standard"), Some(2.5_f64));
        // Empty / unrecognized values return None — Go omits the field.
        assert_eq!(doubao_guidance_scale_for_quality(""), None);
        assert_eq!(doubao_guidance_scale_for_quality("HD"), None);
        assert_eq!(doubao_guidance_scale_for_quality("ultra"), None);
    }

    #[test]
    fn build_doubao_image_request_body_minimal_generation() {
        // Go outbound.go:210-216: bare generation carries model/prompt plus
        // the response_format=b64_json and stream=false defaults.
        let body = build_doubao_image_request_body(
            "doubao-seedream-3-0",
            "a surreal painting of a fox",
            &[],
            None,
            None,
            "",
            None,
            None,
        );
        assert_eq!(body["model"], "doubao-seedream-3-0");
        assert_eq!(body["prompt"], "a surreal painting of a fox");
        assert_eq!(body["response_format"], "b64_json");
        assert_eq!(body["stream"], false);
        // No image/n/size/guidance_scale/user for the bare generation case.
        assert!(body.get("image").is_none());
        assert!(body.get("n").is_none());
        assert!(body.get("size").is_none());
        assert!(body.get("guidance_scale").is_none());
        assert!(body.get("user").is_none());
    }

    #[test]
    fn build_doubao_image_request_body_edit_with_single_image_uses_string() {
        // Go outbound.go:218-222: a single image is emitted as a bare string
        // under the `image` field (NOT wrapped in a one-element array).
        let url = encode_image_bytes_to_data_url(&[0x89_u8, b'P', b'N', b'G']);
        let body = build_doubao_image_request_body(
            "doubao-seedream",
            "edit this",
            &[url.clone()],
            Some(2),
            Some("1024x1024"),
            "hd",
            None,
            None,
        );
        assert_eq!(body["image"], url);
        assert!(body["image"].is_string(), "single image must be a string");
        assert_eq!(body["n"], 2);
        assert_eq!(body["size"], "1024x1024");
        // hd quality -> guidance_scale=7.5 (Go outbound.go:236-237).
        assert_eq!(body["guidance_scale"], 7.5);
    }

    #[test]
    fn build_doubao_image_request_body_edit_with_multiple_images_uses_array() {
        // Go outbound.go:223-225: multiple images are emitted as a JSON array.
        let urls = vec![
            encode_image_bytes_to_data_url(&[0x89_u8, b'P', b'N', b'G']),
            encode_image_bytes_to_data_url(b"GIF89a"),
        ];
        let body = build_doubao_image_request_body(
            "doubao-seedream",
            "edit these",
            &urls,
            None,
            None,
            "standard",
            None,
            None,
        );
        assert!(body["image"].is_array());
        assert_eq!(body["image"].as_array().map(|a| a.len()), Some(2));
        // standard quality -> guidance_scale=2.5 (Go outbound.go:238-239).
        assert_eq!(body["guidance_scale"], 2.5);
    }

    #[test]
    fn build_doubao_image_request_body_response_format_override_wins() {
        // Go outbound.go:242-244: an explicit Image.ResponseFormat overrides
        // the b64_json default.
        let body = build_doubao_image_request_body(
            "doubao-seedream",
            "p",
            &[],
            None,
            None,
            "",
            Some("url"),
            None,
        );
        assert_eq!(body["response_format"], "url");
    }

    #[test]
    fn build_doubao_image_request_body_empty_size_and_user_are_omitted() {
        // Go's `if x != ""` guards skip empty strings (outbound.go:227-248).
        let body =
            build_doubao_image_request_body("m", "p", &[], None, Some(""), "", None, Some(""));
        assert!(body.get("size").is_none());
        assert!(body.get("user").is_none());
    }

    // -----------------------------------------------------------------
    // RUST-P15-001 — Doubao outbound chat/image URL builders + metadata
    //   Go: TestOutboundTransformer_TransformRequest (outbound_test.go:157-397)
    //   Go: TestOutboundTransformer_buildImageGenerationAPIRequest (outbound_test.go:399-576)
    //
    // Pending (require full OutboundTransformer struct + Config + openai
    // integration — [Lovelace-the-3rd ?]):
    //   - TestNewOutboundTransformer (outbound_test.go:18-71, 4 subtests)
    //   - TestNewOutboundTransformerWithConfig (outbound_test.go:73-155, 5 subtests)
    //   - TestOutboundTransformer_TransformRequest "valid chat completion request"
    //     body content assertions (outbound_test.go:176-224) — openai.RequestFromLLM
    //   - TestOutboundTransformer_TransformRequest "image generation request"
    //     HTTP wiring assertions (outbound_test.go:289-310)
    // -----------------------------------------------------------------

    // ---- chat / image URL builders (outbound.go:177, 260) ------------

    #[test]
    fn chat_completions_url_happy_path() {
        // Go outbound_test.go:197: req.URL must equal baseURL + "/chat/completions".
        let url = build_chat_completions_url("https://ark.cn-beijing.volces.com/api/v3");
        assert_eq!(
            url,
            "https://ark.cn-beijing.volces.com/api/v3/chat/completions"
        );
    }

    #[test]
    fn chat_completions_url_normalizes_trailing_slash() {
        // Go outbound_test.go:345-362: base URL with trailing slash produces
        // the same URL as without. Go's NormalizeBaseURL (called in the
        // constructor at outbound.go:66) strips trailing slashes before
        // TransformRequest runs.
        let with_slash = build_chat_completions_url("https://ark.cn-beijing.volces.com/api/v3/");
        let without_slash = build_chat_completions_url("https://ark.cn-beijing.volces.com/api/v3");
        assert_eq!(with_slash, without_slash);
        assert_eq!(
            with_slash,
            "https://ark.cn-beijing.volces.com/api/v3/chat/completions"
        );
    }

    #[test]
    fn image_generations_url_happy_path() {
        // Go outbound_test.go:304: req.URL must equal baseURL + "/images/generations".
        let url = build_image_generations_url("https://ark.cn-beijing.volces.com/api/v3");
        assert_eq!(
            url,
            "https://ark.cn-beijing.volces.com/api/v3/images/generations"
        );
    }

    #[test]
    fn image_generations_url_normalizes_trailing_slash() {
        let with_slash = build_image_generations_url("https://ark.cn-beijing.volces.com/api/v3/");
        let without_slash = build_image_generations_url("https://ark.cn-beijing.volces.com/api/v3");
        assert_eq!(with_slash, without_slash);
    }

    // ---- metadata extraction (outbound.go:139-157) -------------------

    #[test]
    fn extract_metadata_with_both_user_id_and_request_id() {
        // Go outbound_test.go:226-256: both keys present → extracted verbatim.
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("user_id".to_string(), "user123".to_string());
        metadata.insert("request_id".to_string(), "req456".to_string());
        let extracted = extract_doubao_chat_metadata(Some(&metadata), 1_700_000_000);
        assert_eq!(extracted.user_id, "user123");
        assert_eq!(extracted.request_id, "req456");
    }

    #[test]
    fn extract_metadata_auto_generates_request_id_with_req_prefix() {
        // Go outbound_test.go:258-287: only user_id present → request_id is
        // auto-generated with "req_" prefix (outbound.go:151-153).
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("user_id".to_string(), "user123".to_string());
        let extracted = extract_doubao_chat_metadata(Some(&metadata), 1_700_000_000);
        assert_eq!(extracted.user_id, "user123");
        assert_eq!(extracted.request_id, "req_1700000000");
        assert!(extracted.request_id.starts_with("req_"));
    }

    #[test]
    fn extract_metadata_none_auto_generates_request_id() {
        // Go outbound.go:145-154: when Metadata is nil, both fields start
        // empty; request_id is then auto-generated.
        let extracted = extract_doubao_chat_metadata(None, 1_700_000_000);
        assert_eq!(extracted.user_id, "");
        assert_eq!(extracted.request_id, "req_1700000000");
    }

    #[test]
    fn format_request_id_uses_req_prefix_and_unix_timestamp() {
        // Go outbound.go:153: fmt.Sprintf("req_%d", time.Now().Unix()).
        assert_eq!(format_doubao_request_id(1_700_000_000), "req_1700000000");
        assert_eq!(format_doubao_request_id(0), "req_0");
    }

    // ---- chat request field validation (outbound.go:111, 131-133) ----

    #[test]
    fn validate_chat_fields_rejects_empty_model() {
        // Go outbound_test.go:319-333: "model is required".
        match validate_doubao_chat_fields("", 1) {
            Err(err) => assert_eq!(err.message, "model is required"),
            Ok(_) => panic!("expected model-required error"),
        }
    }

    #[test]
    fn validate_chat_fields_rejects_empty_messages() {
        // Go outbound_test.go:335-343: "messages are required".
        match validate_doubao_chat_fields("ep-20241203072800-8f7f", 0) {
            Err(err) => assert_eq!(err.message, "messages are required"),
            Ok(_) => panic!("expected messages-required error"),
        }
    }

    #[test]
    fn validate_chat_fields_accepts_valid_inputs() -> TransformerResult<()> {
        validate_doubao_chat_fields("ep-20241203072800-8f7f", 2)?;
        Ok(())
    }

    #[test]
    fn chat_request_nil_error_message_matches_go() {
        // Go outbound.go:106-108: the nil-request error message. Go uses a
        // plain fmt.Errorf (not wrapping ErrInvalidRequest).
        assert_eq!(CHAT_REQUEST_NIL_ERROR, "chat completion request is nil");
    }

    // ---- image body: user field + watermark absence ------------------
    //   (outbound_test.go:473-495, 519-541)

    #[test]
    fn build_doubao_image_request_body_with_user_field() {
        // Go outbound_test.go:519-541: when Image.User is set, the body
        // carries a "user" field (outbound.go:246-248).
        let body = build_doubao_image_request_body(
            "doubao-image-pro",
            "User image",
            &[],
            None,
            None,
            "",
            None,
            Some("user123"),
        );
        assert_eq!(body["user"], "user123");
    }

    #[test]
    fn build_doubao_image_request_body_watermark_not_present() {
        // Go outbound_test.go:473-495: Doubao does not support a watermark
        // field. The body must never contain "watermark".
        let body = build_doubao_image_request_body(
            "doubao-image-pro",
            "A logo",
            &[],
            None,
            None,
            "",
            None,
            None,
        );
        assert!(body.get("watermark").is_none());
    }
}
