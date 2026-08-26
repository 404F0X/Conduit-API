use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use conduit_db::{RepoError, RequestContext};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::request_service::{
    DataStorageRef, generate_execution_request_body_key, generate_execution_request_dir_key,
    generate_execution_response_body_key, generate_execution_response_chunks_key,
    generate_request_body_key, generate_request_dir_key, generate_request_executions_dir_key,
    generate_response_body_key, generate_response_chunks_key,
};

pub type GcServiceResult<T> = Result<T, GcServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GcServiceError {
    #[error(transparent)]
    Repo(#[from] RepoError),
}

// =========================================================================
// Resource types, cleanup options, and config (Kant-the-2nd)
//
// Mirrors Go `conduit/internal/server/gc/gc.go`:
//   * `defaultBatchSize = 500` (line 26).
//   * `TriggerGcCleanupInput` struct (lines 28-31).
//   * `GcCleanupPreviewItem` struct (lines 33-38).
//   * `Config` struct (lines 40-44).
//   * resource-type switch arms (lines 137-185 — `"requests"` and
//     `"usage_logs"` are the two policy-driven resource types; the
//     threads/traces/channel-probes branches are derived from `requests`
//     days or hard-coded).
//   * `runCleanup` manual-days override resolution (lines 124-136).
//   * `deleteInBatches` loop (lines 83-101).
//   * `runVacuum` `vacuum_full` SQL selection (lines 594-599).
// And mirrors Go `conduit/internal/server/biz/system.go`:
//   * `CleanupOption` struct (lines 281-285).
//   * `StoragePolicy.CleanupOptions` field (line 277).
// And `biz/system_default.go` lines 8-19 (default cleanup options: requests
// days=3 / disabled, usage_logs days=30 / disabled).
//
// All of the above are expressed as pure types + pure helper functions so a
// wired `conduit-scheduler` GC worker can consume them without re-deriving
// the Go switch ladder.
// =========================================================================

/// Default per-batch row count for GC delete loops.
///
/// Parity: Go `gc.defaultBatchSize = 500` (`gc.go` line 26). Every cleanup
/// arm in Go (`cleanupOldRequestExecutions`, `cleanupOldRequestsRecords`,
/// `cleanupUsageLogs`, `cleanupThreads`, `cleanupTraces`,
/// `cleanupChannelProbes`) pages deletes by this constant via
/// `Limit(batchSize)`.
pub const DEFAULT_GC_BATCH_SIZE: u32 = 500;

/// Hard-coded retention for channel probes (always 3 days regardless of
/// policy).
///
/// Parity: Go `runCleanup` calls `w.cleanupChannelProbes(ctx, 3, manual)`
/// unconditionally (`gc.go` line 189) — channel-probe retention is NOT
/// policy-driven.
pub const CHANNEL_PROBE_RETENTION_DAYS: i64 = 3;

/// Resource types the Go GC worker knows how to clean up.
///
/// Parity: Go `runCleanup` resource-type switch (`gc.go` lines 137-185).
/// `Requests` and `UsageLogs` are policy-driven; `Threads`, `Traces`,
/// `ChannelProbes` are derived from the requests days (threads/traces) or
/// hard-coded (channel probes = 3 days, `gc.go` line 189).
///
/// Wire format is the lowercase Go string tag (`"requests"`, `"usage_logs"`)
/// used both in `CleanupOption.ResourceType` and in the manual-trigger
/// `manualDays` map keys (`gc.go` lines 621-626).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GcResourceType {
    Requests,
    UsageLogs,
}

impl GcResourceType {
    /// Iterate the two policy-driven resource types in Go `runCleanup`'s
    /// switch order (`gc.go` lines 137-185: `requests` first, then
    /// `usage_logs`).
    pub const fn policy_driven() -> &'static [GcResourceType] {
        &[GcResourceType::Requests, GcResourceType::UsageLogs]
    }

    /// Go wire string (`"requests"` / `"usage_logs"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            GcResourceType::Requests => "requests",
            GcResourceType::UsageLogs => "usage_logs",
        }
    }
}

/// Per-resource cleanup policy entry.
///
/// Parity: Go `biz.CleanupOption` (`system.go` lines 281-285). JSON tags are
/// snake_case (`"resource_type"`, `"enabled"`, `"cleanup_days"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupOption {
    pub resource_type: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub cleanup_days: i64,
}

impl CleanupOption {
    /// Canonical default cleanup options.
    ///
    /// Parity: Go `defaultStoragePolicy.CleanupOptions` (`system_default.go`
    /// lines 8-19): `requests` (days=3, disabled), `usage_logs` (days=30,
    /// disabled). Both default to **disabled** — GC only runs when an admin
    /// flips `enabled` or fires a manual trigger.
    pub fn defaults() -> Vec<Self> {
        vec![
            Self {
                resource_type: GcResourceType::Requests.as_str().to_string(),
                enabled: false,
                cleanup_days: 3,
            },
            Self {
                resource_type: GcResourceType::UsageLogs.as_str().to_string(),
                enabled: false,
                cleanup_days: 30,
            },
        ]
    }
}

/// Manual cleanup trigger payload.
///
/// Parity: Go `gc.TriggerGcCleanupInput` (`gc.go` lines 28-31). JSON tags
/// are snake_case. A zero/missing field means "do not clean this resource";
/// Go only inserts into the `manualDays` map when the field is `> 0`
/// (`gc.go` lines 621-626).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerGcCleanupInput {
    #[serde(default)]
    pub requests_cleanup_days: i64,
    #[serde(default)]
    pub usage_logs_cleanup_days: i64,
}

/// One row of a cleanup-preview estimate.
///
/// Parity: Go `gc.GcCleanupPreviewItem` (`gc.go` lines 33-38). JSON tags are
/// snake_case (`"resource_type"`, `"estimated_count"`, `"cutoff_time"`,
/// `"retention_days"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcCleanupPreviewItem {
    pub resource_type: String,
    pub estimated_count: i64,
    pub cutoff_time: DateTime<Utc>,
    pub retention_days: i64,
}

/// GC worker configuration.
///
/// Parity: Go `gc.Config` (`gc.go` lines 40-44). `cron` carries the schedule
/// expression; `vacuum_enabled`/`vacuum_full` gate and shape the post-cleanup
/// VACUUM step (`runVacuum`, `gc.go` lines 564-611). Tags are
/// snake_case across Go's `json`/`yaml`/`conf` triple.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcConfig {
    #[serde(default)]
    pub cron: String,
    #[serde(default)]
    pub vacuum_enabled: bool,
    #[serde(default)]
    pub vacuum_full: bool,
}

// -------------------------------------------------------------------------
// Pure helpers — cutoff / manual-days / batch loop / preview / vacuum SQL
// -------------------------------------------------------------------------

/// Compute the cleanup cutoff instant: rows with `created_at < cutoff` are
/// eligible for deletion.
///
/// Parity: Go `cutoffTime := time.Now().AddDate(0, 0, -cleanupDays)` used in
/// `cleanupRequests` (`gc.go` line 215), `cleanupUsageLogs` (line 426),
/// `cleanupThreads` (line 462), `cleanupTraces` (line 498),
/// `cleanupChannelProbes` (line 534), and `PreviewCleanup` (lines 639, 653).
///
/// Returns `None` when `cleanup_days <= 0` — Go's guard (`gc.go` lines 210,
/// 422, 458, 494, 530) makes `<= 0` a no-op for every cleanup arm. The
/// preview path (`gc.go` lines 638, 652) skips the resource entirely when
/// its days field is `<= 0`, so callers must consult the `Option` before
/// calling this helper.
pub fn cleanup_cutoff(now: DateTime<Utc>, cleanup_days: i64) -> Option<DateTime<Utc>> {
    if cleanup_days <= 0 {
        return None;
    }
    Some(now - Duration::days(cleanup_days))
}

/// Whether a given cleanup option should actually run.
///
/// Parity: Go `runCleanup` outer guard (`gc.go` lines 125-130):
/// ```text
/// if option.Enabled || manual {
///     if manual && manualDays != nil {
///         if _, ok := manualDays[option.ResourceType]; !ok {
///             continue
///         }
///     }
///     ...
/// }
/// ```
/// In automatic mode an option runs iff `enabled`. In manual mode the option
/// runs iff its `resource_type` appears in `manual_days` (regardless of the
/// `enabled` flag).
pub fn should_run_cleanup(
    option: &CleanupOption,
    manual: bool,
    manual_days: &BTreeMap<String, i64>,
) -> bool {
    if manual {
        // Manual mode: only the resource types the caller named run; the
        // policy `enabled` flag is ignored (Go short-circuits on `manual`).
        manual_days.contains_key(&option.resource_type)
    } else {
        option.enabled
    }
}

/// Resolve the effective cleanup-days value for an option, applying the
/// manual override if present.
///
/// Parity: Go `runCleanup` days-resolution (`gc.go` lines 131-136):
/// ```text
/// days := option.CleanupDays
/// if manual && manualDays != nil {
///     if d, ok := manualDays[option.ResourceType]; ok {
///         days = d
///     }
/// }
/// ```
/// Non-manual mode returns `option.cleanup_days` verbatim; manual mode
/// substitutes the override when present, otherwise falls back to the
/// policy value.
pub fn effective_cleanup_days(
    option: &CleanupOption,
    manual: bool,
    manual_days: &BTreeMap<String, i64>,
) -> i64 {
    if manual {
        manual_days
            .get(&option.resource_type)
            .copied()
            .unwrap_or(option.cleanup_days)
    } else {
        option.cleanup_days
    }
}

/// Convert a manual trigger input into the Go-shape `manualDays` map.
///
/// Parity: Go `RunCleanupNow` (`gc.go` lines 619-627) which builds
/// `manualDays := map[string]int{}` and only inserts entries whose days
/// field is `> 0`. Resources absent from the map are skipped by
/// `should_run_cleanup` in manual mode.
pub fn resolve_manual_days(input: &TriggerGcCleanupInput) -> BTreeMap<String, i64> {
    let mut map = BTreeMap::new();
    if input.requests_cleanup_days > 0 {
        map.insert(
            GcResourceType::Requests.as_str().to_string(),
            input.requests_cleanup_days,
        );
    }
    if input.usage_logs_cleanup_days > 0 {
        map.insert(
            GcResourceType::UsageLogs.as_str().to_string(),
            input.usage_logs_cleanup_days,
        );
    }
    map
}

/// Build the list of preview items for a manual trigger input without
/// touching the database.
///
/// Parity: Go `PreviewCleanup` (`gc.go` lines 632-667) — only resources
/// whose days field is `> 0` are emitted (lines 638, 652). The
/// `estimated_count` is left at `0` here; the wired layer fills it from the
/// real `COUNT(*)` query. The `resource_type`, `cutoff_time`, and
/// `retention_days` fields are fully determined by the input and `now`, so
/// this helper produces a complete preview *shape* that the caller only
/// needs to annotate with counts.
pub fn preview_plan(
    input: &TriggerGcCleanupInput,
    now: DateTime<Utc>,
) -> Vec<GcCleanupPreviewItem> {
    let mut items = Vec::new();
    if input.requests_cleanup_days > 0
        && let Some(cutoff) = cleanup_cutoff(now, input.requests_cleanup_days)
    {
        items.push(GcCleanupPreviewItem {
            resource_type: GcResourceType::Requests.as_str().to_string(),
            estimated_count: 0,
            cutoff_time: cutoff,
            retention_days: input.requests_cleanup_days,
        });
    }
    if input.usage_logs_cleanup_days > 0
        && let Some(cutoff) = cleanup_cutoff(now, input.usage_logs_cleanup_days)
    {
        items.push(GcCleanupPreviewItem {
            resource_type: GcResourceType::UsageLogs.as_str().to_string(),
            estimated_count: 0,
            cutoff_time: cutoff,
            retention_days: input.usage_logs_cleanup_days,
        });
    }
    items
}

/// Select the PostgreSQL VACUUM statement for the configured maintenance mode.
pub fn select_vacuum_sql(vacuum_full: bool) -> &'static str {
    if vacuum_full { "VACUUM FULL" } else { "VACUUM" }
}

/// Outcome of a single batch-delete step.
///
/// Produced by a closure the caller supplies to [`delete_in_batches`]; the
/// `delete_in_batches` helper then decides whether to keep looping based on
/// `deleted_rows`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchDeleteOutcome {
    pub deleted_rows: u32,
}

/// Drive a Go-style "loop until a batch returns 0" delete walk.
///
/// Parity: Go `deleteInBatches` (`gc.go` lines 83-101):
/// ```text
/// totalDeleted := 0
/// for {
///     deleted, err := deleteFunc()
///     if err != nil { return totalDeleted, fmt.Errorf("failed to delete batch: %w", err) }
///     if deleted == 0 { break }
///     totalDeleted += deleted
/// }
/// return totalDeleted, nil
/// ```
/// `delete_one_batch` returns `Err` to abort the loop (mirroring Go's
/// early return); `Ok(0)` ends the loop cleanly; any other `Ok(n)` is added
/// to the running total and the loop continues.
///
/// The cap is defensive — Go's loop is unbounded by design because each
/// batch is capped at `defaultBatchSize`, but a misbehaving `delete_one_batch`
/// that always returns the same positive number would loop forever. We cap
/// at a generous 100k iterations (well above any realistic 500-row-batch
/// cleanup) and return the partial total instead of spinning.
pub fn delete_in_batches<E>(
    mut delete_one_batch: impl FnMut() -> Result<u32, E>,
) -> Result<u64, E> {
    let mut total_deleted: u64 = 0;
    for _ in 0..100_000 {
        let deleted = delete_one_batch()?;
        if deleted == 0 {
            break;
        }
        total_deleted += u64::from(deleted);
    }
    Ok(total_deleted)
}

// =========================================================================
// S05 (GC cron scheduling decision layer) + S07 (external storage request-dir
// cleanup) — Hilbert-the-8th.
//
// Mirrors Go `conduit/internal/server/gc/`:
//   * `gc.go` lines 73-80   — `RegisterScheduledTasks` (TaskSpec name/cron/tz).
//   * `gc_internal.go` 9-12 — `runAutomaticCleanup` (system bypass + manual=false).
//   * `gc.go` lines 110-206 — `runCleanup` dispatch ladder (which resources run,
//     in which order, channel-probe tail, vacuum tail).
//   * `gc.go` lines 330-418 — external-storage cleanup for executions/requests
//     (`cleanupExecutionExternalStorage`, `cleanupRequestExternalStorage`,
//     `getDataStorageCached`).
// And `conduit/conf/conf.go` lines 228-230 — viper GC defaults.
//
// Context-shaping parity note (`gc.go` lines 113-114): every cleanup run
// executes with `ent.NewContext` + `schematype.SkipSoftDelete(ctx)` — GC
// physically deletes rows INCLUDING soft-deleted ones. The automatic path
// additionally wraps `authz.WithSystemBypass(ctx, "gc-cleanup")`
// (`gc_internal.go` line 10, see [`GC_SYSTEM_BYPASS_REASON`]). The wired
// executor must reproduce both when running a [`GcRunPlan`].
// =========================================================================

/// Scheduler task name for the GC job.
///
/// Parity: Go `scheduler.TaskSpec{Name: "gc"}` (`gc.go` line 75). The Go
/// scheduler rejects duplicate names (`scheduler.go` lines 33-35), so this is
/// also the registry de-dup key.
pub const GC_TASK_NAME: &str = "gc";

/// Scheduler task description for the GC job.
///
/// Parity: Go `TaskSpec.Description` (`gc.go` line 76), copied verbatim.
pub const GC_TASK_DESCRIPTION: &str =
    "Garbage collection — cleanup old requests, traces, usage logs, and channel probes";

/// Timezone the GC cron is evaluated in.
///
/// Parity: Go `TaskSpec{Timezone: "UTC"}` (`gc.go` line 78). The Go scheduler
/// would default an empty timezone to UTC anyway (`scheduler.go` lines
/// 176-179), but gc pins it explicitly.
pub const GC_TASK_TIMEZONE: &str = "UTC";

/// Authz bypass reason used by the automatic cleanup entry point.
///
/// Parity: Go `authz.WithSystemBypass(ctx, "gc-cleanup")`
/// (`gc_internal.go` line 10).
pub const GC_SYSTEM_BYPASS_REASON: &str = "gc-cleanup";

/// Default GC cron expression — daily at 02:00.
///
/// Parity: Go viper default `v.SetDefault("gc.cron", "0 2 * * *")`
/// (`conf/conf.go` line 228).
pub const DEFAULT_GC_CRON: &str = "0 2 * * *";

/// Config-validation failure for [`GcConfig`].
///
/// Parity: Go `gc.Config` tags `CRON` with `validate:"required"`
/// (`gc.go` line 41) — config loading rejects a missing/empty cron.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GcConfigError {
    #[error("gc config: cron is required")]
    MissingCron,
}

impl GcConfig {
    /// The config the Go binary runs with when nothing is set in YAML/env.
    ///
    /// Parity: Go viper defaults (`conf/conf.go` lines 228-230):
    /// `gc.cron = "0 2 * * *"`, `gc.vacuum_enabled = true`,
    /// `gc.vacuum_full = false`. NOTE this differs from
    /// [`GcConfig::default`] (the derived `Default`), which mirrors the Go
    /// zero-value struct (empty cron, both flags false).
    pub fn conf_default() -> Self {
        Self {
            cron: DEFAULT_GC_CRON.to_string(),
            vacuum_enabled: true,
            vacuum_full: false,
        }
    }

    /// Validate the config the way Go's `validate:"required"` tag does
    /// (`gc.go` line 41): only `CRON` is required; the vacuum booleans have
    /// no constraints. go-playground `required` fails on the zero value
    /// (empty string), so whitespace-only strings pass — mirrored here.
    pub fn validate(&self) -> Result<(), GcConfigError> {
        if self.cron.is_empty() {
            return Err(GcConfigError::MissingCron);
        }
        Ok(())
    }
}

/// The scheduler registration values for the GC job.
///
/// Parity: Go `scheduler.TaskSpec` as built by `Worker.RegisterScheduledTasks`
/// (`gc.go` lines 73-80). Go's spec also carries `FixRate` (task.go line 29),
/// which gc never sets — cron-only scheduling, so it is omitted here.
///
/// Scheduling semantics to reproduce when wiring (verified against Go):
/// * The cron callback is wrapped with panic recovery + `lastRunAt`/
///   `lastError` bookkeeping (`scheduler.go` lines 146-168; a panic is
///   recorded as `"panic: %v"`). The Rust `conduit-scheduler` crate's
///   `run_worker_loop` `catch_unwind` container matches this.
/// * Go's gc `Worker` has NO re-entrancy mutex and the production executor is
///   a 64-worker pool (`dependencies/executors.go` lines 25-33), so Go relies
///   solely on the daily cadence to avoid overlap. A Rust driver that awaits
///   each run before the next tick (as `run_worker_loop` does) is therefore a
///   strict superset of Go's protection — no extra guard is needed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcTaskSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub cron_expr: String,
    pub timezone: &'static str,
}

/// Build the GC task spec from config.
///
/// Parity: Go `RegisterScheduledTasks` (`gc.go` lines 73-80) — the cron
/// expression is passed through verbatim from `Config.CRON`; unlike
/// provider-quota (which derives a cron from an interval via
/// `intervalToCronExpr`), gc takes a raw cron expression from config.
pub fn gc_task_spec(config: &GcConfig) -> GcTaskSpec {
    GcTaskSpec {
        name: GC_TASK_NAME,
        description: GC_TASK_DESCRIPTION,
        cron_expr: config.cron.clone(),
        timezone: GC_TASK_TIMEZONE,
    }
}

/// Resources a [`GcRunStep`] can target — the concrete cleanup arms of Go's
/// `runCleanup` (`gc.go` lines 110-206).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcRunResource {
    /// `cleanupRequests` (`gc.go` lines 209-237). The DB layer must delete
    /// request executions FIRST, then requests (`gc.go` lines 217-234), each
    /// batch performing per-row external-storage cleanup
    /// ([`cleanup_execution_external_storage`] /
    /// [`cleanup_request_external_storage`]) before the batch delete.
    Requests,
    /// `cleanupThreads` (`gc.go` lines 456-489) — runs on the same days value
    /// as `Requests` (`gc.go` line 150).
    Threads,
    /// `cleanupTraces` (`gc.go` lines 492-525) — same days as `Requests`
    /// (`gc.go` line 161).
    Traces,
    /// `cleanupUsageLogs` (`gc.go` lines 421-453).
    UsageLogs,
    /// `cleanupChannelProbes` (`gc.go` lines 528-561). NOTE: the probe table
    /// stores unix seconds — Go compares `TimestampLT(cutoffTime.Unix())`
    /// (`gc.go` line 539), so the DB layer must compare `cutoff_at` as a unix
    /// timestamp, not a datetime.
    ChannelProbes,
    /// Erase retained request/execution headers while keeping the audit row.
    RequestHeaders,
    /// Erase inbound request bodies while keeping the request/execution row.
    RequestBodies,
    /// Erase provider response bodies while keeping metrics and status.
    ResponseBodies,
    /// Erase persisted streaming chunks while keeping the request/execution row.
    ResponseChunks,
}

/// One cleanup action of a GC run: delete `resource` rows with
/// `created_at < cutoff_at` (or `timestamp < cutoff_at.timestamp()` for
/// channel probes), paged by [`DEFAULT_GC_BATCH_SIZE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcRunStep {
    pub resource: GcRunResource,
    pub cutoff_at: DateTime<Utc>,
    pub retention_days: i64,
}

/// A fully-resolved GC run: the ordered steps of Go `runCleanup`
/// (`gc.go` lines 110-206) plus the vacuum tail flag.
///
/// Error semantics for the executor (Go parity): each step's failure is
/// logged and the run CONTINUES with the next step (`gc.go` lines 139-196 —
/// every `cleanupXxx` error is `log.Error` + fall-through, never an abort).
/// Only a storage-policy load failure aborts the whole run before any step
/// (`gc.go` lines 116-121) — in that case no plan should be built at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcRunPlan {
    /// Steps in Go dispatch order: policy options in slice order (requests →
    /// threads → traces for the `"requests"` arm; usage_logs for its arm),
    /// then the unconditional channel-probe tail (`gc.go` line 189).
    pub steps: Vec<GcRunStep>,
    /// Whether the post-cleanup VACUUM should run (`gc.go` lines 198-203:
    /// gated only on `Config.VacuumEnabled`; the dialect/SQL choice is
    /// [`select_vacuum_sql`]'s job at execution time).
    pub run_vacuum: bool,
    /// Resource types Go would log "Unknown resource type for cleanup" for
    /// (`gc.go` lines 182-184). No steps are emitted for them.
    pub unknown_resources: Vec<String>,
}

/// Build the cleanup run plan — the decision core of Go `runCleanup`
/// (`gc.go` lines 110-206), parameterized over both entry modes.
///
/// Per option (policy slice order):
/// 1. Gate via [`should_run_cleanup`] (`gc.go` lines 125-130).
/// 2. Resolve days via [`effective_cleanup_days`] (`gc.go` lines 131-136).
/// 3. Dispatch on the resource-type string (`gc.go` lines 137-185):
///    `"requests"` expands to Requests+Threads+Traces on the same cutoff;
///    `"usage_logs"` emits one step; anything else is recorded as unknown.
///    Steps with `days <= 0` are omitted — each Go helper no-ops on that
///    guard (`gc.go` lines 210, 422, 458, 494), so the observable DB effect
///    is identical.
///
/// Then the channel-probe step is ALWAYS appended at 3 days (`gc.go` line
/// 189), and `run_vacuum` mirrors `Config.VacuumEnabled` (`gc.go` line 198).
pub fn build_gc_run_plan(
    cleanup_options: &[CleanupOption],
    manual: bool,
    manual_days: &BTreeMap<String, i64>,
    config: &GcConfig,
    now: DateTime<Utc>,
) -> GcRunPlan {
    let mut steps = Vec::new();
    let mut unknown_resources = Vec::new();

    for option in cleanup_options {
        if !should_run_cleanup(option, manual, manual_days) {
            continue;
        }
        let days = effective_cleanup_days(option, manual, manual_days);

        match option.resource_type.as_str() {
            // Go `case "requests"` (`gc.go` lines 138-170): requests, then
            // threads, then traces, all on the same days value.
            "requests" => {
                if let Some(cutoff_at) = cleanup_cutoff(now, days) {
                    for resource in [
                        GcRunResource::Requests,
                        GcRunResource::Threads,
                        GcRunResource::Traces,
                    ] {
                        steps.push(GcRunStep {
                            resource,
                            cutoff_at,
                            retention_days: days,
                        });
                    }
                }
            }
            // Go `case "usage_logs"` (`gc.go` lines 171-181).
            "usage_logs" => {
                if let Some(cutoff_at) = cleanup_cutoff(now, days) {
                    steps.push(GcRunStep {
                        resource: GcRunResource::UsageLogs,
                        cutoff_at,
                        retention_days: days,
                    });
                }
            }
            "request_headers" => {
                push_content_steps(&mut steps, GcRunResource::RequestHeaders, days, now)
            }
            "request_bodies" => {
                push_content_steps(&mut steps, GcRunResource::RequestBodies, days, now)
            }
            "response_bodies" => {
                push_content_steps(&mut steps, GcRunResource::ResponseBodies, days, now)
            }
            "response_chunks" => {
                push_content_steps(&mut steps, GcRunResource::ResponseChunks, days, now)
            }
            // Go `default:` warn (`gc.go` lines 182-184).
            other => unknown_resources.push(other.to_string()),
        }
    }

    // Channel probes are cleaned unconditionally at 3 days (`gc.go` line 189);
    // the `if let` mirrors the helper's `<= 0` guard (`gc.go` lines 529-532),
    // which is always passed for the constant 3.
    if let Some(cutoff_at) = cleanup_cutoff(now, CHANNEL_PROBE_RETENTION_DAYS) {
        steps.push(GcRunStep {
            resource: GcRunResource::ChannelProbes,
            cutoff_at,
            retention_days: CHANNEL_PROBE_RETENTION_DAYS,
        });
    }

    GcRunPlan {
        steps,
        run_vacuum: config.vacuum_enabled,
        unknown_resources,
    }
}

fn push_content_steps(
    steps: &mut Vec<GcRunStep>,
    resource: GcRunResource,
    days: i64,
    now: DateTime<Utc>,
) {
    if let Some(cutoff_at) = cleanup_cutoff(now, days) {
        steps.push(GcRunStep {
            resource,
            cutoff_at,
            retention_days: days,
        });
    }
}

/// Plan for the automatic (cron) entry point.
///
/// Parity: Go `runAutomaticCleanup` (`gc_internal.go` lines 9-12) —
/// `runCleanup(ctx, false, nil)` under the [`GC_SYSTEM_BYPASS_REASON`]
/// system-bypass principal.
pub fn build_automatic_gc_run_plan(
    cleanup_options: &[CleanupOption],
    config: &GcConfig,
    now: DateTime<Utc>,
) -> GcRunPlan {
    build_gc_run_plan(cleanup_options, false, &BTreeMap::new(), config, now)
}

/// Plan for the manual trigger entry point.
///
/// Parity: Go `RunCleanupNow` (`gc.go` lines 619-629) — builds the
/// `manualDays` map from the input (only `> 0` fields, via
/// [`resolve_manual_days`]) and calls `runCleanup(ctx, true, manualDays)`.
/// An all-zero input yields an empty map, so only the channel-probe tail
/// (and vacuum, if enabled) runs.
pub fn build_manual_gc_run_plan(
    cleanup_options: &[CleanupOption],
    input: &TriggerGcCleanupInput,
    config: &GcConfig,
    now: DateTime<Utc>,
) -> GcRunPlan {
    build_gc_run_plan(
        cleanup_options,
        true,
        &resolve_manual_days(input),
        config,
        now,
    )
}

// -------------------------------------------------------------------------
// S07 — external storage cleanup for request / execution rows.
// -------------------------------------------------------------------------

/// The request-row projection GC works on.
///
/// Parity: Go `cleanupOldRequestsRecords` selects exactly
/// `request.FieldID`, `request.FieldProjectID`, `request.FieldDataStorageID`
/// (`gc.go` lines 294-299). `data_storage_id == 0` is the Go zero value for
/// "no external storage".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcRequestRow {
    pub id: i64,
    pub project_id: i64,
    pub data_storage_id: i64,
}

/// The request-execution-row projection GC works on.
///
/// Parity: Go `cleanupOldRequestExecutions` selects
/// `requestexecution.{FieldID, FieldProjectID, FieldDataStorageID,
/// FieldRequestID}` (`gc.go` lines 245-251).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcRequestExecutionRow {
    pub id: i64,
    pub project_id: i64,
    pub request_id: i64,
    pub data_storage_id: i64,
}

/// Storage keys deleted when GC drops a request row, in Go's exact order.
///
/// Parity: Go `cleanupRequestExternalStorage` keys slice (`gc.go` lines
/// 386-392): request_body, response_body, response_chunks, the executions
/// dir, then the request dir. The order is load-bearing for fs-backed
/// storage — Go's `DeleteData` uses `fs.Remove` (`data_storage.go` line
/// 628), which only removes empty directories, so files must go first, then
/// the executions subdirectory, then the request directory
/// (`gc_test.go` `TestWorker_cleanupRequestExternalStorageDeletesFsArtifacts`
/// asserts all five vanish).
pub fn request_external_cleanup_keys(project_id: i64, request_id: i64) -> Vec<String> {
    vec![
        generate_request_body_key(project_id, request_id),
        generate_response_body_key(project_id, request_id),
        generate_response_chunks_key(project_id, request_id),
        generate_request_executions_dir_key(project_id, request_id),
        generate_request_dir_key(project_id, request_id),
    ]
}

/// Storage keys deleted when GC drops a request-execution row, in Go order.
///
/// Parity: Go `cleanupExecutionExternalStorage` keys slice (`gc.go` lines
/// 349-354): execution request_body, response_body, response_chunks, then
/// the execution dir. Same files-before-dir ordering rationale as
/// [`request_external_cleanup_keys`].
pub fn execution_external_cleanup_keys(
    project_id: i64,
    request_id: i64,
    execution_id: i64,
) -> Vec<String> {
    vec![
        generate_execution_request_body_key(project_id, request_id, execution_id),
        generate_execution_response_body_key(project_id, request_id, execution_id),
        generate_execution_response_chunks_key(project_id, request_id, execution_id),
        generate_execution_request_dir_key(project_id, request_id, execution_id),
    ]
}

/// The two `DataStorageService` capabilities GC needs, as a DI seam.
///
/// Parity: Go gc depends on `*biz.DataStorageService` for
/// `GetDataStorageByID` (`data_storage.go` lines 281-301) and `DeleteData`
/// (`data_storage.go` lines 608-640). The wired impl adapts the
/// `conduit-storage` `DataStorageService` facade plus the (not-yet-ported)
/// DataStorage row repo; tests inject an in-memory fake.
#[async_trait]
pub trait GcExternalStorage: Send + Sync {
    /// Look up a DataStorage row by id.
    ///
    /// Go returns `(*ent.DataStorage, error)` and never `(nil, nil)` —
    /// not-found surfaces as an error (`data_storage.go` lines 289-292).
    /// `Ok(None)` here models Go's defensive `ds == nil` branch
    /// (`gc.go` lines 345, 382), which is unreachable in practice.
    async fn get_data_storage_by_id(&self, id: i64) -> Result<Option<DataStorageRef>, String>;

    /// Delete one object key from the given storage.
    ///
    /// Implementations must mirror Go `DeleteData` (`data_storage.go` lines
    /// 608-640): database-type storage is a no-op success, and a missing key
    /// is success (`os.ErrNotExist` → nil).
    async fn delete_data(&self, storage: &DataStorageRef, key: &str) -> Result<(), String>;
}

/// Per-run DataStorage lookup cache.
///
/// Parity: Go `cache := make(map[int]*ent.DataStorage)` created once per
/// batch walk (`gc.go` lines 242, 291) and consulted by
/// `getDataStorageCached` (`gc.go` lines 405-418). Only successful lookups
/// are cached — an errored lookup is retried on the next row.
pub type GcDataStorageCache = BTreeMap<i64, Option<DataStorageRef>>;

/// A single failed key deletion (Go: warn log + continue,
/// `gc.go` lines 356-364, 394-402).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcKeyDeleteFailure {
    pub key: String,
    pub error: String,
}

/// Observable outcome of one row's external-storage cleanup.
///
/// Go's functions return nothing — every failure is a `log.Warn` and the
/// batch continues. This report is the Rust projection of those warn logs so
/// callers/tests can observe them without a logger dependency. A skipped row
/// (no external storage / primary storage) yields an all-empty report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcExternalCleanupReport {
    /// Keys a delete was attempted for, in attempt order.
    pub attempted_keys: Vec<String>,
    /// Keys whose delete failed (Go `gc.go` lines 357-363: warn + continue —
    /// one bad key never aborts the remaining keys).
    pub delete_failures: Vec<GcKeyDeleteFailure>,
    /// Set when the DataStorage lookup itself failed (Go `gc.go` lines
    /// 336-343, 372-380: warn + skip the row entirely).
    pub lookup_error: Option<String>,
}

/// Shared guard ladder + delete loop for both row kinds.
///
/// Parity: the identical bodies of Go `cleanupRequestExternalStorage`
/// (`gc.go` lines 367-403) and `cleanupExecutionExternalStorage` (`gc.go`
/// lines 330-365):
/// 1. `DataStorageID == 0` → skip (nil-row / nil-service guards are not
///    representable here).
/// 2. Cached lookup via `getDataStorageCached` (`gc.go` lines 405-418);
///    lookup error → warn + skip, NOT cached.
/// 3. `ds == nil || ds.Primary` → skip: primary storage means the payloads
///    live in DB columns and die with the row — only non-primary external
///    storage holds files to delete.
/// 4. Delete each key; a failure is recorded and the loop continues.
async fn cleanup_external_storage_keys(
    storage: &dyn GcExternalStorage,
    data_storage_id: i64,
    keys: Vec<String>,
    cache: &mut GcDataStorageCache,
) -> GcExternalCleanupReport {
    let mut report = GcExternalCleanupReport::default();

    // Go `gc.go` lines 331, 368: zero DataStorageID → nothing external.
    if data_storage_id == 0 {
        return report;
    }

    // Go `getDataStorageCached` (`gc.go` lines 405-418).
    let ds = match cache.get(&data_storage_id) {
        Some(cached) => *cached,
        None => match storage.get_data_storage_by_id(data_storage_id).await {
            Ok(fetched) => {
                cache.insert(data_storage_id, fetched);
                fetched
            }
            Err(error) => {
                // Go warns "Failed to load data storage ..." and returns
                // without caching (`gc.go` lines 336-343 / 372-380 + 410-413).
                report.lookup_error = Some(error);
                return report;
            }
        },
    };

    // Go `gc.go` lines 345, 382: `ds == nil || ds.Primary` → skip.
    let Some(ds) = ds else {
        return report;
    };
    if ds.primary {
        return report;
    }

    for key in keys {
        report.attempted_keys.push(key.clone());
        if let Err(error) = storage.delete_data(&ds, &key).await {
            // Go warn + continue (`gc.go` lines 356-364 / 394-402).
            report
                .delete_failures
                .push(GcKeyDeleteFailure { key, error });
        }
    }

    report
}

/// Clean up the external-storage artifacts of one request row.
///
/// Parity: Go `cleanupRequestExternalStorage` (`gc.go` lines 367-403),
/// invoked per row inside the `cleanupOldRequestsRecords` batch walk
/// (`gc.go` line 315) BEFORE the batch's DB delete.
pub async fn cleanup_request_external_storage(
    storage: &dyn GcExternalStorage,
    req: &GcRequestRow,
    cache: &mut GcDataStorageCache,
) -> GcExternalCleanupReport {
    let keys = request_external_cleanup_keys(req.project_id, req.id);
    cleanup_external_storage_keys(storage, req.data_storage_id, keys, cache).await
}

/// Clean up the external-storage artifacts of one request-execution row.
///
/// Parity: Go `cleanupExecutionExternalStorage` (`gc.go` lines 330-365),
/// invoked per row inside the `cleanupOldRequestExecutions` batch walk
/// (`gc.go` line 268) BEFORE the batch's DB delete.
pub async fn cleanup_execution_external_storage(
    storage: &dyn GcExternalStorage,
    exec: &GcRequestExecutionRow,
    cache: &mut GcDataStorageCache,
) -> GcExternalCleanupReport {
    let keys = execution_external_cleanup_keys(exec.project_id, exec.request_id, exec.id);
    cleanup_external_storage_keys(storage, exec.data_storage_id, keys, cache).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GcTargetKind {
    StaleRequests,
    SoftDeletedRetention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcTarget {
    pub kind: GcTargetKind,
    pub cutoff_at: DateTime<Utc>,
    pub limit: u32,
}

impl GcTarget {
    pub fn stale_requests(now: DateTime<Utc>, stale_after: Duration, limit: u32) -> Self {
        Self {
            kind: GcTargetKind::StaleRequests,
            cutoff_at: now - stale_after,
            limit,
        }
    }

    pub fn soft_deleted_retention(now: DateTime<Utc>, retention: Duration, limit: u32) -> Self {
        Self {
            kind: GcTargetKind::SoftDeletedRetention,
            cutoff_at: now - retention,
            limit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    pub target: GcTarget,
    pub deleted_rows: u64,
}

#[async_trait]
pub trait GcRepo: Send + Sync {
    async fn cleanup_stale_requests(
        &self,
        ctx: &RequestContext,
        target: &GcTarget,
    ) -> GcServiceResult<u64>;

    async fn cleanup_soft_deleted_retention(
        &self,
        ctx: &RequestContext,
        target: &GcTarget,
    ) -> GcServiceResult<u64>;
}

pub struct GcService {
    repo: Arc<dyn GcRepo>,
}

impl GcService {
    pub fn new(repo: Arc<dyn GcRepo>) -> Self {
        Self { repo }
    }

    pub async fn cleanup_stale_requests(
        &self,
        ctx: &RequestContext,
        now: DateTime<Utc>,
        stale_after: Duration,
        limit: u32,
    ) -> GcServiceResult<GcReport> {
        let target = GcTarget::stale_requests(now, stale_after, limit);
        // The service only computes selection bounds; concrete repos own delete semantics.
        let deleted_rows = self.repo.cleanup_stale_requests(ctx, &target).await?;

        Ok(GcReport {
            target,
            deleted_rows,
        })
    }

    pub async fn cleanup_soft_deleted_retention(
        &self,
        ctx: &RequestContext,
        now: DateTime<Utc>,
        retention: Duration,
        limit: u32,
    ) -> GcServiceResult<GcReport> {
        let target = GcTarget::soft_deleted_retention(now, retention, limit);
        // Retention cleanup is intentionally separate from stale request cleanup
        // so future backends can map it to distinct tables without API churn.
        let deleted_rows = self
            .repo
            .cleanup_soft_deleted_retention(ctx, &target)
            .await?;

        Ok(GcReport {
            target,
            deleted_rows,
        })
    }
}

// =========================================================================
// S06 — manual trigger preview with real COUNT queries (Hilbert-the-11th).
//
// Mirrors Go `conduit/internal/server/gc/gc.go` `PreviewCleanup`
// (lines 632-667): Go emits a preview item per resource whose days field is
// `> 0` (lines 638, 652), computes the cutoff via `time.Now().AddDate(0, 0,
// -days)` (lines 639, 653), and runs an `Ent.Request.Count` /
// `Ent.UsageLog.Count` query (lines 640, 654) to populate
// `EstimatedCount`. The Rust split is:
//   * [`preview_plan`] (Kant) — pure shape, count=0.
//   * [`GcPreviewCounter`] + [`preview_with_counts`] (this block) — adds the
//     COUNT query seam and the wired shape-and-count composer.
// A manual trigger's "build the steps but do NOT execute DELETE" semantics
// (`gc.go` line 632 doc-comment: "without actually deleting them") are
// satisfied trivially because the preview path queries `Count`, never
// `Delete`; [`build_manual_gc_run_plan`] is the symmetric "plan a real
// cleanup" helper for `RunCleanupNow`.
// =========================================================================

/// Counts rows eligible for preview-style estimation.
///
/// Parity: Go `PreviewCleanup` (`gc.go` lines 640, 654) — only `Request`
/// and `UsageLog` tables are queried; `threads`/`traces`/`channel_probes`
/// are NOT previewed by Go. Implementations mirror Go's
/// `.Where(*.CreatedAtLT(cutoff)).Count(ctx)`.
#[async_trait]
pub trait GcPreviewCounter: Send + Sync {
    /// Count request rows with `created_at < cutoff`.
    ///
    /// Parity: Go `w.Ent.Request.Query().Where(request.CreatedAtLT(cutoff))
    /// .Count(ctx)` (`gc.go` line 640). Runs under
    /// `schematype.SkipSoftDelete(ctx)` (set up by the caller, `gc.go` line
    /// 634) so soft-deleted rows are included in the estimate.
    async fn count_requests_older_than(&self, cutoff_at: DateTime<Utc>) -> Result<i64, String>;

    /// Count usage-log rows with `created_at < cutoff`.
    ///
    /// Parity: Go `w.Ent.UsageLog.Query().Where(usagelog.CreatedAtLT(cutoff))
    /// .Count(ctx)` (`gc.go` line 654).
    async fn count_usage_logs_older_than(&self, cutoff_at: DateTime<Utc>) -> Result<i64, String>;
}

/// Compose [`preview_plan`] with real COUNT queries, mirroring Go
/// `PreviewCleanup` (`gc.go` lines 632-667) end-to-end.
///
/// Order of operations matches Go exactly: build the shape (requests then
/// usage_logs — `gc.go` lines 638-664), then for each item run the COUNT
/// query for its resource type and fill in `estimated_count`. A COUNT error
/// aborts the whole preview (Go: `return nil, fmt.Errorf(...)`, `gc.go`
/// lines 642, 656).
///
/// This helper performs NO writes — Go's preview path queries `*.Count`,
/// never `*.Delete`, and the Rust trait surface preserves that.
pub async fn preview_with_counts(
    input: &TriggerGcCleanupInput,
    now: DateTime<Utc>,
    counter: &dyn GcPreviewCounter,
) -> Result<Vec<GcCleanupPreviewItem>, String> {
    let mut items = preview_plan(input, now);
    for item in &mut items {
        let count = match item.resource_type.as_str() {
            // Go `case` ladder in `PreviewCleanup` (gc.go lines 640, 654).
            // `preview_plan` only emits these two tags, so other strings are
            // unreachable here — defensive fallback is 0 to keep Go's
            // "emit the item with whatever count we have" shape.
            "requests" => counter.count_requests_older_than(item.cutoff_time).await?,
            "usage_logs" => {
                counter
                    .count_usage_logs_older_than(item.cutoff_time)
                    .await?
            }
            _ => 0,
        };
        item.estimated_count = count;
    }
    Ok(items)
}

// =========================================================================
// S09 — PostgreSQL VACUUM execution decision layer
// (Hilbert-the-11th).
//
// The active Rust product only opens PostgreSQL pools. Regular `VACUUM` is
// non-blocking; `VACUUM FULL` takes an exclusive lock and is used only when
// explicitly configured. Both execute on the scheduled GC path, never in a
// request handler.
// =========================================================================

/// Why the vacuum step was (or was not) taken.
///
/// `Disabled` avoids touching the pool; `Executed` carries the exact SQL sent
/// to PostgreSQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VacuumOutcome {
    /// `GcConfig::vacuum_enabled == false`.
    Disabled,
    /// VACUUM was executed successfully. Carries the exact SQL sent to the
    /// PostgreSQL pool so callers can log it.
    Executed { sql: &'static str },
}

/// The fully-resolved VACUUM decision: skip-reason or the SQL to execute.
///
/// The only skip condition is `vacuum_enabled=false`; otherwise SQL selection
/// follows PostgreSQL's regular/full modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VacuumDecision {
    pub outcome: VacuumOutcome,
}

impl VacuumDecision {
    /// The SQL to execute, if any. `None` for skip outcomes.
    pub fn sql(&self) -> Option<&'static str> {
        match self.outcome {
            VacuumOutcome::Executed { sql } => Some(sql),
            _ => None,
        }
    }

    /// Whether this decision represents a skip (no SQL emitted).
    pub fn is_skip(&self) -> bool {
        !matches!(self.outcome, VacuumOutcome::Executed { .. })
    }
}

/// Resolve the PostgreSQL VACUUM decision before consulting the executor.
pub fn decide_vacuum(config: &GcConfig) -> VacuumDecision {
    if !config.vacuum_enabled {
        return VacuumDecision {
            outcome: VacuumOutcome::Disabled,
        };
    }
    VacuumDecision {
        outcome: VacuumOutcome::Executed {
            sql: select_vacuum_sql(config.vacuum_full),
        },
    }
}

/// Executor seam for the actual VACUUM SQL.
///
/// Parity: Go `sqlDriver.ExecContext(ctx, vacuumSQL)` (`gc.go` line 601).
/// The wired adapter translates to whatever sqlx/ent handle the runtime
/// holds; tests inject a recorder.
#[async_trait]
pub trait VacuumExecutor: Send + Sync {
    /// Execute the given SQL statement.
    ///
    /// Mirrors Go's `(*entsql.Driver).ExecContext` — a single statement, no
    /// args. Errors propagate (Go wraps with `failed to execute %s`,
    /// `gc.go` line 602 — the wired adapter formats the same way).
    async fn exec(&self, sql: &'static str) -> Result<(), String>;
}

/// Drive the full Go `runVacuum` flow against an executor.
///
/// Decide first, then execute the selected PostgreSQL maintenance statement.
/// Disabled maintenance is a clean `Ok` without touching the executor.
pub async fn run_vacuum(
    config: &GcConfig,
    executor: &dyn VacuumExecutor,
) -> Result<VacuumOutcome, String> {
    let decision = decide_vacuum(config);
    match decision.outcome {
        VacuumOutcome::Executed { sql } => {
            executor.exec(sql).await?;
            Ok(VacuumOutcome::Executed { sql })
        }
        other => Ok(other),
    }
}

// =========================================================================
// S10 — cascade delete (executions → requests) decision layer
// (Hilbert-the-11th).
//
// Mirrors Go `conduit/internal/server/gc/gc.go` `cleanupRequests`
// (lines 209-237), `cleanupOldRequestExecutions` (lines 239-286), and
// `cleanupOldRequestsRecords` (lines 288-328). The cascade is:
//   1. Delete child `request_execution` rows first (`gc.go` line 217, the
//      `cleanupOldRequestExecutions` call) — each batch FIRST walks per-row
//      external-storage cleanup (`cleanupExecutionExternalStorage`,
//      `gc.go` line 268) BEFORE the batch DB delete.
//   2. THEN delete the parent `request` rows (`gc.go` line 227, the
//      `cleanupOldRequestsRecords` call) — same per-row external-storage
//      pattern (`cleanupRequestExternalStorage`, `gc.go` line 315).
//
// Foreign-key / soft-delete semantics (S10 spec): Go runs the cleanup under
// `schematype.SkipSoftDelete(ctx)` (`gc.go` line 114) so rows are
// PHYSICALLY deleted, INCLUDING already-soft-deleted ones. The Rust
// `RequestContext` carries the same skip-soft-delete flag at the wired
// layer; the cascade trait below is itself agnostic to that — the caller
// (the wired GC worker) is responsible for setting the request context's
// soft-delete mode before calling [`run_request_cascade`].
//
// Per-batch, per-row ordering (load-bearing, mirrors Go): external storage
// cleanup happens row-by-row BEFORE the batch DELETE because once the row
// is gone Go can no longer read its `data_storage_id` / ids needed to
// compute object keys (`gc.go` lines 266-274 / 313-320). Reversing the
// order would orphan external-storage artifacts.
// =========================================================================

/// The two ordered phases of Go `cleanupRequests` (`gc.go` lines 217-234).
///
/// Order is load-bearing for FK-backed schemas: executions reference
/// requests, so executions must go first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestCascadePhase {
    /// `cleanupOldRequestExecutions` (`gc.go` lines 239-286). Per row the
    /// [`GcRequestExecutionRow`] is fed to
    /// [`cleanup_execution_external_storage`] BEFORE the batch DELETE.
    Executions,
    /// `cleanupOldRequestsRecords` (`gc.go` lines 288-328). Per row the
    /// [`GcRequestRow`] is fed to [`cleanup_request_external_storage`]
    /// BEFORE the batch DELETE.
    Requests,
}

/// The ordered cascade phases Go executes for a `"requests"` cleanup arm.
///
/// Parity: Go `cleanupRequests` (`gc.go` lines 217-234) — `executions`
/// first, `requests` second.
pub const REQUEST_CASCADE_PHASES: [RequestCascadePhase; 2] = [
    RequestCascadePhase::Executions,
    RequestCascadePhase::Requests,
];

/// DB-side operations the cascade walk needs, as a DI seam.
///
/// Each method mirrors one Go `Ent` call inside the batch loops:
/// * `list_old_executions` — `RequestExecution.Query().Select(id, project_id,
///   data_storage_id, request_id).Where(CreatedAtLT(cutoff)).Order(Asc(id))
///   .Limit(batchSize).All(ctx)` (`gc.go` lines 245-258).
/// * `delete_executions_by_ids` — `RequestExecution.Delete().Where(IDIn(ids))
///   .Exec(ctx)` (`gc.go` lines 271-275). Returns the count of rows deleted.
/// * `list_old_requests` / `delete_requests_by_ids` — the symmetric pair on
///   `Request` (`gc.go` lines 294-327).
///
/// Soft-delete handling: Go wraps the whole run in
/// `schematype.SkipSoftDelete(ctx)` (`gc.go` line 114). The trait does NOT
/// re-establish that context — the wired adapter is expected to honour the
/// `RequestContext` it is passed (which carries the skip-soft-delete flag).
#[async_trait]
pub trait GcCascadeRepo: Send + Sync {
    /// Read the next batch of stale executions.
    async fn list_old_executions(
        &self,
        ctx: &RequestContext,
        cutoff_at: DateTime<Utc>,
        batch_size: u32,
    ) -> GcServiceResult<Vec<GcRequestExecutionRow>>;

    /// Delete the given executions by id. Returns the number of rows
    /// deleted (Go returns the count from `Exec`).
    async fn delete_executions_by_ids(
        &self,
        ctx: &RequestContext,
        ids: &[i64],
    ) -> GcServiceResult<u64>;

    /// Read the next batch of stale requests.
    async fn list_old_requests(
        &self,
        ctx: &RequestContext,
        cutoff_at: DateTime<Utc>,
        batch_size: u32,
    ) -> GcServiceResult<Vec<GcRequestRow>>;

    /// Delete the given requests by id. Returns the number of rows deleted.
    async fn delete_requests_by_ids(
        &self,
        ctx: &RequestContext,
        ids: &[i64],
    ) -> GcServiceResult<u64>;
}

/// Per-phase outcome of a cascade walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CascadePhaseReport {
    /// Rows the DB reported as deleted by the batch DELETE calls.
    pub deleted_rows: u64,
    /// External-storage cleanup reports, one per row that actually had its
    /// storage keys walked. Rows with `data_storage_id == 0` (no external
    /// storage) produce an all-default report and are still listed here so
    /// the caller can correlate counts with row ids.
    pub storage_reports: Vec<GcExternalCleanupReport>,
}

/// Full cascade outcome — executions phase then requests phase, in Go order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CascadeReport {
    pub executions: CascadePhaseReport,
    pub requests: CascadePhaseReport,
}

/// Drive Go's full `cleanupRequests` cascade: executions phase, then
/// requests phase, in that exact order.
///
/// Parity: Go `cleanupRequests` (`gc.go` lines 209-237):
/// * `cleanupDays <= 0` → no-op (`gc.go` lines 210-213); this helper
///   expects a resolved `cutoff_at` from [`cleanup_cutoff`] and assumes the
///   caller has already done the `<= 0` guard.
/// * `cleanupOldRequestExecutions(ctx, cutoffTime)` runs first (`gc.go`
///   line 217); the shared DataStorage cache (`gc.go` line 242) is created
///   here and reused across both phases.
/// * `cleanupOldRequestsRecords(ctx, cutoffTime)` runs second (`gc.go` line
///   227), sharing the same cache.
///
/// Each phase uses the [`DEFAULT_GC_BATCH_SIZE`] constant as the page size,
/// mirroring Go's `w.getBatchSize()` (`gc.go` lines 240, 289).
pub async fn run_request_cascade(
    ctx: &RequestContext,
    repo: &dyn GcCascadeRepo,
    storage: &dyn GcExternalStorage,
    cutoff_at: DateTime<Utc>,
) -> Result<CascadeReport, GcServiceError> {
    let batch_size = DEFAULT_GC_BATCH_SIZE;
    let mut cache = GcDataStorageCache::new();
    let mut report = CascadeReport::default();

    // ----- Phase 1: executions (`gc.go` lines 239-286) -----------------
    for _ in 0..100_000 {
        let batch = repo.list_old_executions(ctx, cutoff_at, batch_size).await?;
        if batch.is_empty() {
            break;
        }
        // Per-row external-storage cleanup BEFORE batch DELETE (`gc.go`
        // lines 266-269).
        for exec in &batch {
            let one = cleanup_execution_external_storage(storage, exec, &mut cache).await;
            report.executions.storage_reports.push(one);
        }
        let ids: Vec<i64> = batch.iter().map(|r| r.id).collect();
        let deleted = repo.delete_executions_by_ids(ctx, &ids).await?;
        report.executions.deleted_rows += deleted;
    }

    // ----- Phase 2: requests (`gc.go` lines 288-328) -------------------
    for _ in 0..100_000 {
        let batch = repo.list_old_requests(ctx, cutoff_at, batch_size).await?;
        if batch.is_empty() {
            break;
        }
        for req in &batch {
            let one = cleanup_request_external_storage(storage, req, &mut cache).await;
            report.requests.storage_reports.push(one);
        }
        let ids: Vec<i64> = batch.iter().map(|r| r.id).collect();
        let deleted = repo.delete_requests_by_ids(ctx, &ids).await?;
        report.requests.deleted_rows += deleted;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use conduit_db::{PolicyContext, Principal};
    use tokio::sync::Mutex;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum GcCall {
        Stale(GcTarget),
        SoftDeleted(GcTarget),
    }

    #[derive(Debug, Clone, Copy)]
    enum FakeResult {
        Ok(u64),
        NotImplemented(&'static str),
    }

    impl FakeResult {
        fn into_service_result(self) -> GcServiceResult<u64> {
            match self {
                Self::Ok(rows) => Ok(rows),
                Self::NotImplemented(name) => {
                    Err(GcServiceError::Repo(RepoError::NotImplemented(name)))
                }
            }
        }
    }

    struct FakeGcRepo {
        calls: Mutex<Vec<GcCall>>,
        stale_result: FakeResult,
        soft_deleted_result: FakeResult,
    }

    impl FakeGcRepo {
        fn new(stale_result: FakeResult, soft_deleted_result: FakeResult) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                stale_result,
                soft_deleted_result,
            }
        }

        async fn calls(&self) -> Vec<GcCall> {
            self.calls.lock().await.clone()
        }
    }

    #[async_trait]
    impl GcRepo for FakeGcRepo {
        async fn cleanup_stale_requests(
            &self,
            _ctx: &RequestContext,
            target: &GcTarget,
        ) -> GcServiceResult<u64> {
            self.calls.lock().await.push(GcCall::Stale(target.clone()));
            self.stale_result.into_service_result()
        }

        async fn cleanup_soft_deleted_retention(
            &self,
            _ctx: &RequestContext,
            target: &GcTarget,
        ) -> GcServiceResult<u64> {
            self.calls
                .lock()
                .await
                .push(GcCall::SoftDeleted(target.clone()));
            self.soft_deleted_result.into_service_result()
        }
    }

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn fixed_now() -> Result<DateTime<Utc>, chrono::ParseError> {
        Ok(DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")?.with_timezone(&Utc))
    }

    #[tokio::test]
    async fn stale_request_cleanup_computes_cutoff_and_honors_limit()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(FakeGcRepo::new(FakeResult::Ok(3), FakeResult::Ok(0)));
        let service = GcService::new(repo.clone());
        let report = service
            .cleanup_stale_requests(&ctx(), fixed_now()?, Duration::hours(6), 25)
            .await?;

        let expected_target = GcTarget {
            kind: GcTargetKind::StaleRequests,
            cutoff_at: DateTime::parse_from_rfc3339("2026-06-24T06:00:00Z")?.with_timezone(&Utc),
            limit: 25,
        };
        assert_eq!(report.target, expected_target);
        assert_eq!(report.deleted_rows, 3);
        assert_eq!(repo.calls().await, vec![GcCall::Stale(expected_target)]);
        Ok(())
    }

    #[tokio::test]
    async fn soft_deleted_cleanup_computes_retention_cutoff_and_honors_limit()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(FakeGcRepo::new(FakeResult::Ok(0), FakeResult::Ok(7)));
        let service = GcService::new(repo.clone());
        let report = service
            .cleanup_soft_deleted_retention(&ctx(), fixed_now()?, Duration::days(30), 10)
            .await?;

        let expected_target = GcTarget {
            kind: GcTargetKind::SoftDeletedRetention,
            cutoff_at: DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")?.with_timezone(&Utc),
            limit: 10,
        };
        assert_eq!(report.target, expected_target);
        assert_eq!(report.deleted_rows, 7);
        assert_eq!(
            repo.calls().await,
            vec![GcCall::SoftDeleted(expected_target)]
        );
        Ok(())
    }

    #[tokio::test]
    async fn repo_error_is_returned_without_panic() -> Result<(), Box<dyn std::error::Error>> {
        let repo = Arc::new(FakeGcRepo::new(
            FakeResult::NotImplemented("gc stale request cleanup"),
            FakeResult::Ok(0),
        ));
        let service = GcService::new(repo.clone());

        let result = service
            .cleanup_stale_requests(&ctx(), fixed_now()?, Duration::minutes(5), 1)
            .await;

        assert!(matches!(
            result,
            Err(GcServiceError::Repo(RepoError::NotImplemented(
                "gc stale request cleanup"
            )))
        ));
        assert_eq!(repo.calls().await.len(), 1);
        Ok(())
    }

    // ====================================================================
    // Resource types, cleanup options, and config parity (Kant-the-2nd)
    //
    // Mirrors Go `conduit/internal/server/gc/gc.go` (defaultBatchSize,
    // TriggerGcCleanupInput, GcCleanupPreviewItem, Config, runCleanup's
    // manual-days resolution, deleteInBatches, runVacuum) and
    // `biz/system.go` CleanupOption + `biz/system_default.go` defaults.
    // ====================================================================

    /// Parity: Go `gc.defaultBatchSize = 500` (`gc.go` line 26).
    #[test]
    fn gc_default_batch_size_matches_go_constant() {
        assert_eq!(DEFAULT_GC_BATCH_SIZE, 500);
    }

    /// Parity: Go channel-probe retention is hard-coded to 3 days
    /// (`gc.go` line 189: `w.cleanupChannelProbes(ctx, 3, manual)`).
    #[test]
    fn gc_channel_probe_retention_days_is_hardcoded_three() {
        assert_eq!(CHANNEL_PROBE_RETENTION_DAYS, 3);
    }

    /// Parity: Go wire string tags for the two policy-driven resource types
    /// (`"requests"`, `"usage_logs"`). These strings are the
    /// `CleanupOption.ResourceType` values and the `manualDays` map keys.
    #[test]
    fn gc_resource_type_wire_strings_match_go() {
        assert_eq!(GcResourceType::Requests.as_str(), "requests");
        assert_eq!(GcResourceType::UsageLogs.as_str(), "usage_logs");
    }

    /// Parity: Go `defaultStoragePolicy.CleanupOptions` (`system_default.go`
    /// lines 8-19) — requests days=3 disabled, usage_logs days=30 disabled.
    /// Both default to **disabled**, mirroring the Go "GC off by default"
    /// security posture.
    #[test]
    fn gc_cleanup_option_defaults_match_go_system_default() {
        let defaults = CleanupOption::defaults();
        assert_eq!(defaults.len(), 2);

        assert_eq!(defaults[0].resource_type, "requests");
        assert!(!defaults[0].enabled);
        assert_eq!(defaults[0].cleanup_days, 3);

        assert_eq!(defaults[1].resource_type, "usage_logs");
        assert!(!defaults[1].enabled);
        assert_eq!(defaults[1].cleanup_days, 30);
    }

    /// Parity: Go `CleanupOption` JSON tags (`system.go` lines 282-284):
    /// `resource_type` / `enabled` / `cleanup_days` snake_case.
    #[test]
    fn gc_cleanup_option_round_trips_go_snake_case_tags() -> Result<(), serde_json::Error> {
        let option = CleanupOption {
            resource_type: "requests".to_string(),
            enabled: true,
            cleanup_days: 7,
        };
        let serialized = serde_json::to_string(&option)?;
        assert!(serialized.contains("\"resource_type\":\"requests\""));
        assert!(serialized.contains("\"enabled\":true"));
        assert!(serialized.contains("\"cleanup_days\":7"));

        let parsed: CleanupOption = serde_json::from_str(&serialized)?;
        assert_eq!(parsed, option);
        Ok(())
    }

    /// Parity: Go `TriggerGcCleanupInput` (`gc.go` lines 28-31) snake_case
    /// tags + zero defaults.
    #[test]
    fn gc_trigger_input_round_trips_go_snake_case_tags() -> Result<(), serde_json::Error> {
        let input = TriggerGcCleanupInput {
            requests_cleanup_days: 5,
            usage_logs_cleanup_days: 60,
        };
        let serialized = serde_json::to_string(&input)?;
        assert!(serialized.contains("\"requests_cleanup_days\":5"));
        assert!(serialized.contains("\"usage_logs_cleanup_days\":60"));

        let parsed: TriggerGcCleanupInput = serde_json::from_str(&serialized)?;
        assert_eq!(parsed, input);

        // Default is all-zero (Go zero-value struct).
        assert_eq!(
            TriggerGcCleanupInput::default(),
            TriggerGcCleanupInput {
                requests_cleanup_days: 0,
                usage_logs_cleanup_days: 0,
            }
        );
        Ok(())
    }

    /// Parity: Go `cleanup_cutoff = time.Now().AddDate(0, 0, -cleanupDays)`
    /// (`gc.go` lines 215, 426, 462, 498, 534, 639, 653). The `<= 0` guard
    /// (`gc.go` lines 210, 422, 458, 494, 530) makes non-positive days a
    /// no-op.
    #[test]
    fn gc_cleanup_cutoff_subtracts_days_from_now() -> Result<(), chrono::ParseError> {
        let now = DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")?.with_timezone(&Utc);

        // 7 days back.
        let cutoff = cleanup_cutoff(now, 7);
        assert_eq!(
            cutoff,
            Some(DateTime::parse_from_rfc3339("2026-06-17T12:00:00Z")?.with_timezone(&Utc))
        );

        // 30 days back.
        let cutoff = cleanup_cutoff(now, 30);
        assert_eq!(
            cutoff,
            Some(DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")?.with_timezone(&Utc))
        );
        Ok(())
    }

    #[test]
    fn gc_cleanup_cutoff_none_when_days_non_positive() {
        let now = Utc::now();
        assert_eq!(cleanup_cutoff(now, 0), None);
        assert_eq!(cleanup_cutoff(now, -1), None);
        assert_eq!(cleanup_cutoff(now, -100), None);
    }

    /// Parity: Go `runCleanup` outer guard (`gc.go` lines 125-130) —
    /// automatic mode runs iff `option.Enabled`; manual mode runs iff the
    /// resource type appears in `manualDays`.
    #[test]
    fn gc_should_run_cleanup_automatic_mode_requires_enabled_flag() {
        let mut option = CleanupOption {
            resource_type: "requests".to_string(),
            enabled: false,
            cleanup_days: 3,
        };
        let empty_manual = BTreeMap::new();
        // Disabled option does NOT run in automatic mode.
        assert!(!should_run_cleanup(&option, false, &empty_manual));

        option.enabled = true;
        // Enabled option runs in automatic mode; manual_days is ignored.
        assert!(should_run_cleanup(&option, false, &empty_manual));
    }

    #[test]
    fn gc_should_run_cleanup_manual_mode_requires_resource_in_manual_days() {
        let option = CleanupOption {
            resource_type: "requests".to_string(),
            enabled: false, // even disabled options run in manual mode if listed
            cleanup_days: 3,
        };
        let mut manual = BTreeMap::new();
        // Not in manual_days → skip.
        assert!(!should_run_cleanup(&option, true, &manual));

        manual.insert("requests".to_string(), 7);
        // In manual_days → run, regardless of `enabled`.
        assert!(should_run_cleanup(&option, true, &manual));

        // A different resource type in the map does NOT cause this option to run.
        let option_other = CleanupOption {
            resource_type: "usage_logs".to_string(),
            enabled: true,
            cleanup_days: 30,
        };
        assert!(!should_run_cleanup(&option_other, true, &manual));
    }

    /// Parity: Go days-resolution (`gc.go` lines 131-136) — manual override
    /// wins when present, otherwise policy value is used.
    #[test]
    fn gc_effective_cleanup_days_manual_override_wins() {
        let option = CleanupOption {
            resource_type: "requests".to_string(),
            enabled: true,
            cleanup_days: 3,
        };
        let empty_manual = BTreeMap::new();
        let mut manual = BTreeMap::new();
        manual.insert("requests".to_string(), 14);

        // Automatic mode: policy value.
        assert_eq!(effective_cleanup_days(&option, false, &empty_manual), 3);
        // Manual mode without override: policy value (Go falls through the
        // `if d, ok := ...` when the key is absent).
        assert_eq!(effective_cleanup_days(&option, true, &empty_manual), 3);
        // Manual mode with override: override wins.
        assert_eq!(effective_cleanup_days(&option, true, &manual), 14);
    }

    /// Parity: Go `RunCleanupNow` (`gc.go` lines 619-627) — only days > 0
    /// are inserted into the `manualDays` map; zero/negative are dropped.
    #[test]
    fn gc_resolve_manual_days_drops_non_positive_entries() {
        let both_positive = TriggerGcCleanupInput {
            requests_cleanup_days: 5,
            usage_logs_cleanup_days: 60,
        };
        let map = resolve_manual_days(&both_positive);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("requests"), Some(&5));
        assert_eq!(map.get("usage_logs"), Some(&60));

        let one_zero = TriggerGcCleanupInput {
            requests_cleanup_days: 5,
            usage_logs_cleanup_days: 0, // dropped
        };
        let map = resolve_manual_days(&one_zero);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("requests"), Some(&5));
        assert!(!map.contains_key("usage_logs"));

        let all_zero = TriggerGcCleanupInput::default();
        assert!(resolve_manual_days(&all_zero).is_empty());
    }

    /// Parity: Go `PreviewCleanup` (`gc.go` lines 632-667) — only resources
    /// with days > 0 are emitted; the order is requests-then-usage_logs;
    /// cutoff and retention_days are fully determined by the input + now.
    #[test]
    fn gc_preview_plan_emits_only_positive_days_resources_in_order()
    -> Result<(), chrono::ParseError> {
        let now = DateTime::parse_from_rfc3339("2026-06-24T12:00:00Z")?.with_timezone(&Utc);

        // Both resources requested.
        let input = TriggerGcCleanupInput {
            requests_cleanup_days: 3,
            usage_logs_cleanup_days: 30,
        };
        let items = preview_plan(&input, now);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].resource_type, "requests");
        assert_eq!(items[0].retention_days, 3);
        assert_eq!(items[0].estimated_count, 0); // wired layer fills this in.
        assert_eq!(
            items[0].cutoff_time,
            DateTime::parse_from_rfc3339("2026-06-21T12:00:00Z")?.with_timezone(&Utc)
        );
        assert_eq!(items[1].resource_type, "usage_logs");
        assert_eq!(items[1].retention_days, 30);
        assert_eq!(
            items[1].cutoff_time,
            DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")?.with_timezone(&Utc)
        );
        Ok(())
    }

    #[test]
    fn gc_preview_plan_skips_resources_with_non_positive_days() {
        let now = Utc::now();
        // Only usage_logs has positive days.
        let input = TriggerGcCleanupInput {
            requests_cleanup_days: 0,
            usage_logs_cleanup_days: 14,
        };
        let items = preview_plan(&input, now);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].resource_type, "usage_logs");

        // Empty input → empty preview.
        assert!(preview_plan(&TriggerGcCleanupInput::default(), now).is_empty());
    }

    /// PostgreSQL uses regular VACUUM by default and VACUUM FULL only when
    /// explicitly configured.
    #[test]
    fn gc_select_vacuum_sql_matches_postgres_modes() {
        assert_eq!(select_vacuum_sql(false), "VACUUM");
        assert_eq!(select_vacuum_sql(true), "VACUUM FULL");
    }

    /// Parity: Go `deleteInBatches` (`gc.go` lines 83-101). Mirrors the
    /// golden intent of `TestWorker_deleteInBatches` (`gc_test.go` lines
    /// 207-242): three calls returning (30, 15, 0) sum to 45, and the loop
    /// stops after the third call.
    #[test]
    fn gc_delete_in_batches_sums_deletes_and_stops_on_zero() {
        let calls = std::cell::RefCell::new(Vec::<u32>::new());
        let sequence = [30u32, 15, 0];
        let idx = std::cell::Cell::new(0usize);
        let total = delete_in_batches::<std::convert::Infallible>(|| {
            let i = idx.get();
            let v = sequence[i];
            idx.set(i + 1);
            calls.borrow_mut().push(v);
            Ok(v)
        })
        .unwrap_or(0);

        assert_eq!(total, 45);
        assert_eq!(*calls.borrow(), vec![30, 15, 0]);
    }

    /// Mirrors the second golden intent of `TestWorker_deleteInBatches`: a
    /// closure that immediately returns 0 stops the loop after a single
    /// invocation with a zero total.
    #[test]
    fn gc_delete_in_batches_zero_first_call_yields_zero_total() {
        let mut invoked = 0u32;
        let total = delete_in_batches::<std::convert::Infallible>(|| {
            invoked += 1;
            Ok(0)
        })
        .unwrap_or(0);
        assert_eq!(total, 0);
        assert_eq!(invoked, 1);
    }

    /// Parity: Go's `if err != nil { return totalDeleted, ... }` early-exit
    /// (`gc.go` lines 87-89). A propagated error aborts the loop and the
    /// partial total seen so far is dropped (Go returns the error, the
    /// caller logs and skips this resource).
    #[test]
    fn gc_delete_in_batches_propagates_error_and_aborts() {
        let sequence = [Ok(10u32), Err("db down"), Ok(20)];
        let idx = std::cell::Cell::new(0usize);
        let result: Result<u64, &str> = delete_in_batches(|| {
            let i = idx.get();
            idx.set(i + 1);
            sequence[i]
        });
        assert_eq!(result, Err("db down"));
    }

    // ====================================================================
    // S05 GC cron + S07 external-storage cleanup parity (Hilbert-the-8th)
    //
    // Mirrors Go `conduit/internal/server/gc/gc.go` (RegisterScheduledTasks,
    // runCleanup ladder, cleanup{Request,Execution}ExternalStorage,
    // getDataStorageCached), `gc_internal.go` (runAutomaticCleanup),
    // `conf/conf.go` GC viper defaults, and the fs-artifact golden cases in
    // `gc_test.go`.
    // ====================================================================

    /// Parity: Go `RegisterScheduledTasks` TaskSpec values (`gc.go` lines
    /// 73-80) — name "gc", verbatim description, cron passthrough from
    /// config, timezone pinned to "UTC".
    #[test]
    fn gc_task_spec_mirrors_go_register_scheduled_tasks() {
        let config = GcConfig {
            cron: "*/5 * * * *".to_string(),
            vacuum_enabled: true,
            vacuum_full: false,
        };
        let spec = gc_task_spec(&config);
        assert_eq!(spec.name, "gc");
        assert_eq!(
            spec.description,
            "Garbage collection — cleanup old requests, traces, usage logs, and channel probes"
        );
        assert_eq!(spec.cron_expr, "*/5 * * * *");
        assert_eq!(spec.timezone, "UTC");
    }

    /// Parity: Go viper GC defaults (`conf/conf.go` lines 228-230) —
    /// cron "0 2 * * *" (daily 02:00), vacuum enabled, vacuum-full off.
    #[test]
    fn gc_config_conf_defaults_match_go_viper_defaults() {
        let config = GcConfig::conf_default();
        assert_eq!(config.cron, "0 2 * * *");
        assert!(config.vacuum_enabled);
        assert!(!config.vacuum_full);
        assert_eq!(config.validate(), Ok(()));
    }

    /// Parity: Go `Config.CRON` carries `validate:"required"` (`gc.go` line
    /// 41) — the zero value (empty string) is rejected; nothing else is.
    #[test]
    fn gc_config_validate_requires_cron() {
        let missing = GcConfig::default();
        assert_eq!(missing.validate(), Err(GcConfigError::MissingCron));

        let ok = GcConfig {
            cron: "0 2 * * *".to_string(),
            ..GcConfig::default()
        };
        assert_eq!(ok.validate(), Ok(()));
    }

    /// Parity: Go `gc_internal.go` line 10 — the automatic entry runs under
    /// `authz.WithSystemBypass(ctx, "gc-cleanup")`.
    #[test]
    fn gc_system_bypass_reason_matches_go() {
        assert_eq!(GC_SYSTEM_BYPASS_REASON, "gc-cleanup");
    }

    /// Parity: Go `runCleanup` with the default (all-disabled) policy in
    /// automatic mode — no policy arm fires, but the channel-probe tail
    /// (`gc.go` line 189) and the vacuum flag (`gc.go` line 198) still apply.
    #[test]
    fn gc_run_plan_automatic_disabled_policy_only_channel_probes()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let plan =
            build_automatic_gc_run_plan(&CleanupOption::defaults(), &GcConfig::conf_default(), now);

        assert_eq!(
            plan.steps,
            vec![GcRunStep {
                resource: GcRunResource::ChannelProbes,
                cutoff_at: now - Duration::days(3),
                retention_days: 3,
            }]
        );
        assert!(plan.run_vacuum); // conf_default has vacuum_enabled = true.
        assert!(plan.unknown_resources.is_empty());
        Ok(())
    }

    /// Parity: Go `case "requests"` (`gc.go` lines 138-170) — one enabled
    /// requests option expands to requests → threads → traces on the same
    /// cutoff, followed by the channel-probe tail.
    #[test]
    fn gc_run_plan_automatic_requests_expands_to_requests_threads_traces()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let options = vec![
            CleanupOption {
                resource_type: "requests".to_string(),
                enabled: true,
                cleanup_days: 7,
            },
            CleanupOption {
                resource_type: "usage_logs".to_string(),
                enabled: false,
                cleanup_days: 30,
            },
        ];
        let plan = build_automatic_gc_run_plan(&options, &GcConfig::conf_default(), now);

        let cutoff = now - Duration::days(7);
        let resources: Vec<GcRunResource> = plan.steps.iter().map(|s| s.resource).collect();
        assert_eq!(
            resources,
            vec![
                GcRunResource::Requests,
                GcRunResource::Threads,
                GcRunResource::Traces,
                GcRunResource::ChannelProbes,
            ]
        );
        for step in &plan.steps[..3] {
            assert_eq!(step.cutoff_at, cutoff);
            assert_eq!(step.retention_days, 7);
        }
        Ok(())
    }

    /// Parity: Go `case "usage_logs"` (`gc.go` lines 171-181) — a single
    /// step, then the probe tail.
    #[test]
    fn gc_run_plan_automatic_usage_logs_single_step() -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let options = vec![
            CleanupOption {
                resource_type: "requests".to_string(),
                enabled: false,
                cleanup_days: 3,
            },
            CleanupOption {
                resource_type: "usage_logs".to_string(),
                enabled: true,
                cleanup_days: 14,
            },
        ];
        let plan = build_automatic_gc_run_plan(&options, &GcConfig::conf_default(), now);

        let resources: Vec<GcRunResource> = plan.steps.iter().map(|s| s.resource).collect();
        assert_eq!(
            resources,
            vec![GcRunResource::UsageLogs, GcRunResource::ChannelProbes]
        );
        assert_eq!(plan.steps[0].cutoff_at, now - Duration::days(14));
        assert_eq!(plan.steps[0].retention_days, 14);
        Ok(())
    }

    /// Parity: Go manual mode (`gc.go` lines 125-136 + `RunCleanupNow` lines
    /// 619-629) — only resources named in the input run (the policy
    /// `enabled` flag is ignored), and the manual days override the policy
    /// days.
    #[test]
    fn gc_run_plan_manual_runs_only_named_resources_with_override_days()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        // Both options DISABLED — manual mode must still run "requests"
        // because it is named in the trigger input.
        let options = CleanupOption::defaults();
        let input = TriggerGcCleanupInput {
            requests_cleanup_days: 5,
            usage_logs_cleanup_days: 0, // not named → usage_logs skipped
        };
        let plan = build_manual_gc_run_plan(&options, &input, &GcConfig::conf_default(), now);

        let resources: Vec<GcRunResource> = plan.steps.iter().map(|s| s.resource).collect();
        assert_eq!(
            resources,
            vec![
                GcRunResource::Requests,
                GcRunResource::Threads,
                GcRunResource::Traces,
                GcRunResource::ChannelProbes,
            ]
        );
        // Override days (5) win over the policy value (3).
        assert_eq!(plan.steps[0].retention_days, 5);
        assert_eq!(plan.steps[0].cutoff_at, now - Duration::days(5));
        Ok(())
    }

    /// Parity: Go `RunCleanupNow` with an all-zero input builds an EMPTY
    /// `manualDays` map (`gc.go` lines 620-626), so every policy option is
    /// skipped via the membership check (`gc.go` lines 126-129) — only the
    /// probe tail remains.
    #[test]
    fn gc_run_plan_manual_empty_input_only_channel_probes() -> Result<(), Box<dyn std::error::Error>>
    {
        let now = fixed_now()?;
        let plan = build_manual_gc_run_plan(
            &CleanupOption::defaults(),
            &TriggerGcCleanupInput::default(),
            &GcConfig::conf_default(),
            now,
        );
        let resources: Vec<GcRunResource> = plan.steps.iter().map(|s| s.resource).collect();
        assert_eq!(resources, vec![GcRunResource::ChannelProbes]);
        Ok(())
    }

    /// Parity: Go `default:` arm (`gc.go` lines 182-184) — unknown resource
    /// types produce a warn (recorded here) and no steps.
    #[test]
    fn gc_run_plan_unknown_resource_recorded_and_skipped() -> Result<(), Box<dyn std::error::Error>>
    {
        let now = fixed_now()?;
        let options = vec![CleanupOption {
            resource_type: "payments".to_string(),
            enabled: true,
            cleanup_days: 10,
        }];
        let plan = build_automatic_gc_run_plan(&options, &GcConfig::conf_default(), now);

        assert_eq!(plan.unknown_resources, vec!["payments".to_string()]);
        let resources: Vec<GcRunResource> = plan.steps.iter().map(|s| s.resource).collect();
        assert_eq!(resources, vec![GcRunResource::ChannelProbes]);
        Ok(())
    }

    #[test]
    fn gc_run_plan_supports_independent_content_retention_resources()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let options = [
            "request_headers",
            "request_bodies",
            "response_bodies",
            "response_chunks",
        ]
        .into_iter()
        .map(|resource_type| CleanupOption {
            resource_type: resource_type.to_string(),
            enabled: true,
            cleanup_days: 7,
        })
        .collect::<Vec<_>>();
        let plan = build_automatic_gc_run_plan(&options, &GcConfig::conf_default(), now);
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.resource)
                .collect::<Vec<_>>(),
            vec![
                GcRunResource::RequestHeaders,
                GcRunResource::RequestBodies,
                GcRunResource::ResponseBodies,
                GcRunResource::ResponseChunks,
                GcRunResource::ChannelProbes,
            ]
        );
        assert!(plan.steps[..4].iter().all(|step| step.retention_days == 7));
        Ok(())
    }

    /// Mirrors the intent of Go `TestWorker_cleanupWithZeroDays`
    /// (`gc_test.go` lines 244-263) at the plan level: enabled options with
    /// `days <= 0` produce no steps (each Go helper no-ops on the guard,
    /// `gc.go` lines 210, 422).
    #[test]
    fn gc_run_plan_zero_days_enabled_option_emits_no_steps()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let options = vec![
            CleanupOption {
                resource_type: "requests".to_string(),
                enabled: true,
                cleanup_days: 0,
            },
            CleanupOption {
                resource_type: "usage_logs".to_string(),
                enabled: true,
                cleanup_days: -1,
            },
        ];
        let plan = build_automatic_gc_run_plan(&options, &GcConfig::conf_default(), now);
        let resources: Vec<GcRunResource> = plan.steps.iter().map(|s| s.resource).collect();
        assert_eq!(resources, vec![GcRunResource::ChannelProbes]);
        Ok(())
    }

    /// Parity: Go `runCleanup` vacuum tail is gated ONLY on
    /// `Config.VacuumEnabled` (`gc.go` lines 198-203).
    #[test]
    fn gc_run_plan_vacuum_flag_follows_config() -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let no_vacuum = GcConfig {
            cron: "0 2 * * *".to_string(),
            vacuum_enabled: false,
            vacuum_full: true, // irrelevant when disabled
        };
        let plan = build_automatic_gc_run_plan(&CleanupOption::defaults(), &no_vacuum, now);
        assert!(!plan.run_vacuum);
        Ok(())
    }

    // ----- S07 external-storage cleanup -------------------------------------

    /// Golden key order for a request row, values from Go
    /// `TestWorker_cleanupRequestExternalStorageDeletesFsArtifacts`
    /// (`gc_test.go` lines 52-67: ID=101, ProjectID=202) against the key
    /// slice in `gc.go` lines 386-392.
    #[test]
    fn gc_request_external_cleanup_keys_match_go_order() {
        assert_eq!(
            request_external_cleanup_keys(202, 101),
            vec![
                "/202/requests/101/request_body.json".to_string(),
                "/202/requests/101/response_body.json".to_string(),
                "/202/requests/101/response_chunks.json".to_string(),
                "/202/requests/101/executions".to_string(),
                "/202/requests/101".to_string(),
            ]
        );
    }

    /// Golden key order for an execution row, values from Go
    /// `TestWorker_cleanupExecutionExternalStorageDeletesFsArtifacts`
    /// (`gc_test.go` lines 87-108: request ID=303/ProjectID=404, exec
    /// ID=505) against the key slice in `gc.go` lines 349-354.
    #[test]
    fn gc_execution_external_cleanup_keys_match_go_order() {
        assert_eq!(
            execution_external_cleanup_keys(404, 303, 505),
            vec![
                "/404/requests/303/executions/505/request_body.json".to_string(),
                "/404/requests/303/executions/505/response_body.json".to_string(),
                "/404/requests/303/executions/505/response_chunks.json".to_string(),
                "/404/requests/303/executions/505".to_string(),
            ]
        );
    }

    /// In-memory stand-in for the Go fs-backed `DataStorageService` used by
    /// `setupWorkerWithFSStorage` (`gc_test.go` lines 125-177): a set of
    /// existing object keys plus recorded lookup/delete calls.
    struct FakeExternalStorage {
        storages: BTreeMap<i64, DataStorageRef>,
        lookup_error_ids: std::collections::BTreeSet<i64>,
        failing_keys: std::collections::BTreeSet<String>,
        objects: Mutex<std::collections::BTreeSet<String>>,
        lookups: Mutex<Vec<i64>>,
        deletes: Mutex<Vec<String>>,
    }

    impl FakeExternalStorage {
        fn new(storages: Vec<DataStorageRef>, objects: Vec<String>) -> Self {
            Self {
                storages: storages.into_iter().map(|ds| (ds.id, ds)).collect(),
                lookup_error_ids: std::collections::BTreeSet::new(),
                failing_keys: std::collections::BTreeSet::new(),
                objects: Mutex::new(objects.into_iter().collect()),
                lookups: Mutex::new(Vec::new()),
                deletes: Mutex::new(Vec::new()),
            }
        }

        fn with_lookup_error(mut self, id: i64) -> Self {
            self.lookup_error_ids.insert(id);
            self
        }

        fn with_failing_key(mut self, key: &str) -> Self {
            self.failing_keys.insert(key.to_string());
            self
        }

        async fn remaining_objects(&self) -> Vec<String> {
            self.objects.lock().await.iter().cloned().collect()
        }
    }

    #[async_trait]
    impl GcExternalStorage for FakeExternalStorage {
        async fn get_data_storage_by_id(&self, id: i64) -> Result<Option<DataStorageRef>, String> {
            self.lookups.lock().await.push(id);
            if self.lookup_error_ids.contains(&id) {
                // Shape of Go's wrapped error (`data_storage.go` line 291).
                return Err(format!("failed to get data storage by ID {id}"));
            }
            Ok(self.storages.get(&id).copied())
        }

        async fn delete_data(&self, _storage: &DataStorageRef, key: &str) -> Result<(), String> {
            self.deletes.lock().await.push(key.to_string());
            if self.failing_keys.contains(key) {
                return Err(format!("failed to remove file: {key}"));
            }
            // Missing keys are success — Go `DeleteData` maps os.ErrNotExist
            // to nil (`data_storage.go` lines 628-631).
            self.objects.lock().await.remove(key);
            Ok(())
        }
    }

    /// Mirrors Go `TestWorker_cleanupRequestExternalStorageDeletesFsArtifacts`
    /// (`gc_test.go` lines 49-82): request ID=101 / ProjectID=202 on a
    /// non-primary fs storage; after cleanup all three file keys and both
    /// dir keys are gone, deleted in Go's slice order.
    #[tokio::test]
    async fn gc_cleanup_request_external_storage_deletes_fs_artifacts()
    -> Result<(), Box<dyn std::error::Error>> {
        let expected_keys = request_external_cleanup_keys(202, 101);
        let storage =
            FakeExternalStorage::new(vec![DataStorageRef::external(9)], expected_keys.clone());
        let req = GcRequestRow {
            id: 101,
            project_id: 202,
            data_storage_id: 9,
        };
        let mut cache = GcDataStorageCache::new();

        let report = cleanup_request_external_storage(&storage, &req, &mut cache).await;

        assert_eq!(report.attempted_keys, expected_keys);
        assert!(report.delete_failures.is_empty());
        assert!(report.lookup_error.is_none());
        // Every artifact removed (Go asserts fs.ErrNotExist per key).
        assert!(storage.remaining_objects().await.is_empty());
        // Delete order is the Go slice order (files, executions dir, request
        // dir) — load-bearing for fs `Remove` of directories.
        assert_eq!(*storage.deletes.lock().await, expected_keys);
        Ok(())
    }

    /// Mirrors Go
    /// `TestWorker_cleanupExecutionExternalStorageDeletesFsArtifacts`
    /// (`gc_test.go` lines 84-123): exec ID=505 of request ID=303 /
    /// ProjectID=404; all three file keys plus the execution dir are gone.
    #[tokio::test]
    async fn gc_cleanup_execution_external_storage_deletes_fs_artifacts()
    -> Result<(), Box<dyn std::error::Error>> {
        let expected_keys = execution_external_cleanup_keys(404, 303, 505);
        let storage =
            FakeExternalStorage::new(vec![DataStorageRef::external(9)], expected_keys.clone());
        let exec = GcRequestExecutionRow {
            id: 505,
            project_id: 404,
            request_id: 303,
            data_storage_id: 9,
        };
        let mut cache = GcDataStorageCache::new();

        let report = cleanup_execution_external_storage(&storage, &exec, &mut cache).await;

        assert_eq!(report.attempted_keys, expected_keys);
        assert!(report.delete_failures.is_empty());
        assert!(report.lookup_error.is_none());
        assert!(storage.remaining_objects().await.is_empty());
        assert_eq!(*storage.deletes.lock().await, expected_keys);
        Ok(())
    }

    /// Parity: Go `ds.Primary` guard (`gc.go` lines 345, 382) — primary
    /// storage rows are skipped entirely (payloads live in DB columns).
    #[tokio::test]
    async fn gc_cleanup_skips_primary_storage() -> Result<(), Box<dyn std::error::Error>> {
        let keys = request_external_cleanup_keys(202, 101);
        let storage = FakeExternalStorage::new(vec![DataStorageRef::primary(9)], keys.clone());
        let req = GcRequestRow {
            id: 101,
            project_id: 202,
            data_storage_id: 9,
        };
        let mut cache = GcDataStorageCache::new();

        let report = cleanup_request_external_storage(&storage, &req, &mut cache).await;

        assert_eq!(report, GcExternalCleanupReport::default());
        assert!(storage.deletes.lock().await.is_empty());
        // Nothing removed — remaining_objects() is BTreeSet-sorted, so
        // compare against the sorted key list.
        let mut expected_remaining = keys.clone();
        expected_remaining.sort();
        assert_eq!(storage.remaining_objects().await, expected_remaining);
        Ok(())
    }

    /// Parity: Go `DataStorageID == 0` guard (`gc.go` lines 331, 368) — no
    /// lookup, no deletes.
    #[tokio::test]
    async fn gc_cleanup_skips_zero_data_storage_id() -> Result<(), Box<dyn std::error::Error>> {
        let storage = FakeExternalStorage::new(vec![DataStorageRef::external(9)], Vec::new());
        let req = GcRequestRow {
            id: 101,
            project_id: 202,
            data_storage_id: 0,
        };
        let mut cache = GcDataStorageCache::new();

        let report = cleanup_request_external_storage(&storage, &req, &mut cache).await;

        assert_eq!(report, GcExternalCleanupReport::default());
        assert!(storage.lookups.lock().await.is_empty());
        assert!(storage.deletes.lock().await.is_empty());
        Ok(())
    }

    /// Parity: Go lookup-failure path (`gc.go` lines 372-380) — warn + skip
    /// the row; the error is NOT cached (`gc.go` lines 410-413), so the next
    /// row retries the lookup.
    #[tokio::test]
    async fn gc_cleanup_lookup_error_warns_skips_and_is_not_cached()
    -> Result<(), Box<dyn std::error::Error>> {
        let storage = FakeExternalStorage::new(vec![DataStorageRef::external(9)], Vec::new())
            .with_lookup_error(9);
        let req = GcRequestRow {
            id: 101,
            project_id: 202,
            data_storage_id: 9,
        };
        let mut cache = GcDataStorageCache::new();

        let report = cleanup_request_external_storage(&storage, &req, &mut cache).await;
        assert_eq!(
            report.lookup_error.as_deref(),
            Some("failed to get data storage by ID 9")
        );
        assert!(report.attempted_keys.is_empty());
        assert!(cache.is_empty()); // NOT cached on error.

        // A second row hits the lookup again (Go's cache only stores
        // successes).
        let _ = cleanup_request_external_storage(&storage, &req, &mut cache).await;
        assert_eq!(*storage.lookups.lock().await, vec![9, 9]);
        Ok(())
    }

    /// Parity: Go per-key failure tolerance (`gc.go` lines 394-402) — a
    /// failing key is warned about and the remaining keys are still
    /// attempted; the batch never aborts.
    #[tokio::test]
    async fn gc_cleanup_delete_failure_continues_batch() -> Result<(), Box<dyn std::error::Error>> {
        let keys = request_external_cleanup_keys(202, 101);
        let failing = keys[1].clone(); // response_body.json fails
        let storage = FakeExternalStorage::new(vec![DataStorageRef::external(9)], keys.clone())
            .with_failing_key(&failing);
        let req = GcRequestRow {
            id: 101,
            project_id: 202,
            data_storage_id: 9,
        };
        let mut cache = GcDataStorageCache::new();

        let report = cleanup_request_external_storage(&storage, &req, &mut cache).await;

        // All five keys attempted despite the mid-list failure.
        assert_eq!(report.attempted_keys, keys);
        assert_eq!(
            report.delete_failures,
            vec![GcKeyDeleteFailure {
                key: failing.clone(),
                error: format!("failed to remove file: {failing}"),
            }]
        );
        // Only the failing key survives.
        assert_eq!(storage.remaining_objects().await, vec![failing]);
        Ok(())
    }

    /// Parity: Go `getDataStorageCached` (`gc.go` lines 405-418) — two rows
    /// sharing a storage id trigger exactly one lookup per batch cache.
    #[tokio::test]
    async fn gc_cleanup_caches_storage_lookup_across_rows() -> Result<(), Box<dyn std::error::Error>>
    {
        let objects = [
            request_external_cleanup_keys(202, 101),
            request_external_cleanup_keys(202, 102),
        ]
        .concat();
        let storage = FakeExternalStorage::new(vec![DataStorageRef::external(9)], objects);
        let mut cache = GcDataStorageCache::new();

        for id in [101, 102] {
            let req = GcRequestRow {
                id,
                project_id: 202,
                data_storage_id: 9,
            };
            let report = cleanup_request_external_storage(&storage, &req, &mut cache).await;
            assert!(report.delete_failures.is_empty());
        }

        assert_eq!(*storage.lookups.lock().await, vec![9]); // one lookup only
        assert!(storage.remaining_objects().await.is_empty());
        Ok(())
    }

    // ====================================================================
    // S06 — preview with real COUNT queries (Hilbert-the-11th).
    // Mirrors Go `PreviewCleanup` (`gc.go` lines 632-667).
    // ====================================================================

    struct FakePreviewCounter {
        requests: i64,
        usage_logs: i64,
        recorded_cutoffs: Mutex<Vec<(String, DateTime<Utc>)>>,
    }

    impl FakePreviewCounter {
        fn new(requests: i64, usage_logs: i64) -> Self {
            Self {
                requests,
                usage_logs,
                recorded_cutoffs: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl GcPreviewCounter for FakePreviewCounter {
        async fn count_requests_older_than(&self, cutoff_at: DateTime<Utc>) -> Result<i64, String> {
            self.recorded_cutoffs
                .lock()
                .await
                .push(("requests".to_string(), cutoff_at));
            Ok(self.requests)
        }

        async fn count_usage_logs_older_than(
            &self,
            cutoff_at: DateTime<Utc>,
        ) -> Result<i64, String> {
            self.recorded_cutoffs
                .lock()
                .await
                .push(("usage_logs".to_string(), cutoff_at));
            Ok(self.usage_logs)
        }
    }

    /// Parity: Go `PreviewCleanup` (`gc.go` lines 638-664) — both resources
    /// with days > 0 are emitted in requests-then-usage_logs order and each
    /// has its count populated via the COUNT query seam.
    #[tokio::test]
    async fn gc_preview_with_counts_fills_both_resources_in_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let input = TriggerGcCleanupInput {
            requests_cleanup_days: 7,
            usage_logs_cleanup_days: 30,
        };
        let counter = FakePreviewCounter::new(42, 99);
        let items = preview_with_counts(&input, now, &counter).await?;

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].resource_type, "requests");
        assert_eq!(items[0].estimated_count, 42);
        assert_eq!(items[0].retention_days, 7);
        assert_eq!(items[1].resource_type, "usage_logs");
        assert_eq!(items[1].estimated_count, 99);
        assert_eq!(items[1].retention_days, 30);

        let recorded = counter.recorded_cutoffs.lock().await.clone();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].0, "requests");
        assert_eq!(recorded[0].1, now - Duration::days(7));
        assert_eq!(recorded[1].0, "usage_logs");
        assert_eq!(recorded[1].1, now - Duration::days(30));
        Ok(())
    }

    /// Parity: Go `PreviewCleanup` skips resources with days <= 0
    /// (`gc.go` lines 638, 652) — only the named resource is counted.
    #[tokio::test]
    async fn gc_preview_with_counts_skips_zero_days() -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let input = TriggerGcCleanupInput {
            requests_cleanup_days: 0,
            usage_logs_cleanup_days: 14,
        };
        let counter = FakePreviewCounter::new(0, 5);
        let items = preview_with_counts(&input, now, &counter).await?;

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].resource_type, "usage_logs");
        assert_eq!(items[0].estimated_count, 5);

        let recorded = counter.recorded_cutoffs.lock().await.clone();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "usage_logs");
        Ok(())
    }

    /// Parity: Go `PreviewCleanup` empty input → empty items
    /// (`gc.go` line 636 init + no appends).
    #[tokio::test]
    async fn gc_preview_with_counts_empty_input_no_queries()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        let counter = FakePreviewCounter::new(10, 20);
        let items = preview_with_counts(&TriggerGcCleanupInput::default(), now, &counter).await?;
        assert!(items.is_empty());
        assert!(counter.recorded_cutoffs.lock().await.is_empty());
        Ok(())
    }

    /// Parity: Go `PreviewCleanup` propagates COUNT errors (`gc.go` lines
    /// 642, 656 — `return nil, fmt.Errorf(...)`).
    #[tokio::test]
    async fn gc_preview_with_counts_propagates_counter_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let now = fixed_now()?;
        struct ErroringCounter;
        #[async_trait]
        impl GcPreviewCounter for ErroringCounter {
            async fn count_requests_older_than(
                &self,
                _cutoff_at: DateTime<Utc>,
            ) -> Result<i64, String> {
                Err("db connection lost".to_string())
            }
            async fn count_usage_logs_older_than(
                &self,
                _cutoff_at: DateTime<Utc>,
            ) -> Result<i64, String> {
                Ok(0)
            }
        }
        let input = TriggerGcCleanupInput {
            requests_cleanup_days: 7,
            usage_logs_cleanup_days: 0,
        };
        let result = preview_with_counts(&input, now, &ErroringCounter).await;
        assert_eq!(result, Err("db connection lost".to_string()));
        Ok(())
    }

    // ====================================================================
    // S09 — vacuum decision + executor (Hilbert-the-11th).
    // PostgreSQL regular/full modes and the disabled short-circuit.
    // ====================================================================

    /// Parity: Go `TestWorker_runVacuum_Disabled` (`vacuum_test.go` lines
    /// 19-24) — `VacuumEnabled=false` short-circuits before any driver
    /// interaction.
    #[test]
    fn gc_decide_vacuum_disabled_returns_disabled_outcome() {
        let config = GcConfig {
            cron: "0 2 * * *".to_string(),
            vacuum_enabled: false,
            vacuum_full: false,
        };
        let decision = decide_vacuum(&config);
        assert!(decision.is_skip());
        assert_eq!(decision.outcome, VacuumOutcome::Disabled);
        assert_eq!(decision.sql(), None);
    }

    /// Parity: Go `TestWorker_runVacuum_Postgres` subtests (`vacuum_test.go`
    /// lines 56-69): `vacuum` and `vacuum_full` produce distinct SQL on
    /// Postgres.
    #[test]
    fn gc_decide_vacuum_postgres_full_flag_selects_sql() {
        let mut config = GcConfig {
            cron: "0 2 * * *".to_string(),
            vacuum_enabled: true,
            vacuum_full: false,
        };
        assert_eq!(decide_vacuum(&config).sql(), Some("VACUUM"));
        config.vacuum_full = true;
        assert_eq!(decide_vacuum(&config).sql(), Some("VACUUM FULL"));
    }

    /// Parity: Go `runVacuum` skips return nil without touching the driver
    /// (`gc.go` lines 567, 578, 585) — the executor must NOT be called on
    /// skip outcomes.
    #[tokio::test]
    async fn gc_run_vacuum_skip_outcomes_do_not_invoke_executor()
    -> Result<(), Box<dyn std::error::Error>> {
        struct RecordingExecutor {
            calls: Mutex<Vec<&'static str>>,
        }
        #[async_trait]
        impl VacuumExecutor for RecordingExecutor {
            async fn exec(&self, sql: &'static str) -> Result<(), String> {
                self.calls.lock().await.push(sql);
                Ok(())
            }
        }
        let disabled_config = GcConfig {
            cron: "0 2 * * *".to_string(),
            vacuum_enabled: false,
            vacuum_full: false,
        };
        let executor = RecordingExecutor {
            calls: Mutex::new(Vec::new()),
        };
        let outcome = run_vacuum(&disabled_config, &executor).await?;
        assert_eq!(outcome, VacuumOutcome::Disabled);
        assert!(executor.calls.lock().await.is_empty());
        Ok(())
    }

    /// Parity: Go `runVacuum` executes the chosen SQL via
    /// `sqlDriver.ExecContext` (`gc.go` line 601) and returns nil on
    /// success.
    #[tokio::test]
    async fn gc_run_vacuum_executes_chosen_sql_on_success() -> Result<(), Box<dyn std::error::Error>>
    {
        struct RecordingExecutor {
            calls: Mutex<Vec<&'static str>>,
        }
        #[async_trait]
        impl VacuumExecutor for RecordingExecutor {
            async fn exec(&self, sql: &'static str) -> Result<(), String> {
                self.calls.lock().await.push(sql);
                Ok(())
            }
        }
        let executor = RecordingExecutor {
            calls: Mutex::new(Vec::new()),
        };
        let config = GcConfig {
            cron: "0 2 * * *".to_string(),
            vacuum_enabled: true,
            vacuum_full: true,
        };
        let outcome = run_vacuum(&config, &executor).await?;
        assert_eq!(outcome, VacuumOutcome::Executed { sql: "VACUUM FULL" });
        assert_eq!(*executor.calls.lock().await, vec!["VACUUM FULL"]);
        Ok(())
    }

    /// Parity: Go wraps `ExecContext` errors with `failed to execute %s`
    /// (`gc.go` line 602). The decision layer propagates executor errors
    /// verbatim — the wired adapter is responsible for the Go-format wrap.
    #[tokio::test]
    async fn gc_run_vacuum_propagates_executor_error() -> Result<(), Box<dyn std::error::Error>> {
        struct ErroringExecutor;
        #[async_trait]
        impl VacuumExecutor for ErroringExecutor {
            async fn exec(&self, _sql: &'static str) -> Result<(), String> {
                Err("syntax error".to_string())
            }
        }
        let config = GcConfig {
            cron: "0 2 * * *".to_string(),
            vacuum_enabled: true,
            vacuum_full: false,
        };
        let result = run_vacuum(&config, &ErroringExecutor).await;
        assert_eq!(result, Err("syntax error".to_string()));
        Ok(())
    }

    // ====================================================================
    // S10 — request cascade (executions → requests) decision layer
    // (Hilbert-the-11th). Mirrors Go `cleanupRequests` (`gc.go` lines
    // 209-237), `cleanupOldRequestExecutions` (lines 239-286),
    // `cleanupOldRequestsRecords` (lines 288-328).
    // ====================================================================

    /// Parity: Go `cleanupRequests` (`gc.go` lines 217-234) — the cascade
    /// order is executions FIRST, then requests (FK ordering).
    #[test]
    fn gc_request_cascade_phases_match_go_order() {
        assert_eq!(
            REQUEST_CASCADE_PHASES,
            [
                RequestCascadePhase::Executions,
                RequestCascadePhase::Requests,
            ]
        );
    }

    struct FakeCascadeRepo {
        executions_batches: Mutex<std::collections::VecDeque<Vec<GcRequestExecutionRow>>>,
        requests_batches: Mutex<std::collections::VecDeque<Vec<GcRequestRow>>>,
        deleted_executions: Mutex<Vec<Vec<i64>>>,
        deleted_requests: Mutex<Vec<Vec<i64>>>,
    }

    impl FakeCascadeRepo {
        fn new(
            executions_batches: Vec<Vec<GcRequestExecutionRow>>,
            requests_batches: Vec<Vec<GcRequestRow>>,
        ) -> Self {
            Self {
                executions_batches: Mutex::new(executions_batches.into()),
                requests_batches: Mutex::new(requests_batches.into()),
                deleted_executions: Mutex::new(Vec::new()),
                deleted_requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl GcCascadeRepo for FakeCascadeRepo {
        async fn list_old_executions(
            &self,
            _ctx: &RequestContext,
            _cutoff_at: DateTime<Utc>,
            _batch_size: u32,
        ) -> GcServiceResult<Vec<GcRequestExecutionRow>> {
            Ok(self
                .executions_batches
                .lock()
                .await
                .pop_front()
                .unwrap_or_default())
        }

        async fn delete_executions_by_ids(
            &self,
            _ctx: &RequestContext,
            ids: &[i64],
        ) -> GcServiceResult<u64> {
            self.deleted_executions.lock().await.push(ids.to_vec());
            Ok(ids.len() as u64)
        }

        async fn list_old_requests(
            &self,
            _ctx: &RequestContext,
            _cutoff_at: DateTime<Utc>,
            _batch_size: u32,
        ) -> GcServiceResult<Vec<GcRequestRow>> {
            Ok(self
                .requests_batches
                .lock()
                .await
                .pop_front()
                .unwrap_or_default())
        }

        async fn delete_requests_by_ids(
            &self,
            _ctx: &RequestContext,
            ids: &[i64],
        ) -> GcServiceResult<u64> {
            self.deleted_requests.lock().await.push(ids.to_vec());
            Ok(ids.len() as u64)
        }
    }

    /// Parity: Go `cleanupRequests` (`gc.go` lines 209-237) — one batch of
    /// executions, then one batch of requests. Each phase's rows are
    /// deleted AFTER per-row external-storage cleanup.
    #[tokio::test]
    async fn gc_run_request_cascade_deletes_executions_then_requests()
    -> Result<(), Box<dyn std::error::Error>> {
        let cutoff = fixed_now()?;
        let repo = FakeCascadeRepo::new(
            vec![vec![GcRequestExecutionRow {
                id: 505,
                project_id: 404,
                request_id: 303,
                data_storage_id: 0, // no external storage → no lookup
            }]],
            vec![vec![GcRequestRow {
                id: 303,
                project_id: 404,
                data_storage_id: 0,
            }]],
        );
        let storage = FakeExternalStorage::new(vec![], vec![]);
        let report = run_request_cascade(&ctx(), &repo, &storage, cutoff).await?;

        assert_eq!(report.executions.deleted_rows, 1);
        assert_eq!(report.requests.deleted_rows, 1);
        assert_eq!(
            repo.deleted_executions.lock().await.clone(),
            vec![vec![505]]
        );
        assert_eq!(repo.deleted_requests.lock().await.clone(), vec![vec![303]]);
        // No external storage on either row → no lookups, no deletes.
        assert!(storage.lookups.lock().await.is_empty());
        assert!(storage.deletes.lock().await.is_empty());
        Ok(())
    }

    /// Parity: Go per-batch loop end condition (`gc.go` lines 260-262,
    /// 308-310 — `if len == 0 { break }`) — paging continues until an
    /// empty batch is returned.
    #[tokio::test]
    async fn gc_run_request_cascade_pages_until_empty_batch()
    -> Result<(), Box<dyn std::error::Error>> {
        let cutoff = fixed_now()?;
        let repo = FakeCascadeRepo::new(
            vec![
                vec![
                    GcRequestExecutionRow {
                        id: 1,
                        project_id: 1,
                        request_id: 1,
                        data_storage_id: 0,
                    },
                    GcRequestExecutionRow {
                        id: 2,
                        project_id: 1,
                        request_id: 1,
                        data_storage_id: 0,
                    },
                ],
                vec![GcRequestExecutionRow {
                    id: 3,
                    project_id: 1,
                    request_id: 1,
                    data_storage_id: 0,
                }],
                // Empty third batch ends the executions phase.
            ],
            vec![
                vec![GcRequestRow {
                    id: 1,
                    project_id: 1,
                    data_storage_id: 0,
                }],
                // Empty second batch ends the requests phase.
            ],
        );
        let storage = FakeExternalStorage::new(vec![], vec![]);
        let report = run_request_cascade(&ctx(), &repo, &storage, cutoff).await?;

        assert_eq!(report.executions.deleted_rows, 3);
        assert_eq!(report.requests.deleted_rows, 1);
        assert_eq!(
            repo.deleted_executions.lock().await.clone(),
            vec![vec![1, 2], vec![3]]
        );
        assert_eq!(repo.deleted_requests.lock().await.clone(), vec![vec![1]]);
        Ok(())
    }

    /// Parity: Go per-row external-storage cleanup runs BEFORE the batch
    /// DELETE (`gc.go` lines 266-274 / 313-320). The cascade must invoke
    /// storage cleanup for each row, and the cache must be shared across
    /// phases.
    #[tokio::test]
    async fn gc_run_request_cascade_invokes_external_cleanup_per_row_with_shared_cache()
    -> Result<(), Box<dyn std::error::Error>> {
        let cutoff = fixed_now()?;
        let exec_keys = execution_external_cleanup_keys(404, 303, 505);
        let req_keys = request_external_cleanup_keys(404, 303);
        let mut all_keys = exec_keys.clone();
        all_keys.extend(req_keys.clone());
        // Same DataStorage id=9 for both rows → cache means ONE lookup.
        let storage = FakeExternalStorage::new(vec![DataStorageRef::external(9)], all_keys);
        let repo = FakeCascadeRepo::new(
            vec![vec![GcRequestExecutionRow {
                id: 505,
                project_id: 404,
                request_id: 303,
                data_storage_id: 9,
            }]],
            vec![vec![GcRequestRow {
                id: 303,
                project_id: 404,
                data_storage_id: 9,
            }]],
        );
        let report = run_request_cascade(&ctx(), &repo, &storage, cutoff).await?;

        assert_eq!(report.executions.deleted_rows, 1);
        assert_eq!(report.requests.deleted_rows, 1);
        assert_eq!(report.executions.storage_reports.len(), 1);
        assert_eq!(report.requests.storage_reports.len(), 1);
        // One shared lookup for both rows.
        assert_eq!(*storage.lookups.lock().await, vec![9]);
        // All keys removed.
        assert!(storage.remaining_objects().await.is_empty());
        Ok(())
    }

    /// Parity: Go's empty cascade (no stale rows) is a clean no-op
    /// (`gc.go` lines 260-262 / 308-310 — first batch empty → break
    /// immediately).
    #[tokio::test]
    async fn gc_run_request_cascade_empty_is_noop() -> Result<(), Box<dyn std::error::Error>> {
        let cutoff = fixed_now()?;
        let repo = FakeCascadeRepo::new(vec![], vec![]);
        let storage = FakeExternalStorage::new(vec![], vec![]);
        let report = run_request_cascade(&ctx(), &repo, &storage, cutoff).await?;

        assert_eq!(report, CascadeReport::default());
        assert!(repo.deleted_executions.lock().await.is_empty());
        assert!(repo.deleted_requests.lock().await.is_empty());
        Ok(())
    }
}
