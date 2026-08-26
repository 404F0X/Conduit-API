use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    sync::Arc,
};

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use conduit_db::RequestContext;
use conduit_storage::{StorageAdapter, StorageError, StorageMetadata, StorageObject};
use rand::{Rng, distributions::Alphanumeric};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

pub type BackupServiceResult<T> = Result<T, BackupServiceError>;

#[derive(Debug, Error)]
pub enum BackupServiceError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("backup not found: {0}")]
    BackupNotFound(String),
    #[error("backup status conflict for {backup_id}: expected {expected:?}, actual {actual:?}")]
    StatusConflict {
        backup_id: String,
        expected: BackupStatus,
        actual: BackupStatus,
    },
    #[error("invalid backup status transition: {from:?} -> {to:?}")]
    InvalidStatusTransition {
        from: BackupStatus,
        to: BackupStatus,
    },
    #[error("backup {backup_id} cannot be restored from status {status:?}")]
    InvalidRestoreStatus {
        backup_id: String,
        status: BackupStatus,
    },
    #[error("backup version mismatch: expected {expected}, got {got}")]
    BackupVersionMismatch { expected: String, got: String },
    #[error("backup persistence lock poisoned")]
    LockPoisoned,
    /// No [`BackupDataSource`] wired, so a real dump cannot be produced.
    /// Surfaced instead of writing an empty/placeholder archive.
    #[error("backup data source is not wired")]
    DataSourceUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl BackupStatus {
    pub fn can_transition_to(self, next: Self) -> bool {
        match (self, next) {
            (current, next) if current == next => true,
            (Self::Pending, Self::Running | Self::Completed | Self::Failed) => true,
            (Self::Running, Self::Completed | Self::Failed) => true,
            (Self::Completed | Self::Failed, _) => false,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupJob {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub status: BackupStatus,
    pub storage_key: String,
    pub artifact_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub failure_message: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl BackupJob {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        project_id: impl Into<String>,
        storage_key: impl Into<String>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            project_id: project_id.into(),
            status: BackupStatus::Pending,
            storage_key: storage_key.into(),
            artifact_key: None,
            created_at,
            completed_at: None,
            failure_message: None,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupRestoreStatus {
    Pending,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupRestoreRequest {
    pub id: String,
    pub backup_id: String,
    pub source_project_id: String,
    pub target_project_id: String,
    pub status: BackupRestoreStatus,
    pub requested_at: DateTime<Utc>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl BackupRestoreRequest {
    pub fn new(
        id: impl Into<String>,
        backup_id: impl Into<String>,
        source_project_id: impl Into<String>,
        target_project_id: impl Into<String>,
        requested_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            backup_id: backup_id.into(),
            source_project_id: source_project_id.into(),
            target_project_id: target_project_id.into(),
            status: BackupRestoreStatus::Pending,
            requested_at,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreDryRunReport {
    pub valid: bool,
    pub checked_items: usize,
    pub errors: Vec<RestoreValidationError>,
}

impl RestoreDryRunReport {
    fn from_errors(checked_items: usize, errors: Vec<RestoreValidationError>) -> Self {
        Self {
            valid: errors.is_empty(),
            checked_items,
            errors,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RestoreValidationError {
    DuplicateKey {
        key: String,
        first_index: usize,
        duplicate_index: usize,
    },
    DuplicateName {
        name: String,
        first_index: usize,
        duplicate_index: usize,
    },
    UnknownEnumValue {
        field: String,
        value: String,
        index: Option<usize>,
    },
}

// =========================================================================
// S04 / S05 — backup options resolution
//
// Mirrors Go `conduit/internal/server/backup/types.go` (`BackupOptions`,
// lines 199-207) and `conduit/internal/server/biz/system_default.go`
// (`defaultAutoBackupSettings`, lines 57-67) where the rule
// "IncludeAPIKeys defaults to false" is encoded.
// =========================================================================

/// Effective entity set for a single backup run.
///
/// Parity: Go `backup.BackupOptions` (`types.go` lines 199-207). Field order
/// matches the Go struct; serde tags are not present on the Go side (this is a
/// pure in-memory struct, never JSON-marshalled by Go).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupOptions {
    pub include_projects: bool,
    pub include_channels: bool,
    pub include_models: bool,
    pub include_api_keys: bool,
    pub include_model_prices: bool,
    pub include_usage_stats: bool,
    pub include_request_logs: bool,
}

/// Requested entity set; `None` means "use the system default for this field".
///
/// This mirrors the optional shape of Go's on-the-wire backup request where
/// callers may override individual defaults. Tri-state parity is achieved by
/// representing Go's `*bool` (nullable override) as `Option<bool>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_projects: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_channels: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_models: Option<bool>,
    /// S05 — api-keys default NOT included. `None` defers to `defaults`;
    /// `Some(true)` is the only way to turn api-key backup on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_api_keys: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_model_prices: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_usage_stats: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_request_logs: Option<bool>,
}

/// Resolve a `BackupRequest` against system defaults to produce the effective
/// `BackupOptions` for a run.
///
/// Parity rule (S04 + S05): each entity is taken from the request when
/// `Some(...)`, otherwise from `defaults`. The "api keys default false unless
/// include_api_keys true" rule falls out naturally: when
/// `request.include_api_keys` is `None` the default is used (and the canonical
/// default is `false`); the only way to enable api-key backup is to set
/// `request.include_api_keys = Some(true)` or carry an explicitly-true default.
pub fn resolve_backup_options(request: &BackupRequest, defaults: BackupOptions) -> BackupOptions {
    BackupOptions {
        include_projects: request
            .include_projects
            .unwrap_or(defaults.include_projects),
        include_channels: request
            .include_channels
            .unwrap_or(defaults.include_channels),
        include_models: request.include_models.unwrap_or(defaults.include_models),
        include_api_keys: request
            .include_api_keys
            .unwrap_or(defaults.include_api_keys),
        include_model_prices: request
            .include_model_prices
            .unwrap_or(defaults.include_model_prices),
        include_usage_stats: request
            .include_usage_stats
            .unwrap_or(defaults.include_usage_stats),
        include_request_logs: request
            .include_request_logs
            .unwrap_or(defaults.include_request_logs),
    }
}

// =========================================================================
// Restore options + per-entity conflict strategy (Kant-the-2nd)
//
// Mirrors Go `conduit/internal/server/backup/types.go`:
//   * `ConflictStrategy` string enum + its three constants (lines 209-215).
//   * `RestoreOptions` struct (lines 217-230).
// And mirrors Go `conduit/internal/server/backup/restore.go`:
//   * The include-gated entity walk in `restore()` (lines 77-128).
//   * The per-entity `switch opts.XxxConflictStrategy` decision
//     (lines 371-388 for projects, 486-509 for model prices, 592-626 for
//     channels, 681-710 for models, 763-785 for api keys).
// Expressed here as pure decision helpers so the (future) wired restore path
// has a single contract for "what to do with this row" without re-deriving
// the switch ladder at each call site.
// =========================================================================

/// Per-entity conflict-resolution strategy applied during restore when an
/// entity with the same natural key already exists in the target.
///
/// Parity: Go `backup.ConflictStrategy` (`types.go` lines 209-215). Wire
/// format is the lowercase string tag (`"skip" | "overwrite" | "error"`).
/// Unknown values are treated as `Skip` (matches Go's `switch` falling through
/// the `default` arm — none of the restore switches has a `default`, so an
/// unrecognised value silently does nothing for the conflicting row).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConflictStrategy {
    /// `ConflictStrategySkip` — keep the existing row, ignore the backup row.
    /// Go: `log.Info("skipping existing ...")` then `continue`
    /// (`restore.go` lines 372-374, 487-488, 593-594, 682-683, 764-765).
    #[serde(rename = "skip")]
    #[default]
    Skip,
    /// `ConflictStrategyOverwrite` — replace the existing row's fields with
    /// the backup row's values. Go: `db.X.UpdateOneID(...).Set...().Save()`
    /// (`restore.go` lines 378-387, 491-509, 601-626, 690-710, 772-785).
    #[serde(rename = "overwrite")]
    Overwrite,
    /// `ConflictStrategyError` — abort the whole restore with a structured
    /// error mentioning the conflicting natural key. Go: `return fmt.Errorf(
    /// "<entity> %s already exists", ...)` (`restore.go` lines 375-377,
    /// 489-490, 595-600, 684-689, 766-771).
    #[serde(rename = "error")]
    Error,
}

/// Effective restore configuration: which entity classes to consider and how
/// to resolve conflicts against existing rows.
///
/// Parity: Go `backup.RestoreOptions` (`types.go` lines 217-230). Field order
/// matches the Go struct; serde tags are absent on the Go side (pure in-memory
/// struct, never JSON-marshalled). We keep Rust serde-default to allow future
/// wire transport without churn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOptions {
    pub include_projects: bool,
    pub include_channels: bool,
    pub include_models: bool,
    pub include_api_keys: bool,
    pub include_model_prices: bool,
    pub include_usage_stats: bool,
    pub include_request_logs: bool,
    pub project_conflict_strategy: ConflictStrategy,
    pub channel_conflict_strategy: ConflictStrategy,
    pub model_conflict_strategy: ConflictStrategy,
    pub model_price_conflict_strategy: ConflictStrategy,
    pub api_key_conflict_strategy: ConflictStrategy,
}

/// Entity classes the Go restore walk knows how to apply. Used as the
/// discriminant for [`decide_restore_action`] and [`plan_restore`].
///
/// Parity: Go `restore()` dispatch (`restore.go` lines 77-128) iterates the
/// fixed sequence channels → model-prices → models → projects → api-keys →
/// usage-data. The `UsageData` variant stands in for the combined usage-logs +
/// usage-requests branch (Go gates both under
/// `opts.IncludeUsageStats || opts.IncludeRequestLogs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RestoreEntity {
    Channels,
    ModelPrices,
    Models,
    Projects,
    ApiKeys,
    UsageData,
}

impl RestoreEntity {
    /// All entities in the order Go's `restore()` visits them
    /// (`restore.go` lines 78-125).
    pub const fn walk_order() -> &'static [RestoreEntity] {
        &[
            RestoreEntity::Channels,
            RestoreEntity::ModelPrices,
            RestoreEntity::Models,
            RestoreEntity::Projects,
            RestoreEntity::ApiKeys,
            RestoreEntity::UsageData,
        ]
    }
}

/// Concrete action a restore driver should take for one entity row.
///
/// This is the pure projection of Go's `switch opts.XxxConflictStrategy`
/// ladder. The driver only needs to know *whether* to insert/overwrite/skip/
/// abort; the *how* (db.UpdateOneID(...).Set...().Save()) stays in the wired
/// layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreAction {
    /// No existing row — Go's `db.X.Create()` branch
    /// (`restore.go` lines 393-401, 533-539, 644-654, 723-734, 805-824).
    Create,
    /// Existing row + `ConflictStrategySkip` — Go's `continue` after the
    /// `log.Info("skipping existing ...")` (`restore.go` lines 372-374).
    Skip,
    /// Existing row + `ConflictStrategyOverwrite` — Go's
    /// `db.X.UpdateOneID(existing.ID).Set...().Save()` branch
    /// (`restore.go` lines 378-387).
    Overwrite,
    /// Existing row + `ConflictStrategyError` — Go's
    /// `return fmt.Errorf("<entity> %s already exists", ...)`. The caller
    /// surfaces this as a terminal restore error (`restore.go` lines 375-377).
    Error,
}

/// Decide what to do with a restore candidate row given whether it already
/// exists and the configured per-entity conflict strategy.
///
/// Parity: Go `switch opts.XxxConflictStrategy` ladders
/// (`restore.go` lines 371-388 / 486-509 / 592-626 / 681-710 / 763-785).
/// When `exists` is `false` the answer is always `Create`, regardless of
/// strategy (Go unconditionally executes the `db.X.Create()` arm). When
/// `exists` is `true` the strategy selects between Skip / Overwrite / Error.
pub fn decide_restore_action(exists: bool, strategy: ConflictStrategy) -> RestoreAction {
    if !exists {
        return RestoreAction::Create;
    }
    match strategy {
        ConflictStrategy::Skip => RestoreAction::Skip,
        ConflictStrategy::Overwrite => RestoreAction::Overwrite,
        ConflictStrategy::Error => RestoreAction::Error,
    }
}

/// The strategy configured for a given entity class.
///
/// `UsageData` returns `None` because Go's usage-restore path has no
/// per-entity `ConflictStrategy` (it silently drops duplicates — see
/// `restore.go` line 1204 `"usage log already exists for request, skipping"`).
pub fn strategy_for_entity(
    entity: RestoreEntity,
    opts: RestoreOptions,
) -> Option<ConflictStrategy> {
    match entity {
        RestoreEntity::Projects => Some(opts.project_conflict_strategy),
        RestoreEntity::Channels => Some(opts.channel_conflict_strategy),
        RestoreEntity::Models => Some(opts.model_conflict_strategy),
        RestoreEntity::ModelPrices => Some(opts.model_price_conflict_strategy),
        RestoreEntity::ApiKeys => Some(opts.api_key_conflict_strategy),
        RestoreEntity::UsageData => None,
    }
}

/// Whether the given entity class should be processed at all under `opts`.
///
/// Parity: Go `restore()` include-gates (`restore.go` lines 78, 89, 95, 101,
/// 115, 121). `UsageData` is included when EITHER usage-stats OR request-logs
/// is requested, matching Go's
/// `if opts.IncludeUsageStats || opts.IncludeRequestLogs` (line 121).
pub fn entity_is_included(entity: RestoreEntity, opts: RestoreOptions) -> bool {
    match entity {
        RestoreEntity::Channels => opts.include_channels,
        RestoreEntity::ModelPrices => opts.include_model_prices,
        RestoreEntity::Models => opts.include_models,
        RestoreEntity::Projects => opts.include_projects,
        RestoreEntity::ApiKeys => opts.include_api_keys,
        RestoreEntity::UsageData => opts.include_usage_stats || opts.include_request_logs,
    }
}

/// Filtered, ordered list of entity classes a restore run will actually touch.
///
/// Parity intent: Go's `restore()` walks the fixed sequence
/// (channels → model-prices → models → projects → api-keys → usage-data) and
/// silently skips any class whose include flag is `false`. This helper returns
/// the surviving sub-sequence so a wired driver can iterate without
/// re-checking each gate.
pub fn plan_restore(opts: RestoreOptions) -> Vec<RestoreEntity> {
    RestoreEntity::walk_order()
        .iter()
        .copied()
        .filter(|entity| entity_is_included(*entity, opts))
        .collect()
}

/// Build the Go-shape error message for a `ConflictStrategy::Error` collision.
///
/// Parity: Go's `<entity> %s already exists` format (`restore.go` lines 377,
/// 490, 600, 689, 771). The `entity_label` is the human-readable class name
/// (`"project"`, `"channel"`, `"model"`, `"channel model price"`, `"API key"`).
/// `natural_key` is the row's name / model_id pair used in the Go `Errorf`.
pub fn conflict_error_message(entity_label: &str, natural_key: &str) -> String {
    format!("{entity_label} {natural_key} already exists")
}

// =========================================================================
// S06 / S07 — auto-backup settings + retention + scheduling
//
// Mirrors Go:
//   - `biz.BackupFrequency` constants (`system.go` lines 225-231)
//   - `biz.AutoBackupSettings` struct (`system.go` lines 234-254)
//   - `defaultAutoBackupSettings` (`system_default.go` lines 57-67)
//   - `BackupService.shouldRunBackup` (`autobackup.go` lines 74-85)
// =========================================================================

/// How often automatic backups are created.
///
/// Parity: Go `biz.BackupFrequency` (`system.go` lines 225-231). Wire format
/// is lowercase snake (`"daily" | "weekly" | "monthly"`); unknown values fall
/// back to `Daily` (matching Go's `default: return true` in `shouldRunBackup`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum BackupFrequency {
    #[default]
    Daily,
    Weekly,
    Monthly,
}

/// Automatic backup configuration.
///
/// Parity: Go `biz.AutoBackupSettings` (`system.go` lines 234-254). JSON tags
/// are camelCase on the Go side, but `AutoBackupSettings` is persisted as a
/// JSON blob under `system_auto_backup_settings` and the Go struct uses
/// snake_case tags, so we keep snake_case here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutoBackupSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub frequency: BackupFrequency,
    #[serde(default)]
    pub data_storage_id: i64,
    #[serde(default)]
    pub include_channels: bool,
    #[serde(default)]
    pub include_models: bool,
    /// S05 — api keys default NOT included. Canonical default is `false`
    /// (`system_default.go` line 62).
    #[serde(default)]
    pub include_api_keys: bool,
    #[serde(default)]
    pub include_model_prices: bool,
    #[serde(default)]
    pub include_usage_stats: bool,
    #[serde(default)]
    pub include_request_logs: bool,
    /// Days to keep backups (0 = keep all).
    #[serde(default)]
    pub retention_days: i64,
    /// Timestamp of the last successful backup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_backup_at: Option<DateTime<Utc>>,
    /// Error message from the last backup attempt, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_backup_error: Option<String>,
}

impl Default for AutoBackupSettings {
    fn default() -> Self {
        // Parity: Go `defaultAutoBackupSettings` (`system_default.go` lines
        // 57-67). Field-by-field identical.
        Self {
            enabled: false,
            frequency: BackupFrequency::Daily,
            data_storage_id: 0,
            include_channels: true,
            include_models: true,
            include_api_keys: false,
            include_model_prices: true,
            include_usage_stats: false,
            include_request_logs: false,
            retention_days: 30,
            last_backup_at: None,
            last_backup_error: None,
        }
    }
}

impl AutoBackupSettings {
    /// Collapse settings into a `BackupOptions` for the actual dump step.
    ///
    /// Parity: Go `performBackup` (`autobackup.go` lines 93-100) which copies
    /// the six include-* booleans into a `BackupOptions` before calling
    /// `BackupWithoutAuth`.
    pub fn to_backup_options(&self) -> BackupOptions {
        BackupOptions {
            include_projects: false,
            include_channels: self.include_channels,
            include_models: self.include_models,
            include_api_keys: self.include_api_keys,
            include_model_prices: self.include_model_prices,
            include_usage_stats: self.include_usage_stats,
            include_request_logs: self.include_request_logs,
        }
    }

    /// Whether `now` is a day on which a scheduled backup should run.
    ///
    /// Parity: Go `BackupService.shouldRunBackup` (`autobackup.go` lines
    /// 74-85). Daily always runs; weekly runs on Sunday; monthly runs on the
    /// 1st; unknown frequencies fall back to "always run" (Go `default:
    /// return true`).
    pub fn should_run_on(&self, now: DateTime<Utc>) -> bool {
        match self.frequency {
            BackupFrequency::Daily => true,
            BackupFrequency::Weekly => now.weekday() == Weekday::Sun,
            BackupFrequency::Monthly => now.day() == 1,
        }
    }

    /// Compute the next scheduled backup instant at or after `now`.
    ///
    /// The Go cron spec is `"0 2 * * *"` (`service.go` line 43 +
    /// `autobackup.go` line 26): **02:00 local time every day**. The scheduler
    /// then gates on `shouldRunBackup`. To stay pure and timezone-agnostic we
    /// express the cadence directly in UTC at 02:00 and walk forward day-by-day
    /// until we hit a date whose frequency predicate is satisfied.
    ///
    /// The result is the next eligible scheduled fire instant; a caller that
    /// already has a `last_backup_at` may pass it for an idempotent
    /// "advance past last run" behaviour.
    pub fn next_backup_run(
        &self,
        last: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> DateTime<Utc> {
        // Start from whichever is later: the day after the last run, or today's
        // 02:00 slot. This mirrors a cron scheduler that fires at most once
        // per eligible day.
        let today_slot = match Utc.with_ymd_and_hms(now.year(), now.month(), now.day(), 2, 0, 0) {
            chrono::LocalResult::Single(dt) => dt,
            // Should not happen for valid dates; fall back to `now`.
            _ => now,
        };

        let mut cursor = match last {
            Some(last_run) if last_run >= today_slot => {
                // Last run was today after the slot: begin scanning tomorrow.
                next_day_start(last_run)
            }
            _ => today_slot,
        };

        // Walk forward day by day until the frequency predicate accepts the
        // date. Cap the scan to avoid pathological infinite loops for
        // misconfigured calendars; 366 iterations covers any valid year.
        for _ in 0..366 {
            if self.should_run_on(cursor) && cursor >= now {
                return cursor;
            }
            cursor += Duration::days(1);
        }
        cursor
    }

    /// Compute the retention cutoff: backups older than this instant should be
    /// deleted.
    ///
    /// Parity: Go `BackupService.cleanupOldBackups` (`autobackup.go` line 139)
    /// which computes `cutoff := time.Now().AddDate(0, 0, -retentionDays)`.
    /// `retention_days <= 0` disables retention (returns `None`), matching
    /// Go's `if settings.RetentionDays > 0` guard (`autobackup.go` line 119).
    pub fn retention_cutoff(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if self.retention_days <= 0 {
            return None;
        }
        Some(now - Duration::days(self.retention_days))
    }
}

fn next_day_start(dt: DateTime<Utc>) -> DateTime<Utc> {
    let Some(naive_date) = NaiveDate::from_ymd_opt(dt.year(), dt.month(), dt.day()) else {
        return dt + Duration::days(1);
    };
    // Advance to the 02:00 slot one day later. `and_hms_opt(2,0,0)` is never
    // `None` for a valid `NaiveDate` (UTC has no DST).
    match naive_date
        .succ_opt()
        .and_then(|d| d.and_hms_opt(2, 0, 0))
        .and_then(|n| Utc.from_local_datetime(&n).single())
    {
        Some(next) => next,
        None => dt + Duration::days(1),
    }
}

// =========================================================================
// S06 / S09 — backup-format parse + version compatibility
//
// Mirrors Go `conduit/internal/server/backup/types.go` (`BackupData`,
// lines 13-23) and `conduit/internal/server/backup/restore.go`
// (lines 36-47) where the archive is parsed and its version validated.
//
// IMPORTANT serde-tag gotcha (CLAUDE.md parity rule): unlike most Conduit API
// structs, `BackupData` uses **snake_case** json tags on the Go side
// (`types.go` lines 13-23: `"channel_model_prices"`, `"api_keys"`,
// `"usage_requests"`, `"usage_logs"`). We therefore do NOT use
// `rename_all = "camelCase"` here and instead mirror the snake_case tags
// verbatim.
// =========================================================================

/// Backup-format version constants.
///
/// Parity: Go `backup.BackupVersion*` (`types.go` lines 192-197).
pub const BACKUP_VERSION: &str = "1.4";
pub const BACKUP_VERSION_V1: &str = "1.0";
pub const BACKUP_VERSION_V2: &str = "1.1";
pub const BACKUP_VERSION_V3: &str = "1.2";
pub const BACKUP_VERSION_V4: &str = "1.3";

/// All backup-format versions the current binary accepts on restore.
///
/// Parity: Go `lo.Contains([]string{BackupVersion, BackupVersionV3,
/// BackupVersionV2, BackupVersionV1}, backupData.Version)` at `restore.go`
/// line 41.
pub fn supported_backup_versions() -> &'static [&'static str] {
    &[
        BACKUP_VERSION,
        BACKUP_VERSION_V4,
        BACKUP_VERSION_V3,
        BACKUP_VERSION_V2,
        BACKUP_VERSION_V1,
    ]
}

/// Parsed top-level backup archive manifest.
///
/// Parity: Go `backup.BackupData` (`types.go` lines 13-23). Tags are
/// snake_case to match the Go struct field-for-field; `entities` is the
/// untyped analogue of Go's per-entity slices and is preserved verbatim so
/// callers can stream individual sections without re-parsing the whole blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackupManifest {
    /// Go `BackupData.Version` (`types.go` line 14).
    pub version: String,
    /// Go `BackupData.Timestamp` (`types.go` line 15), tag `"timestamp"`.
    pub timestamp: DateTime<Utc>,
    /// Untyped per-entity blobs keyed by their snake_case Go tag
    /// (`projects`, `channels`, `models`, `channel_model_prices`,
    /// `api_keys`, `usage_requests`, `usage_logs`). Missing sections are
    /// `None`, matching Go's `omitempty`.
    ///
    /// `#[serde(flatten)]` here mirrors the flat Go `BackupData` layout
    /// (`types.go` lines 13-23) where `projects`/`channels`/... live at the
    /// same level as `version` and `timestamp`, not nested under a parent
    /// key. Without `flatten`, serde would look for an `entities` object in
    /// the JSON which Go never emits.
    #[serde(default, flatten)]
    pub entities: BackupEntities,
    /// Free-form metadata block (Go `BackupData` has no metadata field today
    /// but the archive envelope is expected to grow one; preserved verbatim).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Per-entity sections of a backup archive.
///
/// Each field is the raw JSON array for that entity; large sections can be
/// streamed independently without buffering the whole archive. `None` means
/// the section was absent (Go `omitempty`); `Some(Vec::new())` means present
/// but empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupEntities {
    /// Go `"projects,omitempty"` (`types.go` line 16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects: Option<Value>,
    /// Go `"channels"` (`types.go` line 17).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<Value>,
    /// Go `"models"` (`types.go` line 18).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Value>,
    /// Go `"channel_model_prices,omitempty"` (`types.go` line 19).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_model_prices: Option<Value>,
    /// Rust 1.4 extension containing accounting settings, retail prices,
    /// procurement history, provider observations, review drafts and audits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_configuration: Option<Value>,
    /// Go `"api_keys,omitempty"` (`types.go` line 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_keys: Option<Value>,
    /// Go `"usage_requests,omitempty"` (`types.go` line 21).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_requests: Option<Value>,
    /// Go `"usage_logs,omitempty"` (`types.go` line 22).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_logs: Option<Value>,
}

impl BackupEntities {
    /// Count of top-level entries across all present sections.
    ///
    /// Used by `streaming_plan` to size chunks without buffering payloads.
    pub fn total_entries(&self) -> usize {
        [
            &self.projects,
            &self.channels,
            &self.models,
            &self.channel_model_prices,
            &self.pricing_configuration,
            &self.api_keys,
            &self.usage_requests,
            &self.usage_logs,
        ]
        .into_iter()
        .filter_map(|opt| {
            opt.as_ref().map(|value| match value {
                Value::Array(values) => values.len(),
                Value::Object(sections) => sections
                    .values()
                    .filter_map(Value::as_array)
                    .map(std::vec::Vec::len)
                    .sum(),
                _ => 0,
            })
        })
        .sum()
    }
}

/// Parse a backup archive JSON blob into a [`BackupManifest`].
///
/// Parity: Go `json.Unmarshal(data, &backupData)` (`restore.go` line 37).
/// Returns a structured error for malformed JSON (matches Go's "Invalid JSON"
/// golden case at `restore_test.go` lines 500-513).
pub fn parse_backup_manifest(data: &[u8]) -> Result<BackupManifest, serde_json::Error> {
    serde_json::from_slice::<BackupManifest>(data)
}

/// Reject backup archives whose declared version is not in the supported set.
///
/// Parity: Go `restore.go` lines 41-47:
/// ```text
/// if !lo.Contains([]string{BackupVersion, BackupVersionV3,
///     BackupVersionV2, BackupVersionV1}, backupData.Version) {
///     return fmt.Errorf("backup version mismatch: expected %s, got %s",
///         BackupVersion, backupData.Version)
/// }
/// ```
/// The "invalid version" golden case lives at `restore_test.go`
/// lines 515-535.
pub fn validate_backup_version(
    manifest_version: &str,
    supported: &[&str],
) -> Result<(), BackupServiceError> {
    if supported.contains(&manifest_version) {
        Ok(())
    } else {
        Err(BackupServiceError::BackupVersionMismatch {
            expected: supported.first().copied().unwrap_or("").to_string(),
            got: manifest_version.to_string(),
        })
    }
}

// =========================================================================
// S12 / S13 — streaming + sensitive-field masking
//
// The Go runtime never logs plaintext API keys (see `restore.go`:
// `log.String("name", akData.Name)` at line 765/769/781, never `akData.Key`;
// `log.String("channel", chData.Name)` at line 594/597/622, never
// `credentials.APIKey`; the `usageRestoreResolver.apiKeyKeys` map
// (`restore.go` lines 200-205, 225-227) keys on plaintext but only ever
// surfaces the resolved numeric ID in logs). S12/S13 codify that rule with a
// pure redactor and a streaming-chunk plan.
//
// Large backups are restored in fixed-size batches: Go uses
// `usageBackupBatchSize = 500` (`backup_ops.go` line 18) for usage-log bulk
// inserts (`restore.go` lines 1174, 1271) and ID-batched reads
// (`restore.go` lines 996-1028, 1158-1171). `streaming_plan` expresses the
// same batched-walk shape as a pure function over an entity-count budget.
// =========================================================================

/// Batch size used by Go for streaming usage-log reads/writes.
///
/// Parity: Go `usageBackupBatchSize` (`backup_ops.go` line 18).
pub const USAGE_BACKUP_BATCH_SIZE: usize = 500;

/// A log-safe view of a backup operation entry: any field that could carry a
/// plaintext API key, OAuth token, or credential blob is replaced by a fixed
/// mask. The `name`/`kind`/`id` fields are always safe to emit (Go logs them
/// verbatim across `restore.go`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogSafeEntry {
    /// Entity kind (`"channel"`, `"api_key"`, `"usage_request"`, ...).
    pub kind: String,
    /// Human-readable name. Go logs `akData.Name`, `chData.Name`,
    /// `projData.Name` — never the credential field.
    pub name: String,
    /// Stable identifier (numeric or `"<unknown>"`).
    pub id: String,
    /// `true` if the entry carried a redactable secret that was masked.
    pub had_secret: bool,
}

/// Mask any sensitive field on a backup entry before it reaches a log line.
///
/// Parity intent (CLAUDE.md "sensitive-field policy"): Go never logs plaintext
/// API keys. The fields considered sensitive — mirroring the Go struct fields
/// the restore path reads but does NOT log — are:
/// * `key`           (Go `ent.APIKey.Key`, see `restore.go` line 756 lookup)
/// * `api_key_key`   (Go `BackupUsageRequest/Log.APIKeyKey`, lines 88/158)
/// * `credentials`   (Go `BackupChannel.Credentials`, line 580/590)
/// * `oauth`         (Go `ChannelCredentials.OAuth`)
/// * `request_body` / `response_body` / `response_chunks` (may embed keys)
///
/// The function returns a [`LogSafeEntry`] with only the safe `kind`/`name`/
/// `id` fields populated. It never returns the secret itself.
pub fn redact_backup_log_entry(entry: &Value) -> LogSafeEntry {
    let kind = entry
        .get("kind")
        .or_else(|| entry.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let id = entry
        .get("id")
        .map(|v| match v {
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
            _ => "<unknown>".to_string(),
        })
        .unwrap_or_else(|| "<unknown>".to_string());

    let had_secret = is_sensitive_field_present(entry, "key")
        || is_sensitive_field_present(entry, "api_key_key")
        || is_sensitive_field_present(entry, "credentials")
        || is_sensitive_field_present(entry, "oauth")
        || is_sensitive_field_present(entry, "request_body")
        || is_sensitive_field_present(entry, "response_body")
        || is_sensitive_field_present(entry, "response_chunks");

    LogSafeEntry {
        kind,
        name,
        id,
        had_secret,
    }
}

fn is_sensitive_field_present(entry: &Value, field: &str) -> bool {
    match entry.get(field) {
        None => false,
        Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Bool(_)) | Some(Value::Number(_)) => true,
    }
}

/// Streaming-chunk plan for a restore run: how many batches, of what size,
/// over how many total entries.
///
/// Parity intent: Go `restoreUsageLogs` (`restore.go` lines 1174-1278) flushes
/// `db.UsageLog.CreateBulk(builders...)` every `usageBackupBatchSize` rows;
/// `existingUsageRequests` (`restore.go` lines 996-1028) pages ID-batched
/// reads by the same constant. `streaming_plan` reproduces that batching shape
/// as a pure planning step so the caller can pre-allocate buffers / log
/// progress without buffering the whole archive in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamPlan {
    /// Total entries the plan covers.
    pub total_entries: usize,
    /// Per-batch size (Go `usageBackupBatchSize`).
    pub batch_size: usize,
    /// Number of full batches of size `batch_size`.
    pub full_batches: usize,
    /// Size of the trailing partial batch (0 when `total_entries` divides
    /// evenly).
    pub remainder: usize,
    /// Total number of batches (`full_batches + (1 if remainder > 0)`).
    pub batch_count: usize,
}

/// Build a streaming-chunk plan for a given entity count.
///
/// The Go code uses a fixed `usageBackupBatchSize = 500`; callers may override
/// `batch_size` for testing or for entity types with a different constant
/// (none exist today — all usage-log/usage-request paging uses 500).
pub fn streaming_plan(entity_counts: impl IntoIterator<Item = usize>) -> StreamPlan {
    streaming_plan_with_batch(entity_counts, USAGE_BACKUP_BATCH_SIZE)
}

/// Same as [`streaming_plan`] but with an explicit batch size. Useful for
/// tests where the default 500-row batch would hide off-by-one errors.
pub fn streaming_plan_with_batch(
    entity_counts: impl IntoIterator<Item = usize>,
    batch_size: usize,
) -> StreamPlan {
    let total: usize = entity_counts.into_iter().sum();
    let bs = if batch_size == 0 { 1 } else { batch_size };
    let full_batches = total / bs;
    let remainder = total % bs;
    StreamPlan {
        total_entries: total,
        batch_size: bs,
        full_batches,
        remainder,
        batch_count: full_batches + usize::from(remainder > 0),
    }
}

impl StreamPlan {
    /// Iterate the `(start, end)` byte-offset-style ranges of each batch,
    /// mirroring Go's `for start := 0; start < len(ids); start += batchSize`
    /// loop (`restore.go` line 996).
    pub fn batch_ranges(&self) -> impl Iterator<Item = (usize, usize)> {
        let bs = self.batch_size;
        let total = self.total_entries;
        (0..self.batch_count).map(move |i| {
            let start = i * bs;
            let end = std::cmp::min(start + bs, total);
            (start, end)
        })
    }
}

// =========================================================================
// P13-002 — backup section emission + projection rules
//
// Mirrors Go `doBackup` (`conduit/internal/server/backup/backup_ops.go`
// lines 39-167) where each archive section is populated only when its
// `IncludeXxx` flag is set, and `backupUsageLog` / `backupUsageRequest`
// (`backup_ops.go` lines 206-220, 270-284) gate plaintext api-key emission
// on the same `includeAPIKeyValues` flag. Expressed here as pure decision
// helpers so the (future) wired dump path has a single contract for
// "should this section appear" and "what api-key value should land in this
// usage row" without re-deriving the include-ladder and map-gate at each
// call site.
// =========================================================================

/// Backup sections the archive walk knows how to emit.
///
/// Parity: Go `doBackup` (`backup_ops.go` lines 39-167) iterates seven
/// sections (projects → channels → model-prices → models → api-keys →
/// usage-requests → usage-logs), each gated by an `IncludeXxx` flag on
/// `BackupOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BackupSection {
    Projects,
    Channels,
    ModelPrices,
    PricingConfiguration,
    Models,
    ApiKeys,
    UsageRequests,
    UsageLogs,
}

impl BackupSection {
    /// All sections in dependency-friendly archive order. The pricing
    /// configuration entry is the Rust 1.4 extension adjacent to model prices.
    pub const fn emit_order() -> &'static [BackupSection] {
        &[
            BackupSection::Projects,
            BackupSection::Channels,
            BackupSection::ModelPrices,
            BackupSection::PricingConfiguration,
            BackupSection::Models,
            BackupSection::ApiKeys,
            BackupSection::UsageRequests,
            BackupSection::UsageLogs,
        ]
    }
}

/// Whether a section is populated under the given options.
///
/// Parity: Go `doBackup` per-branch include gates (`backup_ops.go`
/// lines 46, 57, 71, 95, 110, 134, 142). Note that `UsageLogs` is gated
/// by `IncludeUsageStats` and `UsageRequests` by `IncludeRequestLogs` —
/// matching Go's branch structure exactly (the two usage branches are
/// independent rather than being OR'd together at the gate).
pub fn section_is_emitted(section: BackupSection, opts: BackupOptions) -> bool {
    match section {
        BackupSection::Projects => opts.include_projects,
        BackupSection::Channels => opts.include_channels,
        BackupSection::ModelPrices => opts.include_model_prices,
        BackupSection::PricingConfiguration => opts.include_model_prices,
        BackupSection::Models => opts.include_models,
        BackupSection::ApiKeys => opts.include_api_keys,
        BackupSection::UsageRequests => opts.include_request_logs,
        BackupSection::UsageLogs => opts.include_usage_stats,
    }
}

/// JSON tag the Go `BackupData` field carries for each section.
///
/// Parity: Go `BackupData` json tags (`types.go` lines 14-22). Note that
/// `channels` and `models` lack `omitempty` and are always emitted even
/// when empty; the remaining sections carry `omitempty` and are dropped
/// when nil. Tests rely on these exact snake_case spellings (see Go
/// `TestBackupService_Backup_*`).
pub fn section_json_tag(section: BackupSection) -> &'static str {
    match section {
        BackupSection::Projects => "projects",
        BackupSection::Channels => "channels",
        BackupSection::ModelPrices => "channel_model_prices",
        BackupSection::PricingConfiguration => "pricing_configuration",
        BackupSection::Models => "models",
        BackupSection::ApiKeys => "api_keys",
        BackupSection::UsageRequests => "usage_requests",
        BackupSection::UsageLogs => "usage_logs",
    }
}

/// Whether a section's slice is dropped from JSON entirely when empty.
///
/// Parity: Go `BackupData` (`types.go` lines 16-22) — only `channels` and
/// `models` lack `omitempty` and are always serialized; everything else
/// is omitted when the slice is nil.
pub fn section_omits_when_empty(section: BackupSection) -> bool {
    !matches!(section, BackupSection::Channels | BackupSection::Models)
}

/// Look up the plaintext api-key value to embed in a usage row, applying
/// the `includeAPIKeyValues` gate.
///
/// Parity: Go `backupUsageLog` (`backup_ops.go` lines 278-280):
/// ```text
/// if ul.APIKeyID != 0 {
///     data.APIKeyKey = apiKeyKeys[ul.APIKeyID]
/// }
/// ```
/// `apiKeyKeys` is the map Go builds at `backup_ops.go` lines 224-238 —
/// only populated when `includeAPIKeyValues` is true. When the flag is
/// false the map is empty so the lookup naturally yields Go's zero string
/// (`""`), which is what gets written into the archive. This helper makes
/// that contract explicit and testable.
pub fn projected_usage_log_api_key_key(
    api_key_id: i64,
    api_key_keys: &BTreeMap<i64, String>,
    include_api_keys: bool,
) -> String {
    if !include_api_keys || api_key_id == 0 {
        return String::new();
    }
    api_key_keys.get(&api_key_id).cloned().unwrap_or_default()
}

/// Look up the plaintext api-key value for a usage-request row.
///
/// Parity: Go `backupUsageRequest` (`backup_ops.go` lines 214-216):
/// ```text
/// if includeAPIKeyValues && req.Edges.APIKey != nil {
///     data.APIKeyKey = req.Edges.APIKey.Key
/// }
/// ```
/// The Go code reads the key directly off the loaded edge rather than a
/// lookup map; we mirror the gating logic (only emit when both
/// `include_api_keys` is true AND an edge is present) so callers can wrap
/// either source uniformly.
pub fn projected_usage_request_api_key_key(
    api_key_edge: Option<&str>,
    include_api_keys: bool,
) -> String {
    match api_key_edge {
        Some(key) if include_api_keys => key.to_string(),
        _ => String::new(),
    }
}

/// Build the id→key map used to enrich usage-log rows.
///
/// Parity: Go `backupUsageLogs` (`backup_ops.go` lines 224-238) — returns
/// an empty map when `includeAPIKeyValues` is false (Go skips the query
/// entirely), or a fully-populated map when true. Mirrors the golden
/// intent of `TestBackupService_Backup_WithUsageStats` (`backup_test.go`
/// lines 322-363): the first invocation
/// (`IncludeAPIKeys=false, IncludeUsageStats=true`) yields an empty
/// `APIKeyKey`, the second (`IncludeAPIKeys=true, IncludeUsageStats=true`)
/// yields `"sk-test-key-1"`.
pub fn build_api_key_keys_map<'a, I>(api_keys: I, include_api_keys: bool) -> BTreeMap<i64, String>
where
    I: IntoIterator<Item = (&'a i64, &'a String)>,
{
    if !include_api_keys {
        return BTreeMap::new();
    }
    api_keys
        .into_iter()
        .map(|(id, key)| (*id, key.clone()))
        .collect()
}

/// Whether the JSON archive should be serialized compactly.
///
/// Parity: Go `doBackup` (`backup_ops.go` lines 162-166):
/// ```text
/// if opts.IncludeUsageStats || opts.IncludeRequestLogs {
///     return json.Marshal(backupData)
/// }
/// return json.MarshalIndent(backupData, "", "  ")
/// ```
/// Large usage dumps are compact to save bytes; everything else is
/// indented for human inspection.
pub fn archive_use_compact_encoding(opts: BackupOptions) -> bool {
    opts.include_usage_stats || opts.include_request_logs
}

// =========================================================================
// S11 — restore dry-run validation
//
// The Bacon-era skeleton (validate_restore_dry_run_manifest) checks
// duplicate key/name and enum status/kind. S11 adds foreign-key validation:
// resources that reference a parent resource (e.g. a model-price referencing
// a channel) must point at a key/name that exists elsewhere in the manifest.
// Mirrors Go `restoreChannelModelPrices`'s "channel not found, skipping"
// branch (`restore.go` lines 457-464) and `usageRestoreResolver` lookups
// (`restore.go` lines 232-267), expressed here as a pure pre-flight check
// over a self-contained manifest.
// =========================================================================

pub fn validate_restore_dry_run_manifest(manifest: &Value) -> RestoreDryRunReport {
    let mut errors = Vec::new();
    validate_status_field(manifest, None, &mut errors);

    let Some(resources) = manifest.get("resources").and_then(Value::as_array) else {
        return RestoreDryRunReport::from_errors(0, errors);
    };

    let mut keys = BTreeMap::<String, usize>::new();
    let mut names = BTreeMap::<String, usize>::new();

    for (index, resource) in resources.iter().enumerate() {
        if let Some(key) = string_field(resource, "key") {
            match keys.entry(key.to_string()) {
                Entry::Vacant(entry) => {
                    entry.insert(index);
                }
                Entry::Occupied(entry) => {
                    errors.push(RestoreValidationError::DuplicateKey {
                        key: key.to_string(),
                        first_index: *entry.get(),
                        duplicate_index: index,
                    });
                }
            }
        }

        if let Some(name) = string_field(resource, "name") {
            match names.entry(name.to_string()) {
                Entry::Vacant(entry) => {
                    entry.insert(index);
                }
                Entry::Occupied(entry) => {
                    errors.push(RestoreValidationError::DuplicateName {
                        name: name.to_string(),
                        first_index: *entry.get(),
                        duplicate_index: index,
                    });
                }
            }
        }

        validate_kind_field(resource, index, &mut errors);
        validate_status_field(resource, Some(index), &mut errors);
        validate_foreign_keys(resource, index, &names, &mut errors);
    }

    RestoreDryRunReport::from_errors(resources.len(), errors)
}

/// Validate that any `parent_name` reference on this resource resolves to a
/// resource declared earlier in the manifest. This is the pure, self-contained
/// analogue of Go's `usageRestoreResolver.resolveChannelID` /
/// `getChannel(name)` lookups (`restore.go` lines 246-258, 421-441).
fn validate_foreign_keys(
    resource: &Value,
    index: usize,
    known_names: &BTreeMap<String, usize>,
    errors: &mut Vec<RestoreValidationError>,
) {
    // Model-price rows reference their parent channel by name.
    // Parity: Go `restoreChannelModelPrices` looks up
    // `db.Channel.Query().Where(channel.Name(pData.ChannelName))`
    // (`restore.go` lines 452-456) and logs "channel not found, skipping"
    // when the lookup fails.
    if let Some(channel_name) = string_field(resource, "channel_name")
        && !channel_name.is_empty()
        && !known_names.contains_key(channel_name)
    {
        errors.push(RestoreValidationError::UnknownEnumValue {
            field: "resources.channel_name".to_string(),
            value: channel_name.to_string(),
            index: Some(index),
        });
    }

    // API-key / usage rows reference their parent project by name.
    // Parity: Go `restoreAPIKeys` resolves `akData.ProjectName` against the
    // projects table (`restore.go` lines 793-807); the dry-run analogue is a
    // manifest-level reference check.
    if let Some(project_name) = string_field(resource, "project_name")
        && !project_name.is_empty()
        && !known_names.contains_key(project_name)
    {
        errors.push(RestoreValidationError::UnknownEnumValue {
            field: "resources.project_name".to_string(),
            value: project_name.to_string(),
            index: Some(index),
        });
    }
}

#[async_trait]
pub trait BackupRepo: Send + Sync {
    async fn create_backup(
        &self,
        ctx: &RequestContext,
        job: BackupJob,
    ) -> BackupServiceResult<BackupJob>;

    async fn get_backup(
        &self,
        ctx: &RequestContext,
        backup_id: &str,
    ) -> BackupServiceResult<Option<BackupJob>>;

    async fn update_backup_status(
        &self,
        ctx: &RequestContext,
        backup_id: &str,
        expected_status: BackupStatus,
        job: BackupJob,
    ) -> BackupServiceResult<Option<BackupJob>>;

    async fn create_restore_request(
        &self,
        ctx: &RequestContext,
        request: BackupRestoreRequest,
    ) -> BackupServiceResult<BackupRestoreRequest>;
}

/// Row source for the backup dump — one JSON array per section.
///
/// Go's `doBackup` (`backup_ops.go:39-167`) queries the ent client directly for
/// each enabled section. The Rust service layer holds no DB handle, so the
/// per-section reads live behind this trait and the host wires a DB-backed impl.
///
/// Each method returns the section's JSON array **already shaped like the Go
/// `Backup*` structs** (that shaping is a row→JSON concern, so it belongs with
/// the repo implementation, not here). Sections the caller did not enable are
/// never requested.
#[async_trait]
pub trait BackupDataSource: Send + Sync {
    /// All rows for `section`, as a JSON array. An empty table yields
    /// `Value::Array(vec![])` — the caller applies Go's omitempty rules.
    async fn load_section(
        &self,
        ctx: &RequestContext,
        section: BackupSection,
    ) -> BackupServiceResult<Value>;
}

/// Assemble the backup archive JSON from per-section arrays.
///
/// Mirrors Go `doBackup`'s tail (`backup_ops.go:148-167`): build `BackupData`
/// with `version` + `timestamp`, then marshal. Section presence follows the Go
/// json tags exactly — `channels` and `models` have no `omitempty` and are
/// emitted even when empty/disabled, every other section is dropped when absent
/// ([`section_omits_when_empty`]).
///
/// Pure: no I/O, so the envelope shape is unit-testable without a database.
pub fn assemble_backup_archive(
    timestamp: DateTime<Utc>,
    sections: &BTreeMap<BackupSection, Value>,
) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("version".to_string(), Value::from(BACKUP_VERSION));
    out.insert(
        "timestamp".to_string(),
        Value::from(timestamp.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
    );

    for section in BackupSection::emit_order() {
        let tag = section_json_tag(*section);
        match sections.get(section) {
            Some(value) => {
                out.insert(tag.to_string(), value.clone());
            }
            // Absent section: Go emits `null` for the two non-omitempty fields
            // (a nil slice marshals to `null`) and drops the rest.
            None if !section_omits_when_empty(*section) => {
                out.insert(tag.to_string(), Value::Null);
            }
            None => {}
        }
    }

    Value::Object(out)
}

/// Serialize the assembled archive the way Go does.
///
/// Parity: Go `doBackup` (`backup_ops.go:163-167`) uses compact `json.Marshal`
/// when either usage section is included (those payloads are large) and
/// `json.MarshalIndent(.., "", "  ")` otherwise.
pub fn serialize_backup_archive(
    archive: &Value,
    opts: BackupOptions,
) -> BackupServiceResult<Vec<u8>> {
    let bytes = if opts.include_usage_stats || opts.include_request_logs {
        serde_json::to_vec(archive)
    } else {
        serde_json::to_vec_pretty(archive)
    };
    bytes.map_err(|error| {
        BackupServiceError::Storage(StorageError::Serialization(error.to_string()))
    })
}

pub struct BackupService {
    repo: Arc<dyn BackupRepo>,
    storage: Arc<dyn StorageAdapter>,
    data_source: Option<Arc<dyn BackupDataSource>>,
}

impl BackupService {
    pub fn new(repo: Arc<dyn BackupRepo>, storage: Arc<dyn StorageAdapter>) -> Self {
        Self {
            repo,
            storage,
            data_source: None,
        }
    }

    /// Attach the row source that makes [`Self::dump`] emit real data.
    ///
    /// Without it the service keeps writing the metadata-only manifest, which is
    /// all it could do before the dump was ported.
    pub fn with_data_source(mut self, data_source: Arc<dyn BackupDataSource>) -> Self {
        self.data_source = Some(data_source);
        self
    }

    /// Produce the backup archive bytes for `opts`.
    ///
    /// Mirrors Go `BackupService.doBackup` (`backup_ops.go:39-167`): query each
    /// enabled section, assemble `BackupData`, marshal (indented unless a usage
    /// section is included). Authorization is the caller's job — Go splits
    /// `Backup` (owner-only, `backup_ops.go:20-33`) from `BackupWithoutAuth`
    /// (used by the auto-backup scheduler), and both funnel here.
    ///
    /// Returns [`BackupServiceError::DataSourceUnavailable`] when no source is
    /// wired, rather than silently writing an empty archive.
    pub async fn dump(
        &self,
        ctx: &RequestContext,
        opts: BackupOptions,
    ) -> BackupServiceResult<Vec<u8>> {
        let Some(source) = self.data_source.as_ref() else {
            return Err(BackupServiceError::DataSourceUnavailable);
        };

        let mut sections = BTreeMap::new();
        for section in BackupSection::emit_order() {
            if !section_is_emitted(*section, opts) {
                continue;
            }
            let rows = source.load_section(ctx, *section).await?;
            sections.insert(*section, rows);
        }

        let archive = assemble_backup_archive(Utc::now(), &sections);
        serialize_backup_archive(&archive, opts)
    }

    pub async fn create_backup_metadata(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        name: &str,
    ) -> BackupServiceResult<BackupJob> {
        let now = Utc::now();
        let id = next_id("backup");
        let storage_key = backup_manifest_key(project_id, &id);
        let job = BackupJob::new(id, name, project_id, storage_key, now);

        self.storage.put(manifest_object(&job)?).await?;

        self.repo.create_backup(ctx, job).await
    }

    /// Create the job row **and** upload the real archive.
    ///
    /// This is the full Go flow: `doBackup` produces the bytes, which are stored
    /// under the job's storage key. `create_backup_metadata` remains for callers
    /// that only need the metadata row.
    pub async fn create_backup_with_dump(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        name: &str,
        opts: BackupOptions,
    ) -> BackupServiceResult<BackupJob> {
        let now = Utc::now();
        let id = next_id("backup");
        let storage_key = backup_manifest_key(project_id, &id);
        let job = BackupJob::new(id, name, project_id, storage_key.clone(), now);

        let bytes = self.dump(ctx, opts).await?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.storage
            .put(
                StorageObject::new(storage_key.clone(), bytes).with_metadata(
                    StorageMetadata::new(storage_key, size).with_content_type("application/json"),
                ),
            )
            .await?;

        self.repo.create_backup(ctx, job).await
    }

    pub async fn mark_completed(
        &self,
        ctx: &RequestContext,
        backup_id: &str,
        artifact_key: Option<String>,
    ) -> BackupServiceResult<BackupJob> {
        self.transition_backup(
            ctx,
            backup_id,
            BackupStatus::Completed,
            Some(Utc::now()),
            None,
            artifact_key,
        )
        .await
    }

    pub async fn mark_failed(
        &self,
        ctx: &RequestContext,
        backup_id: &str,
        failure_message: impl Into<String>,
    ) -> BackupServiceResult<BackupJob> {
        self.transition_backup(
            ctx,
            backup_id,
            BackupStatus::Failed,
            Some(Utc::now()),
            Some(failure_message.into()),
            None,
        )
        .await
    }

    pub async fn request_restore(
        &self,
        ctx: &RequestContext,
        backup_id: &str,
        target_project_id: Option<String>,
    ) -> BackupServiceResult<BackupRestoreRequest> {
        let backup = self
            .repo
            .get_backup(ctx, backup_id)
            .await?
            .ok_or_else(|| BackupServiceError::BackupNotFound(backup_id.to_string()))?;

        if backup.status != BackupStatus::Completed {
            return Err(BackupServiceError::InvalidRestoreStatus {
                backup_id: backup_id.to_string(),
                status: backup.status,
            });
        }

        let request = BackupRestoreRequest::new(
            next_id("restore"),
            backup.id,
            backup.project_id.clone(),
            target_project_id.unwrap_or(backup.project_id),
            Utc::now(),
        );
        self.repo.create_restore_request(ctx, request).await
    }

    async fn transition_backup(
        &self,
        ctx: &RequestContext,
        backup_id: &str,
        next_status: BackupStatus,
        completed_at: Option<DateTime<Utc>>,
        failure_message: Option<String>,
        artifact_key: Option<String>,
    ) -> BackupServiceResult<BackupJob> {
        let current = self
            .repo
            .get_backup(ctx, backup_id)
            .await?
            .ok_or_else(|| BackupServiceError::BackupNotFound(backup_id.to_string()))?;

        if !current.status.can_transition_to(next_status) {
            return Err(BackupServiceError::InvalidStatusTransition {
                from: current.status,
                to: next_status,
            });
        }

        let mut updated = current.clone();
        updated.status = next_status;
        updated.completed_at = completed_at;
        updated.failure_message = failure_message;
        if let Some(artifact_key) = artifact_key {
            updated.artifact_key = Some(artifact_key);
        }

        // Keep status writes compare-and-set shaped so real repos can make the
        // transition atomic while fake repos stay simple.
        match self
            .repo
            .update_backup_status(ctx, backup_id, current.status, updated)
            .await?
        {
            Some(job) => Ok(job),
            None => {
                let actual = self
                    .repo
                    .get_backup(ctx, backup_id)
                    .await?
                    .map(|job| job.status)
                    .unwrap_or(current.status);
                Err(BackupServiceError::StatusConflict {
                    backup_id: backup_id.to_string(),
                    expected: current.status,
                    actual,
                })
            }
        }
    }
}

fn manifest_object(job: &BackupJob) -> BackupServiceResult<StorageObject> {
    let bytes = serde_json::to_vec_pretty(&json!({
        "id": job.id,
        "project_id": job.project_id,
        "status": job.status,
        "created_at": job.created_at,
        "note": "metadata-only manifest; full archive is written by create_backup_with_dump"
    }))
    .map_err(|error| StorageError::Serialization(error.to_string()))?;

    Ok(
        StorageObject::new(job.storage_key.clone(), bytes).with_metadata(
            StorageMetadata::new(job.storage_key.clone(), 0).with_content_type("application/json"),
        ),
    )
}

fn validate_kind_field(resource: &Value, index: usize, errors: &mut Vec<RestoreValidationError>) {
    let Some(kind) = string_field(resource, "kind").or_else(|| string_field(resource, "type"))
    else {
        return;
    };

    if !allowed_restore_kinds().contains(kind) {
        errors.push(RestoreValidationError::UnknownEnumValue {
            field: "resources.kind".to_string(),
            value: kind.to_string(),
            index: Some(index),
        });
    }
}

fn validate_status_field(
    value: &Value,
    index: Option<usize>,
    errors: &mut Vec<RestoreValidationError>,
) {
    let Some(status) = string_field(value, "status") else {
        return;
    };

    if !allowed_restore_statuses().contains(status) {
        errors.push(RestoreValidationError::UnknownEnumValue {
            field: match index {
                Some(_) => "resources.status",
                None => "status",
            }
            .to_string(),
            value: status.to_string(),
            index,
        });
    }
}

fn string_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn allowed_restore_kinds() -> BTreeSet<&'static str> {
    BTreeSet::from(["backup", "restore_request"])
}

fn allowed_restore_statuses() -> BTreeSet<&'static str> {
    BTreeSet::from(["pending", "running", "completed", "failed"])
}

fn backup_manifest_key(project_id: &str, backup_id: &str) -> String {
    format!(
        "backups/{}/{}/manifest.json",
        key_segment(project_id),
        key_segment(backup_id)
    )
}

fn key_segment(value: &str) -> String {
    let segment = value
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || value == '-' || value == '_' {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();

    if segment.is_empty() {
        "unknown".to_string()
    } else {
        segment
    }
}

fn next_id(prefix: &str) -> String {
    let suffix = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect::<String>();
    format!("{prefix}-{suffix}")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::Timelike;
    use conduit_db::{PolicyContext, Principal};
    use conduit_storage::StorageResult;
    use tokio::sync::Mutex;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeBackupRepo {
        backups: Mutex<BTreeMap<String, BackupJob>>,
        restore_requests: Mutex<Vec<BackupRestoreRequest>>,
    }

    impl FakeBackupRepo {
        async fn backup_count(&self) -> usize {
            self.backups.lock().await.len()
        }

        async fn restore_requests(&self) -> Vec<BackupRestoreRequest> {
            self.restore_requests.lock().await.clone()
        }
    }

    #[async_trait]
    impl BackupRepo for FakeBackupRepo {
        async fn create_backup(
            &self,
            _ctx: &RequestContext,
            job: BackupJob,
        ) -> BackupServiceResult<BackupJob> {
            self.backups
                .lock()
                .await
                .insert(job.id.clone(), job.clone());
            Ok(job)
        }

        async fn get_backup(
            &self,
            _ctx: &RequestContext,
            backup_id: &str,
        ) -> BackupServiceResult<Option<BackupJob>> {
            Ok(self.backups.lock().await.get(backup_id).cloned())
        }

        async fn update_backup_status(
            &self,
            _ctx: &RequestContext,
            backup_id: &str,
            expected_status: BackupStatus,
            job: BackupJob,
        ) -> BackupServiceResult<Option<BackupJob>> {
            let mut backups = self.backups.lock().await;
            let Some(current) = backups.get_mut(backup_id) else {
                return Ok(None);
            };
            if current.status != expected_status {
                return Ok(None);
            }

            *current = job.clone();
            Ok(Some(job))
        }

        async fn create_restore_request(
            &self,
            _ctx: &RequestContext,
            request: BackupRestoreRequest,
        ) -> BackupServiceResult<BackupRestoreRequest> {
            self.restore_requests.lock().await.push(request.clone());
            Ok(request)
        }
    }

    #[derive(Debug, Default)]
    struct FakeStorage {
        objects: Mutex<Vec<StorageObject>>,
        fail_put: bool,
    }

    impl FakeStorage {
        fn failing() -> Self {
            Self {
                objects: Mutex::new(Vec::new()),
                fail_put: true,
            }
        }

        async fn objects(&self) -> Vec<StorageObject> {
            self.objects.lock().await.clone()
        }
    }

    #[async_trait]
    impl StorageAdapter for FakeStorage {
        async fn put(&self, object: StorageObject) -> StorageResult<StorageMetadata> {
            if self.fail_put {
                return Err(StorageError::Unavailable("fake storage down".to_string()));
            }

            let metadata = object.metadata.clone();
            self.objects.lock().await.push(object);
            Ok(metadata)
        }

        async fn get(&self, key: &str) -> StorageResult<Option<StorageObject>> {
            Ok(self
                .objects
                .lock()
                .await
                .iter()
                .find(|object| object.metadata.key == key)
                .cloned())
        }

        async fn delete(&self, key: &str) -> StorageResult<bool> {
            let mut objects = self.objects.lock().await;
            let before = objects.len();
            objects.retain(|object| object.metadata.key != key);
            Ok(objects.len() != before)
        }

        async fn head(&self, key: &str) -> StorageResult<Option<StorageMetadata>> {
            Ok(self.get(key).await?.map(|object| object.metadata))
        }

        async fn list(&self, prefix: &str) -> StorageResult<Vec<StorageMetadata>> {
            Ok(self
                .objects
                .lock()
                .await
                .iter()
                .filter(|object| object.metadata.key.starts_with(prefix))
                .map(|object| object.metadata.clone())
                .collect())
        }
    }

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn service(repo: Arc<FakeBackupRepo>, storage: Arc<FakeStorage>) -> BackupService {
        BackupService::new(repo, storage)
    }

    #[tokio::test]
    async fn create_backup_metadata_writes_manifest_and_repo_row() -> BackupServiceResult<()> {
        let repo = Arc::new(FakeBackupRepo::default());
        let storage = Arc::new(FakeStorage::default());
        let service = service(repo.clone(), storage.clone());

        let job = service
            .create_backup_metadata(&ctx(), "project/a", "nightly")
            .await?;

        assert_eq!(job.name, "nightly");
        assert_eq!(job.project_id, "project/a");
        assert_eq!(job.status, BackupStatus::Pending);
        assert!(job.storage_key.starts_with("backups/project_a/backup-"));
        assert_eq!(repo.backup_count().await, 1);

        let objects = storage.objects().await;
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].metadata.key, job.storage_key);
        assert_eq!(
            objects[0].metadata.content_type.as_deref(),
            Some("application/json")
        );
        Ok(())
    }

    #[tokio::test]
    async fn mark_completed_records_terminal_state() -> BackupServiceResult<()> {
        let repo = Arc::new(FakeBackupRepo::default());
        let storage = Arc::new(FakeStorage::default());
        let service = service(repo, storage);
        let job = service
            .create_backup_metadata(&ctx(), "project-a", "nightly")
            .await?;

        let completed = service
            .mark_completed(
                &ctx(),
                &job.id,
                Some("backups/project-a/dump.sql".to_string()),
            )
            .await?;

        assert_eq!(completed.status, BackupStatus::Completed);
        assert_eq!(
            completed.artifact_key,
            Some("backups/project-a/dump.sql".to_string())
        );
        assert!(completed.completed_at.is_some());
        assert_eq!(completed.failure_message, None);
        Ok(())
    }

    #[tokio::test]
    async fn mark_failed_records_failure_message() -> BackupServiceResult<()> {
        let repo = Arc::new(FakeBackupRepo::default());
        let storage = Arc::new(FakeStorage::default());
        let service = service(repo, storage);
        let job = service
            .create_backup_metadata(&ctx(), "project-a", "nightly")
            .await?;

        let failed = service
            .mark_failed(&ctx(), &job.id, "dump command unavailable")
            .await?;

        assert_eq!(failed.status, BackupStatus::Failed);
        assert_eq!(
            failed.failure_message,
            Some("dump command unavailable".to_string())
        );
        assert!(failed.completed_at.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn terminal_status_cannot_be_changed() -> BackupServiceResult<()> {
        let repo = Arc::new(FakeBackupRepo::default());
        let storage = Arc::new(FakeStorage::default());
        let service = service(repo, storage);
        let job = service
            .create_backup_metadata(&ctx(), "project-a", "nightly")
            .await?;
        service.mark_completed(&ctx(), &job.id, None).await?;

        let err = service.mark_failed(&ctx(), &job.id, "late failure").await;

        assert!(matches!(
            err,
            Err(BackupServiceError::InvalidStatusTransition {
                from: BackupStatus::Completed,
                to: BackupStatus::Failed,
            })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn restore_request_is_created_for_completed_backup() -> BackupServiceResult<()> {
        let repo = Arc::new(FakeBackupRepo::default());
        let storage = Arc::new(FakeStorage::default());
        let service = service(repo.clone(), storage);
        let job = service
            .create_backup_metadata(&ctx(), "project-a", "nightly")
            .await?;
        service.mark_completed(&ctx(), &job.id, None).await?;

        let restore = service
            .request_restore(&ctx(), &job.id, Some("project-b".to_string()))
            .await?;

        assert_eq!(restore.backup_id, job.id);
        assert_eq!(restore.source_project_id, "project-a");
        assert_eq!(restore.target_project_id, "project-b");
        assert_eq!(restore.status, BackupRestoreStatus::Pending);
        assert_eq!(repo.restore_requests().await, vec![restore]);
        Ok(())
    }

    #[tokio::test]
    async fn storage_failure_prevents_repo_create() {
        let repo = Arc::new(FakeBackupRepo::default());
        let storage = Arc::new(FakeStorage::failing());
        let service = service(repo.clone(), storage);

        let result = service
            .create_backup_metadata(&ctx(), "project-a", "nightly")
            .await;

        assert!(matches!(
            result,
            Err(BackupServiceError::Storage(StorageError::Unavailable(_)))
        ));
        assert_eq!(repo.backup_count().await, 0);
    }

    #[test]
    fn restore_dry_run_accepts_valid_manifest() {
        let manifest = json!({
            "status": "completed",
            "resources": [
                {
                    "kind": "backup",
                    "key": "backups/project-a/backup-1/manifest.json",
                    "name": "nightly",
                    "status": "completed"
                },
                {
                    "kind": "restore_request",
                    "key": "restores/restore-1",
                    "name": "restore nightly",
                    "status": "pending"
                }
            ]
        });

        let report = validate_restore_dry_run_manifest(&manifest);

        assert!(report.valid);
        assert_eq!(report.checked_items, 2);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn restore_dry_run_reports_duplicate_key_and_name() {
        let manifest = json!({
            "resources": [
                {
                    "kind": "backup",
                    "key": "backups/project-a/backup-1/manifest.json",
                    "name": "nightly",
                    "status": "completed"
                },
                {
                    "kind": "backup",
                    "key": "backups/project-a/backup-1/manifest.json",
                    "name": "nightly",
                    "status": "completed"
                }
            ]
        });

        let report = validate_restore_dry_run_manifest(&manifest);

        assert!(!report.valid);
        assert_eq!(report.checked_items, 2);
        assert_eq!(
            report.errors,
            vec![
                RestoreValidationError::DuplicateKey {
                    key: "backups/project-a/backup-1/manifest.json".to_string(),
                    first_index: 0,
                    duplicate_index: 1,
                },
                RestoreValidationError::DuplicateName {
                    name: "nightly".to_string(),
                    first_index: 0,
                    duplicate_index: 1,
                },
            ]
        );
    }

    #[test]
    fn restore_dry_run_reports_unknown_enum_and_status() {
        let manifest = json!({
            "status": "archived",
            "resources": [
                {
                    "kind": "workspace",
                    "key": "backups/project-a/backup-1/manifest.json",
                    "name": "nightly",
                    "status": "paused"
                }
            ]
        });

        let report = validate_restore_dry_run_manifest(&manifest);

        assert!(!report.valid);
        assert_eq!(report.checked_items, 1);
        assert_eq!(
            report.errors,
            vec![
                RestoreValidationError::UnknownEnumValue {
                    field: "status".to_string(),
                    value: "archived".to_string(),
                    index: None,
                },
                RestoreValidationError::UnknownEnumValue {
                    field: "resources.kind".to_string(),
                    value: "workspace".to_string(),
                    index: Some(0),
                },
                RestoreValidationError::UnknownEnumValue {
                    field: "resources.status".to_string(),
                    value: "paused".to_string(),
                    index: Some(0),
                },
            ]
        );
    }

    // ====================================================================
    // S04 / S05 — BackupOptions resolution
    // ====================================================================

    fn canonical_defaults() -> BackupOptions {
        // Parity: Go `defaultAutoBackupSettings` (channels/models/prices on,
        // api-keys/usage-stats/request-logs off).
        BackupOptions {
            include_projects: false,
            include_channels: true,
            include_models: true,
            include_api_keys: false,
            include_model_prices: true,
            include_usage_stats: false,
            include_request_logs: false,
        }
    }

    #[test]
    fn resolve_backup_options_uses_defaults_when_request_is_blank() {
        let request = BackupRequest::default();
        let resolved = resolve_backup_options(&request, canonical_defaults());

        assert_eq!(resolved, canonical_defaults());
    }

    #[test]
    fn resolve_backup_options_request_overrides_each_default() {
        // Parity: Go's `performBackup` copies the user-supplied flag through
        // when set. We confirm each field is independently overrideable.
        let request = BackupRequest {
            include_projects: Some(true),
            include_channels: Some(false),
            include_models: Some(false),
            include_api_keys: Some(true),
            include_model_prices: Some(false),
            include_usage_stats: Some(true),
            include_request_logs: Some(true),
        };

        let resolved = resolve_backup_options(&request, canonical_defaults());

        assert_eq!(
            resolved,
            BackupOptions {
                include_projects: true,
                include_channels: false,
                include_models: false,
                include_api_keys: true,
                include_model_prices: false,
                include_usage_stats: true,
                include_request_logs: true,
            }
        );
    }

    /// S05 — "api keys default NOT included unless include_api_keys is true".
    /// This is the load-bearing security default: with no request and the
    /// canonical system defaults, api keys are never serialized.
    #[test]
    fn s05_api_keys_default_excluded_unless_explicitly_requested() {
        // Default request + default system settings => api keys OFF.
        let resolved = resolve_backup_options(&BackupRequest::default(), canonical_defaults());
        assert!(!resolved.include_api_keys);

        // Caller explicitly enables via request.
        let resolved = resolve_backup_options(
            &BackupRequest {
                include_api_keys: Some(true),
                ..BackupRequest::default()
            },
            canonical_defaults(),
        );
        assert!(resolved.include_api_keys);

        // Explicit request false wins even if defaults were true (defensive).
        let resolved = resolve_backup_options(
            &BackupRequest {
                include_api_keys: Some(false),
                ..BackupRequest::default()
            },
            BackupOptions {
                include_api_keys: true,
                ..canonical_defaults()
            },
        );
        assert!(!resolved.include_api_keys);
    }

    /// Mirrors Go `TestBackupService_Backup_WithUsageStats` golden intent
    /// (`backup_test.go` lines 322-363): usage stats expose the api-key value
    /// only when `IncludeAPIKeys` is true. Resolution rule is the gatekeeper.
    #[test]
    fn usage_stats_resolution_only_emits_api_key_when_include_api_keys() {
        // Without api-keys: usage stats enabled, api keys disabled.
        let resolved = resolve_backup_options(
            &BackupRequest {
                include_usage_stats: Some(true),
                ..BackupRequest::default()
            },
            canonical_defaults(),
        );
        assert!(resolved.include_usage_stats);
        assert!(!resolved.include_api_keys);

        // With api-keys: usage stats AND api keys enabled (matches the second
        // `service.Backup(ctx, BackupOptions{IncludeAPIKeys: true,
        // IncludeUsageStats: true})` invocation in the Go test).
        let resolved = resolve_backup_options(
            &BackupRequest {
                include_usage_stats: Some(true),
                include_api_keys: Some(true),
                ..BackupRequest::default()
            },
            canonical_defaults(),
        );
        assert!(resolved.include_usage_stats);
        assert!(resolved.include_api_keys);
    }

    // ====================================================================
    // S06 — AutoBackupSettings default + frequency enum parity
    // ====================================================================

    #[test]
    fn auto_backup_settings_default_matches_go_system_default_go() {
        let settings = AutoBackupSettings::default();

        // Parity: `system_default.go` lines 57-67.
        assert!(!settings.enabled);
        assert_eq!(settings.frequency, BackupFrequency::Daily);
        assert_eq!(settings.data_storage_id, 0);
        assert!(settings.include_channels);
        assert!(settings.include_models);
        // S05: api keys NOT in default auto-backup.
        assert!(!settings.include_api_keys);
        assert!(settings.include_model_prices);
        assert!(!settings.include_usage_stats);
        assert!(!settings.include_request_logs);
        assert_eq!(settings.retention_days, 30);
        assert_eq!(settings.last_backup_at, None);
        assert_eq!(settings.last_backup_error, None);
    }

    #[test]
    fn auto_backup_settings_to_backup_options_preserves_six_include_flags() {
        // Parity: Go `performBackup` (`autobackup.go` lines 93-100).
        let settings = AutoBackupSettings {
            include_channels: true,
            include_models: true,
            include_api_keys: false,
            include_model_prices: true,
            include_usage_stats: false,
            include_request_logs: false,
            ..AutoBackupSettings::default()
        };
        let opts = settings.to_backup_options();

        assert!(!opts.include_projects); // auto-backup never backs up projects.
        assert!(opts.include_channels);
        assert!(opts.include_models);
        assert!(!opts.include_api_keys);
        assert!(opts.include_model_prices);
        assert!(!opts.include_usage_stats);
        assert!(!opts.include_request_logs);
    }

    #[test]
    fn auto_backup_settings_serializes_frequency_as_snake_case() {
        let settings = AutoBackupSettings {
            frequency: BackupFrequency::Weekly,
            ..AutoBackupSettings::default()
        };
        let serialized = serde_json::to_string(&settings).unwrap_or_default();
        assert!(serialized.contains("\"frequency\":\"weekly\""));
    }

    // ====================================================================
    // S07 — scheduling predicates + retention cutoff
    // ====================================================================

    fn at(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, 0, 0)
            .single()
            .unwrap_or_default()
    }

    /// Parity: Go `BackupService.shouldRunBackup` (`autobackup.go` lines
    /// 74-85). Daily always runs; weekly runs on Sunday; monthly on the 1st.
    #[test]
    fn should_run_on_mirrors_go_should_run_backup() {
        let daily = AutoBackupSettings {
            frequency: BackupFrequency::Daily,
            ..AutoBackupSettings::default()
        };
        let weekly = AutoBackupSettings {
            frequency: BackupFrequency::Weekly,
            ..AutoBackupSettings::default()
        };
        let monthly = AutoBackupSettings {
            frequency: BackupFrequency::Monthly,
            ..AutoBackupSettings::default()
        };

        // 2024-03-01 is a Friday; 2024-03-03 is Sunday.
        let friday = at(2024, 3, 1, 2);
        let sunday = at(2024, 3, 3, 2);

        assert!(daily.should_run_on(friday));
        assert!(daily.should_run_on(sunday));

        // Weekly: only Sunday.
        assert!(!weekly.should_run_on(friday));
        assert!(weekly.should_run_on(sunday));

        // Monthly: only the 1st.
        assert!(monthly.should_run_on(friday)); // friday is march 1
        assert!(!monthly.should_run_on(sunday)); // sunday is march 3
    }

    #[test]
    fn next_backup_run_weekly_picks_next_sunday_at_02utc() {
        // Parity intent: cron `"0 2 * * *"` filtered by shouldRunBackup.
        // 2024-03-04 (Monday) 12:00 → next Sunday 03-10 02:00.
        let weekly = AutoBackupSettings {
            frequency: BackupFrequency::Weekly,
            ..AutoBackupSettings::default()
        };
        let now = at(2024, 3, 4, 12);

        let next = weekly.next_backup_run(None, now);

        assert_eq!(next, at(2024, 3, 10, 2));
        assert_eq!(next.weekday(), Weekday::Sun);
        assert_eq!(next.hour(), 2);
    }

    #[test]
    fn next_backup_run_monthly_picks_first_of_next_month_at_02utc() {
        let monthly = AutoBackupSettings {
            frequency: BackupFrequency::Monthly,
            ..AutoBackupSettings::default()
        };
        // 2024-03-04 12:00 → next eligible slot is 2024-04-01 02:00.
        let now = at(2024, 3, 4, 12);
        let next = monthly.next_backup_run(None, now);

        assert_eq!(next, at(2024, 4, 1, 2));
        assert_eq!(next.day(), 1);
    }

    #[test]
    fn next_backup_run_daily_returns_today_slot_when_before_02utc() {
        let daily = AutoBackupSettings {
            frequency: BackupFrequency::Daily,
            ..AutoBackupSettings::default()
        };
        // 2024-03-04 00:30 → today's slot 02:00 is still ahead.
        let now = Utc
            .with_ymd_and_hms(2024, 3, 4, 0, 30, 0)
            .single()
            .unwrap_or_default();
        let next = daily.next_backup_run(None, now);

        assert_eq!(next, at(2024, 3, 4, 2));
    }

    #[test]
    fn next_backup_run_advances_past_last_run() {
        // If we already ran today after the slot, the next daily run is
        // tomorrow's 02:00 (idempotent "don't fire twice in one day").
        let daily = AutoBackupSettings {
            frequency: BackupFrequency::Daily,
            ..AutoBackupSettings::default()
        };
        let last = at(2024, 3, 4, 3); // ran today at 03:00
        let now = at(2024, 3, 4, 12);

        let next = daily.next_backup_run(Some(last), now);

        assert_eq!(next, at(2024, 3, 5, 2));
    }

    #[test]
    fn retention_cutoff_zero_days_disables_retention() {
        // Parity: Go `if settings.RetentionDays > 0` gate
        // (`autobackup.go` line 119).
        let settings = AutoBackupSettings {
            retention_days: 0,
            ..AutoBackupSettings::default()
        };
        assert_eq!(settings.retention_cutoff(at(2024, 3, 10, 2)), None);
    }

    #[test]
    fn retention_cutoff_subtracts_retention_days_from_now() {
        // Parity: Go `cutoff := time.Now().AddDate(0, 0, -retentionDays)`
        // (`autobackup.go` line 139).
        let settings = AutoBackupSettings {
            retention_days: 7,
            ..AutoBackupSettings::default()
        };
        let now = at(2024, 3, 10, 2);
        assert_eq!(settings.retention_cutoff(now), Some(at(2024, 3, 3, 2)));
    }

    // ====================================================================
    // S11 — restore dry-run foreign-key validation (extension)
    // ====================================================================

    #[test]
    fn restore_dry_run_flags_unknown_channel_name_reference() {
        // Parity intent: Go `restoreChannelModelPrices` skips a price when
        // its referenced channel is absent (`restore.go` lines 457-464). The
        // dry-run surfaces this as a structured problem instead of silently
        // dropping the row.
        let manifest = json!({
            "resources": [
                {
                    "kind": "channel",
                    "name": "Channel 1",
                    "status": "enabled"
                },
                {
                    "kind": "model_price",
                    "channel_name": "Missing Channel",
                    "status": "enabled"
                }
            ]
        });

        let report = validate_restore_dry_run_manifest(&manifest);

        assert!(!report.valid);
        assert!(report.errors.iter().any(|err| matches!(
            err,
            RestoreValidationError::UnknownEnumValue {
                field,
                value,
                index: Some(1),
            } if field == "resources.channel_name" && value == "Missing Channel"
        )));
    }

    #[test]
    fn restore_dry_run_flags_unknown_project_name_reference() {
        let manifest = json!({
            "resources": [
                {
                    "kind": "project",
                    "name": "Default"
                },
                {
                    "kind": "api_key",
                    "project_name": "Ghost Project",
                    "status": "enabled"
                }
            ]
        });

        let report = validate_restore_dry_run_manifest(&manifest);

        assert!(!report.valid);
        assert!(report.errors.iter().any(|err| matches!(
            err,
            RestoreValidationError::UnknownEnumValue {
                field,
                value,
                index: Some(1),
            } if field == "resources.project_name" && value == "Ghost Project"
        )));
    }

    #[test]
    fn restore_dry_run_accepts_known_parent_references() {
        // The model_price's channel_name and the api_key's project_name both
        // resolve to earlier resources → no foreign-key errors.
        let manifest = json!({
            "resources": [
                {
                    "kind": "channel",
                    "name": "Channel 1",
                    "status": "enabled"
                },
                {
                    "kind": "project",
                    "name": "Default"
                },
                {
                    "kind": "model_price",
                    "channel_name": "Channel 1",
                    "status": "enabled"
                },
                {
                    "kind": "api_key",
                    "project_name": "Default",
                    "status": "enabled"
                }
            ]
        });

        let report = validate_restore_dry_run_manifest(&manifest);

        assert_eq!(
            report
                .errors
                .iter()
                .filter(|err| matches!(
                    err,
                    RestoreValidationError::UnknownEnumValue { field, .. }
                        if field.starts_with("resources.channel_name")
                            || field.starts_with("resources.project_name")
                ))
                .count(),
            0
        );
    }

    #[test]
    fn restore_dry_run_empty_parent_reference_is_skipped() {
        // An empty channel_name/project_name should NOT be flagged — Go's
        // resolver treats empty names as "no reference" (see
        // `hasBackupChannelRef` at `restore.go` line 957). Use a valid `kind`
        // so the only possible errors are FK errors.
        let manifest = json!({
            "resources": [
                {
                    "kind": "backup",
                    "channel_name": "",
                    "project_name": ""
                }
            ]
        });

        let report = validate_restore_dry_run_manifest(&manifest);

        assert!(report.errors.is_empty());
    }

    // ====================================================================
    // S06 — backup archive manifest parse
    //
    // Parity: Go `BackupData` unmarshal in `restore.go` line 37 + golden
    // `TestBackupService_Restore_InvalidJSON` at `restore_test.go`
    // lines 500-513 (malformed JSON → error).
    // ====================================================================

    /// Mirrors the Go `TestBackupService_Restore_InvalidJSON` golden intent:
    /// malformed JSON must surface as an error, never panic.
    #[test]
    fn s06_parse_backup_manifest_rejects_invalid_json() -> Result<(), serde_json::Error> {
        let result = parse_backup_manifest(b"invalid json");
        assert!(result.is_err());
        Ok(())
    }

    /// Mirrors Go `BackupData` (`types.go` lines 13-23): the canonical
    /// version `BackupVersion = "1.3"`, a timestamp, and one of each entity
    /// section round-trips through serde preserving the snake_case tags Go
    /// emits. Note the explicit `channel_model_prices` / `api_keys` /
    /// `usage_requests` / `usage_logs` snake_case tags — these are NOT
    /// converted to camelCase (Go source is the contract).
    #[test]
    fn s06_parse_backup_manifest_round_trips_canonical_archive()
    -> Result<(), Box<dyn std::error::Error>> {
        let archive = json!({
            "version": BACKUP_VERSION,
            "timestamp": "2024-03-10T02:00:00Z",
            "projects": [{"name": "Default"}],
            "channels": [{"name": "Channel 1"}],
            "models": [{"model_id": "gpt-4"}],
            "channel_model_prices": [{"channel_name": "Channel 1", "model_id": "gpt-4"}],
            "api_keys": [{"name": "key-1"}],
            "usage_requests": [{"id": 1}],
            "usage_logs": [{"id": 1, "request_id": 1}],
            "metadata": {"source": "test"}
        });
        let bytes = serde_json::to_vec(&archive)?;

        let manifest = parse_backup_manifest(&bytes)?;

        assert_eq!(manifest.version, BACKUP_VERSION);
        assert_eq!(
            manifest.timestamp,
            Utc.with_ymd_and_hms(2024, 3, 10, 2, 0, 0)
                .single()
                .unwrap_or_default()
        );
        assert!(manifest.entities.projects.is_some());
        assert!(manifest.entities.channels.is_some());
        assert!(manifest.entities.models.is_some());
        assert!(manifest.entities.channel_model_prices.is_some());
        assert!(manifest.entities.api_keys.is_some());
        assert!(manifest.entities.usage_requests.is_some());
        assert!(manifest.entities.usage_logs.is_some());
        assert_eq!(
            manifest
                .metadata
                .as_ref()
                .and_then(|v| v.get("source"))
                .and_then(Value::as_str),
            Some("test")
        );

        // Re-serialize and confirm snake_case tags survive the round-trip.
        let reserialized = serde_json::to_string(&manifest)?;
        assert!(reserialized.contains("\"channel_model_prices\""));
        assert!(reserialized.contains("\"api_keys\""));
        assert!(reserialized.contains("\"usage_requests\""));
        assert!(reserialized.contains("\"usage_logs\""));
        // camelCase must NOT appear for these sections.
        assert!(!reserialized.contains("channelModelPrices"));
        assert!(!reserialized.contains("apiKeys"));
        Ok(())
    }

    /// Missing optional sections deserialize as `None` (Go `omitempty`).
    /// The required `version` + `timestamp` fields remain mandatory.
    #[test]
    fn s06_parse_backup_manifest_treats_entity_sections_as_optional()
    -> Result<(), serde_json::Error> {
        let archive = json!({
            "version": BACKUP_VERSION,
            "timestamp": "2024-03-10T02:00:00Z",
        });
        let bytes = serde_json::to_vec(&archive)?;

        let manifest = parse_backup_manifest(&bytes)?;

        assert_eq!(manifest.entities, BackupEntities::default());
        assert_eq!(manifest.entities.total_entries(), 0);
        Ok(())
    }

    #[test]
    fn s06_backup_entities_total_entries_sums_present_sections() {
        let entities = BackupEntities {
            channels: Some(json!([{"name": "a"}, {"name": "b"}])),
            models: Some(json!([{"id": 1}])),
            usage_logs: Some(json!([{}, {}, {}, {}])),
            ..BackupEntities::default()
        };
        assert_eq!(entities.total_entries(), 7);
    }

    #[test]
    fn s06_backup_entities_total_entries_sums_pricing_configuration_tables() {
        let entities = BackupEntities {
            channel_model_prices: Some(json!([{"id": 1}])),
            pricing_configuration: Some(json!({
                "accounting_settings": [{"key": "system_general_settings"}],
                "price_books": [{"id": 10}, {"id": 11}],
                "price_book_versions": [{"id": 20}],
                "pricing_change_audits": [{"id": 30}, {"id": 31}, {"id": 32}],
                "format_metadata": {"ignored": true}
            })),
            ..BackupEntities::default()
        };
        assert_eq!(entities.total_entries(), 8);
    }

    // ====================================================================
    // S09 — backup format version compatibility
    //
    // Parity: Go `restore.go` lines 41-47 version check + golden
    // `TestBackupService_Restore_InvalidVersion` at `restore_test.go`
    // lines 515-535 (unknown version → "backup version mismatch" error).
    // ====================================================================

    #[test]
    fn s09_supported_backup_versions_keep_legacy_v13_restore_compatibility() {
        // Parity: Go `BackupVersion`, `BackupVersionV3`, `BackupVersionV2`,
        // `BackupVersionV1` (`types.go` lines 192-197).
        let supported = supported_backup_versions();
        assert!(supported.contains(&BACKUP_VERSION));
        assert!(supported.contains(&BACKUP_VERSION_V4));
        assert!(supported.contains(&BACKUP_VERSION_V3));
        assert!(supported.contains(&BACKUP_VERSION_V2));
        assert!(supported.contains(&BACKUP_VERSION_V1));
    }

    #[test]
    fn s09_legacy_v13_archive_without_pricing_configuration_still_parses()
    -> Result<(), serde_json::Error> {
        let archive = json!({
            "version": BACKUP_VERSION_V4,
            "timestamp": "2024-03-10T02:00:00Z",
            "channel_model_prices": [{"id": 1}]
        });
        let manifest = parse_backup_manifest(&serde_json::to_vec(&archive)?)?;

        assert_eq!(manifest.version, BACKUP_VERSION_V4);
        assert!(manifest.entities.channel_model_prices.is_some());
        assert!(manifest.entities.pricing_configuration.is_none());
        assert!(validate_backup_version(&manifest.version, supported_backup_versions()).is_ok());
        Ok(())
    }

    #[test]
    fn s09_validate_backup_version_accepts_every_supported_version() {
        for v in supported_backup_versions() {
            assert!(validate_backup_version(v, supported_backup_versions()).is_ok());
        }
    }

    /// Mirrors `TestBackupService_Restore_InvalidVersion`: an unknown version
    /// string is rejected with a structured error whose message reproduces the
    /// Go `"backup version mismatch: expected %s, got %s"` shape.
    #[test]
    fn s09_validate_backup_version_rejects_unknown_version() {
        let result = validate_backup_version("invalid-version", supported_backup_versions());
        match result {
            Err(BackupServiceError::BackupVersionMismatch { expected, got }) => {
                assert_eq!(expected, BACKUP_VERSION);
                assert_eq!(got, "invalid-version");
            }
            other => panic!("expected BackupVersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn s09_validate_backup_version_rejects_empty_string() {
        assert!(validate_backup_version("", supported_backup_versions()).is_err());
    }

    #[test]
    fn s09_validate_backup_version_accepts_explicit_supported_subset() {
        // Caller can narrow the supported set (e.g. only the current version).
        assert!(validate_backup_version(BACKUP_VERSION, &[BACKUP_VERSION]).is_ok());
        assert!(validate_backup_version(BACKUP_VERSION_V1, &[BACKUP_VERSION]).is_err());
    }

    // ====================================================================
    // S12 — streaming-chunk plan
    //
    // Parity intent: Go `restoreUsageLogs` bulk-inserts every
    // `usageBackupBatchSize = 500` rows (`restore.go` lines 1174, 1271) and
    // `existingUsageRequests` pages reads by the same constant
    // (`restore.go` lines 996-1028). `streaming_plan` reproduces the batched
    // walk shape as a pure function.
    // ====================================================================

    #[test]
    fn s12_streaming_plan_uses_go_default_batch_size_of_500() {
        // Parity: Go `usageBackupBatchSize = 500` (`backup_ops.go` line 18).
        assert_eq!(USAGE_BACKUP_BATCH_SIZE, 500);
    }

    #[test]
    fn s12_streaming_plan_zero_entries_yields_zero_batches() {
        let plan = streaming_plan(std::iter::empty::<usize>());
        assert_eq!(plan.total_entries, 0);
        assert_eq!(plan.batch_count, 0);
        assert_eq!(plan.batch_ranges().count(), 0);
    }

    #[test]
    fn s12_streaming_plan_exact_multiple_produces_no_remainder() {
        // 1500 rows / 500 = 3 full batches, no tail.
        let plan = streaming_plan_with_batch([1500], 500);
        assert_eq!(plan.full_batches, 3);
        assert_eq!(plan.remainder, 0);
        assert_eq!(plan.batch_count, 3);

        let ranges: Vec<(usize, usize)> = plan.batch_ranges().collect();
        assert_eq!(ranges, vec![(0, 500), (500, 1000), (1000, 1500)]);
    }

    #[test]
    fn s12_streaming_plan_partial_tail_becomes_extra_batch() {
        // 1202 rows / 500 = 2 full batches + 1 tail of 202.
        let plan = streaming_plan_with_batch([1202], 500);
        assert_eq!(plan.full_batches, 2);
        assert_eq!(plan.remainder, 202);
        assert_eq!(plan.batch_count, 3);

        let ranges: Vec<(usize, usize)> = plan.batch_ranges().collect();
        assert_eq!(ranges, vec![(0, 500), (500, 1000), (1000, 1202)]);
    }

    #[test]
    fn s12_streaming_plan_sums_multiple_entity_counts() {
        // Two entity sections: 600 + 200 = 800 total → 1 full + 1 tail.
        let plan = streaming_plan_with_batch([600, 200], 500);
        assert_eq!(plan.total_entries, 800);
        assert_eq!(plan.full_batches, 1);
        assert_eq!(plan.remainder, 300);
        assert_eq!(plan.batch_count, 2);
    }

    #[test]
    fn s12_streaming_plan_default_batch_matches_go_constant() {
        let plan = streaming_plan_with_batch([1], USAGE_BACKUP_BATCH_SIZE);
        assert_eq!(plan.batch_size, 500);
    }

    // ====================================================================
    // S13 — sensitive-field masking
    //
    // Parity intent (CLAUDE.md "sensitive-field policy"): the Go restore path
    // never logs plaintext API keys, channel credentials, OAuth tokens, or
    // request/response bodies. See `restore.go`:
    //   * line 765/769/781 — `log.String("name", akData.Name)` (never Key)
    //   * line 594/597/622 — `log.String("channel", chData.Name)` (never
    //     `credentials.APIKey`)
    //   * lines 200-205, 225-227 — `apiKeyKeys` map holds plaintext keys but
    //     only resolved numeric IDs ever appear in log lines.
    // ====================================================================

    #[test]
    fn s13_redact_backup_log_entry_masks_api_key_field() {
        // Mirrors Go `BackupAPIKey` (`types.go` lines 39-43): the `key` field
        // carries the plaintext API key and must NEVER be surfaced.
        let entry = json!({
            "kind": "api_key",
            "name": "Backup API Key",
            "id": 42,
            "key": "sk-super-secret-never-log-me"
        });

        let safe = redact_backup_log_entry(&entry);

        assert_eq!(safe.kind, "api_key");
        assert_eq!(safe.name, "Backup API Key");
        assert_eq!(safe.id, "42");
        assert!(safe.had_secret);
        // The LogSafeEntry struct physically cannot carry the secret string.
    }

    #[test]
    fn s13_redact_backup_log_entry_masks_channel_credentials() {
        // Mirrors Go `BackupChannel.Credentials` (`types.go` lines 29-33):
        // credentials embed `APIKey`/`APIKeys`/`OAuth`.
        let entry = json!({
            "kind": "channel",
            "name": "Channel 1",
            "id": 7,
            "credentials": {"api_key": "channel-secret"}
        });

        let safe = redact_backup_log_entry(&entry);

        assert_eq!(safe.name, "Channel 1");
        assert!(safe.had_secret);
    }

    #[test]
    fn s13_redact_backup_log_entry_masks_usage_request_api_key_key() {
        // Mirrors Go `BackupUsageRequest.APIKeyKey` (`types.go` line 57).
        let entry = json!({
            "kind": "usage_request",
            "id": 100,
            "api_key_key": "sk-usage-secret",
            "request_body": {"model": "gpt-4"}
        });

        let safe = redact_backup_log_entry(&entry);

        assert!(safe.had_secret);
    }

    #[test]
    fn s13_redact_backup_log_entry_flags_oauth_token_as_secret() {
        let entry = json!({
            "kind": "channel",
            "name": "OAuth Channel",
            "oauth": {"access_token": "abc"}
        });

        let safe = redact_backup_log_entry(&entry);

        assert!(safe.had_secret);
    }

    #[test]
    fn s13_redact_backup_log_entry_no_secret_when_absent() {
        // A bare model row (Go `BackupModel`, `types.go` lines 35-37) has no
        // credential fields; logging it must not flag a secret.
        let entry = json!({
            "kind": "model",
            "name": "GPT-4",
            "id": 1,
            "developer": "openai"
        });

        let safe = redact_backup_log_entry(&entry);

        assert!(!safe.had_secret);
        assert_eq!(safe.kind, "model");
        assert_eq!(safe.name, "GPT-4");
        assert_eq!(safe.id, "1");
    }

    #[test]
    fn s13_redact_backup_log_entry_empty_string_secret_not_flagged() {
        // An empty `key: ""` carries no secret and must not trip the flag.
        let entry = json!({
            "kind": "api_key",
            "name": "empty key",
            "key": ""
        });

        let safe = redact_backup_log_entry(&entry);

        assert!(!safe.had_secret);
    }

    #[test]
    fn s13_redact_backup_log_entry_handles_missing_fields() {
        let entry = json!({});

        let safe = redact_backup_log_entry(&entry);

        assert_eq!(safe.kind, "unknown");
        assert_eq!(safe.name, "");
        assert_eq!(safe.id, "<unknown>");
        assert!(!safe.had_secret);
    }

    #[test]
    fn s13_redact_backup_log_entry_accepts_type_alias_for_kind() {
        // Some manifests use `"type"` instead of `"kind"` (mirrors the Go
        // restore-dry-run reader which checks both).
        let entry = json!({"type": "channel", "name": "x"});

        let safe = redact_backup_log_entry(&entry);

        assert_eq!(safe.kind, "channel");
    }

    // ====================================================================
    // Restore options + conflict strategy parity (Kant-the-2nd)
    //
    // Mirrors Go `backup.ConflictStrategy` + `backup.RestoreOptions`
    // (`types.go` lines 209-230) and the include-gated walk + per-entity
    // `switch opts.XxxConflictStrategy` ladders in `restore.go`
    // (lines 77-128, 371-388, 486-509, 592-626, 681-710, 763-785).
    // ====================================================================

    /// Parity: Go `backup.ConflictStrategy` constants (`types.go` lines
    /// 211-215). Wire format is the lowercase snake string.
    #[test]
    fn rcs_conflict_strategy_serializes_as_go_lowercase_string() {
        let skip = serde_json::to_string(&ConflictStrategy::Skip).unwrap_or_default();
        let overwrite = serde_json::to_string(&ConflictStrategy::Overwrite).unwrap_or_default();
        let error = serde_json::to_string(&ConflictStrategy::Error).unwrap_or_default();

        assert_eq!(skip, "\"skip\"");
        assert_eq!(overwrite, "\"overwrite\"");
        assert_eq!(error, "\"error\"");
    }

    #[test]
    fn rcs_conflict_strategy_round_trips_through_string_tags() {
        for (wire, expected) in [
            ("\"skip\"", ConflictStrategy::Skip),
            ("\"overwrite\"", ConflictStrategy::Overwrite),
            ("\"error\"", ConflictStrategy::Error),
        ] {
            let parsed: ConflictStrategy =
                serde_json::from_str(wire).unwrap_or(ConflictStrategy::Skip);
            assert_eq!(parsed, expected);
        }
    }

    /// Default `ConflictStrategy` is `Skip`, mirroring Go's zero-value
    /// (`""`) falling through every `switch` arm as a no-op.
    #[test]
    fn rcs_conflict_strategy_default_is_skip() {
        assert_eq!(ConflictStrategy::default(), ConflictStrategy::Skip);
    }

    /// Parity: Go `backup.RestoreOptions` (`types.go` lines 217-230) —
    /// the field set matches one-for-one, all defaulting to false / Skip.
    #[test]
    fn rcs_restore_options_default_is_all_false_and_skip_strategies() {
        let opts = RestoreOptions::default();
        assert!(!opts.include_projects);
        assert!(!opts.include_channels);
        assert!(!opts.include_models);
        assert!(!opts.include_api_keys);
        assert!(!opts.include_model_prices);
        assert!(!opts.include_usage_stats);
        assert!(!opts.include_request_logs);
        assert_eq!(opts.project_conflict_strategy, ConflictStrategy::Skip);
        assert_eq!(opts.channel_conflict_strategy, ConflictStrategy::Skip);
        assert_eq!(opts.model_conflict_strategy, ConflictStrategy::Skip);
        assert_eq!(opts.model_price_conflict_strategy, ConflictStrategy::Skip);
        assert_eq!(opts.api_key_conflict_strategy, ConflictStrategy::Skip);
    }

    /// Parity: Go's `switch opts.XxxConflictStrategy` ladders — when the row
    /// is new, every strategy maps to `Create`. The Go code unconditionally
    /// runs `db.X.Create()` when no existing row is found, regardless of
    /// strategy (`restore.go` lines 393-401 / 533-539 / 644-654 / 723-734 /
    /// 805-824).
    #[test]
    fn rcs_decide_restore_action_create_when_not_existing() {
        for strategy in [
            ConflictStrategy::Skip,
            ConflictStrategy::Overwrite,
            ConflictStrategy::Error,
        ] {
            assert_eq!(
                decide_restore_action(false, strategy),
                RestoreAction::Create,
                "strategy={strategy:?} must produce Create when not existing"
            );
        }
    }

    /// Parity: Go's `ConflictStrategySkip` → `log.Info("skipping ...")` +
    /// `continue` (`restore.go` lines 372-374 / 487-488 / 593-594 /
    /// 682-683 / 764-765).
    #[test]
    fn rcs_decide_restore_action_skip_when_existing_and_skip_strategy() {
        assert_eq!(
            decide_restore_action(true, ConflictStrategy::Skip),
            RestoreAction::Skip
        );
    }

    /// Parity: Go's `ConflictStrategyOverwrite` →
    /// `db.X.UpdateOneID(existing.ID).Set...().Save()`
    /// (`restore.go` lines 378-387 / 491-509 / 601-626 / 690-710 / 772-785).
    #[test]
    fn rcs_decide_restore_action_overwrite_when_existing_and_overwrite_strategy() {
        assert_eq!(
            decide_restore_action(true, ConflictStrategy::Overwrite),
            RestoreAction::Overwrite
        );
    }

    /// Parity: Go's `ConflictStrategyError` →
    /// `return fmt.Errorf("<entity> %s already exists", ...)`
    /// (`restore.go` lines 375-377 / 489-490 / 595-600 / 684-689 / 766-771).
    /// Mirrors the golden intent of
    /// `TestBackupService_Restore_ModelPriceConflictStrategy_Error`
    /// (`restore_test.go` lines 645-689): the restore aborts with an
    /// "already exists" error.
    #[test]
    fn rcs_decide_restore_action_error_when_existing_and_error_strategy() {
        assert_eq!(
            decide_restore_action(true, ConflictStrategy::Error),
            RestoreAction::Error
        );
    }

    /// `strategy_for_entity` surfaces each configured per-entity strategy,
    /// and returns `None` for `UsageData` (Go's usage path has no
    /// `ConflictStrategy`).
    #[test]
    fn rcs_strategy_for_entity_maps_each_entity_to_its_field() {
        let opts = RestoreOptions {
            project_conflict_strategy: ConflictStrategy::Error,
            channel_conflict_strategy: ConflictStrategy::Overwrite,
            model_conflict_strategy: ConflictStrategy::Skip,
            model_price_conflict_strategy: ConflictStrategy::Error,
            api_key_conflict_strategy: ConflictStrategy::Overwrite,
            ..RestoreOptions::default()
        };

        assert_eq!(
            strategy_for_entity(RestoreEntity::Projects, opts),
            Some(ConflictStrategy::Error)
        );
        assert_eq!(
            strategy_for_entity(RestoreEntity::Channels, opts),
            Some(ConflictStrategy::Overwrite)
        );
        assert_eq!(
            strategy_for_entity(RestoreEntity::Models, opts),
            Some(ConflictStrategy::Skip)
        );
        assert_eq!(
            strategy_for_entity(RestoreEntity::ModelPrices, opts),
            Some(ConflictStrategy::Error)
        );
        assert_eq!(
            strategy_for_entity(RestoreEntity::ApiKeys, opts),
            Some(ConflictStrategy::Overwrite)
        );
        // UsageData has no per-entity strategy in Go.
        assert_eq!(strategy_for_entity(RestoreEntity::UsageData, opts), None);
    }

    /// `entity_is_included` mirrors Go's per-branch include gate, in
    /// particular the `IncludeUsageStats || IncludeRequestLogs` rule for
    /// usage data (`restore.go` line 121).
    #[test]
    fn rcs_entity_is_included_usage_data_needs_either_flag() {
        let neither = RestoreOptions::default();
        assert!(!entity_is_included(RestoreEntity::UsageData, neither));

        let only_stats = RestoreOptions {
            include_usage_stats: true,
            ..RestoreOptions::default()
        };
        assert!(entity_is_included(RestoreEntity::UsageData, only_stats));

        let only_logs = RestoreOptions {
            include_request_logs: true,
            ..RestoreOptions::default()
        };
        assert!(entity_is_included(RestoreEntity::UsageData, only_logs));
    }

    #[test]
    fn rcs_entity_is_included_other_entities_match_their_flag() {
        let opts = RestoreOptions {
            include_projects: true,
            include_channels: true,
            include_models: true,
            include_api_keys: true,
            include_model_prices: true,
            ..RestoreOptions::default()
        };
        assert!(entity_is_included(RestoreEntity::Projects, opts));
        assert!(entity_is_included(RestoreEntity::Channels, opts));
        assert!(entity_is_included(RestoreEntity::Models, opts));
        assert!(entity_is_included(RestoreEntity::ApiKeys, opts));
        assert!(entity_is_included(RestoreEntity::ModelPrices, opts));
        // UsageData is off because neither usage flag is set.
        assert!(!entity_is_included(RestoreEntity::UsageData, opts));
    }

    /// `plan_restore` returns Go's fixed walk order (channels → model-prices
    /// → models → projects → api-keys → usage-data) filtered by the include
    /// flags. Parity: Go `restore()` lines 78-125.
    #[test]
    fn rcs_plan_restore_empty_options_yields_empty_plan() {
        let plan = plan_restore(RestoreOptions::default());
        assert!(plan.is_empty());
    }

    #[test]
    fn rcs_plan_restore_preserves_go_walk_order() {
        // All flags on: the full Go sequence must come back unchanged.
        let opts = RestoreOptions {
            include_projects: true,
            include_channels: true,
            include_models: true,
            include_api_keys: true,
            include_model_prices: true,
            include_usage_stats: true,
            include_request_logs: true,
            ..RestoreOptions::default()
        };
        let plan = plan_restore(opts);
        assert_eq!(
            plan,
            vec![
                RestoreEntity::Channels,
                RestoreEntity::ModelPrices,
                RestoreEntity::Models,
                RestoreEntity::Projects,
                RestoreEntity::ApiKeys,
                RestoreEntity::UsageData,
            ]
        );
    }

    /// Mirrors the intent of `TestBackupService_Restore_ModelPriceConflict`
    /// triple (`restore_test.go` lines 537-689): only model prices are
    /// included, so the plan should contain exactly `[ModelPrices]`.
    #[test]
    fn rcs_plan_restore_model_price_only_matches_go_test_intent() {
        let opts = RestoreOptions {
            include_model_prices: true,
            model_price_conflict_strategy: ConflictStrategy::Overwrite,
            ..RestoreOptions::default()
        };
        let plan = plan_restore(opts);
        assert_eq!(plan, vec![RestoreEntity::ModelPrices]);
    }

    /// `conflict_error_message` reproduces the Go `<entity> <key> already
    /// exists` format used by every `ConflictStrategyError` arm
    /// (`restore.go` lines 377 / 490 / 600 / 689 / 771).
    ///
    /// Go actually uses two shapes:
    ///  - single-key entities (`project %s`, `channel %s`, `model %s`,
    ///    `API key %s`) follow the simple `"<label> <key> already exists"`
    ///    pattern that this helper reproduces verbatim;
    ///  - the model-price arm (`restore.go` line 490) is the bespoke form
    ///    `"channel model price already exists: channel=%s model_id=%s"`.
    ///
    /// The Go test
    /// (`TestBackupService_Restore_ModelPriceConflictStrategy_Error`,
    /// `restore_test.go` line 688) only asserts `Contains("channel model
    /// price already exists")` — so we golden-check the simple shape here
    /// and document that the model-price arm builds its own bespoke message
    /// at the call site rather than going through this helper.
    #[test]
    fn rcs_conflict_error_message_matches_go_single_key_format() {
        // Go `restore.go` line 377: `"project %s already exists"`.
        assert_eq!(
            conflict_error_message("project", "Default"),
            "project Default already exists"
        );

        // Go `restore.go` line 600: `"channel %s already exists"`.
        assert_eq!(
            conflict_error_message("channel", "Channel 1"),
            "channel Channel 1 already exists"
        );

        // Go `restore.go` line 689: `"model %s already exists"`.
        assert_eq!(
            conflict_error_message("model", "gpt-4"),
            "model gpt-4 already exists"
        );

        // Go `restore.go` line 771: `"API key %s already exists"`.
        assert_eq!(
            conflict_error_message("API key", "sk-test"),
            "API key sk-test already exists"
        );
    }

    // ====================================================================
    // P13-002 — backup section emission + projection rules
    //
    // Mirrors the Go `doBackup` walk (`backup_ops.go` lines 39-167) and
    // the api-key projection ladders (`backup_ops.go` lines 206-220,
    // 270-284). Each Go test's golden intent is reproduced against the
    // pure helpers above so the wired dump driver can rely on them
    // without re-deriving the contract.
    // ====================================================================

    /// `BackupSection::emit_order` lists every section Go's `doBackup`
    /// populates (`backup_ops.go` lines 46-148) in declared order.
    #[test]
    fn p13_emit_order_lists_go_sections_and_pricing_extension() {
        assert_eq!(
            BackupSection::emit_order(),
            &[
                BackupSection::Projects,
                BackupSection::Channels,
                BackupSection::ModelPrices,
                BackupSection::PricingConfiguration,
                BackupSection::Models,
                BackupSection::ApiKeys,
                BackupSection::UsageRequests,
                BackupSection::UsageLogs,
            ]
        );
    }

    /// Mirrors `TestBackupService_Backup` (`backup_test.go` lines 210-248)
    /// golden intent: with channels/models/model-prices all flagged on,
    /// exactly those three sections are emitted, in declared order.
    #[test]
    fn p13_section_emission_backup_full_three_sections() {
        let opts = BackupOptions {
            include_channels: true,
            include_models: true,
            include_model_prices: true,
            ..BackupOptions::default()
        };
        let emitted: Vec<BackupSection> = BackupSection::emit_order()
            .iter()
            .copied()
            .filter(|s| section_is_emitted(*s, opts))
            .collect();
        assert_eq!(
            emitted,
            vec![
                BackupSection::Channels,
                BackupSection::ModelPrices,
                BackupSection::PricingConfiguration,
                BackupSection::Models,
            ]
        );
    }

    /// Mirrors `TestBackupService_Backup_ExcludeModelPrices`
    /// (`backup_test.go` lines 250-272): only channels flagged → only
    /// the channels section is emitted.
    #[test]
    fn p13_section_emission_exclude_model_prices() {
        let opts = BackupOptions {
            include_channels: true,
            ..BackupOptions::default()
        };
        let emitted: Vec<BackupSection> = BackupSection::emit_order()
            .iter()
            .copied()
            .filter(|s| section_is_emitted(*s, opts))
            .collect();
        assert_eq!(emitted, vec![BackupSection::Channels]);
    }

    /// Mirrors `TestBackupService_Backup_ModelPricesOnly`
    /// (`backup_test.go` lines 274-298): only `IncludeModelPrices=true`
    /// → only model prices emitted.
    #[test]
    fn p13_section_emission_model_prices_only() {
        let opts = BackupOptions {
            include_model_prices: true,
            ..BackupOptions::default()
        };
        let emitted: Vec<BackupSection> = BackupSection::emit_order()
            .iter()
            .copied()
            .filter(|s| section_is_emitted(*s, opts))
            .collect();
        assert_eq!(
            emitted,
            vec![
                BackupSection::ModelPrices,
                BackupSection::PricingConfiguration
            ]
        );
    }

    /// Mirrors `TestBackupService_Backup_Empty` (`backup_test.go`
    /// lines 300-320): all three flags on, but no rows in the DB → all
    /// three sections emitted as empty arrays. The section-emission
    /// contract is what matters here (the empty-payload aspect is the
    /// DB-layer's responsibility).
    #[test]
    fn p13_section_emission_empty_backup_still_emits_all_flagged() {
        let opts = BackupOptions {
            include_channels: true,
            include_models: true,
            include_model_prices: true,
            ..BackupOptions::default()
        };
        let emitted: Vec<BackupSection> = BackupSection::emit_order()
            .iter()
            .copied()
            .filter(|s| section_is_emitted(*s, opts))
            .collect();
        assert_eq!(emitted.len(), 4);
    }

    /// Mirrors `TestBackupService_Backup_WithUsageStats` first invocation
    /// (`backup_test.go` lines 332-334): `IncludeUsageStats: true` alone
    /// emits usage_logs, not usage_requests or api_keys.
    #[test]
    fn p13_section_emission_usage_stats_only_logs_not_requests() {
        let opts = BackupOptions {
            include_usage_stats: true,
            ..BackupOptions::default()
        };
        assert!(section_is_emitted(BackupSection::UsageLogs, opts));
        assert!(!section_is_emitted(BackupSection::UsageRequests, opts));
        assert!(!section_is_emitted(BackupSection::ApiKeys, opts));
    }

    /// Mirrors `TestBackupService_Backup_WithRequestLogs`
    /// (`backup_test.go` lines 365-405): `IncludeRequestLogs: true` alone
    /// emits usage_requests, not usage_logs or api_keys.
    #[test]
    fn p13_section_emission_request_logs_only_requests_not_logs() {
        let opts = BackupOptions {
            include_request_logs: true,
            ..BackupOptions::default()
        };
        assert!(section_is_emitted(BackupSection::UsageRequests, opts));
        assert!(!section_is_emitted(BackupSection::UsageLogs, opts));
        assert!(!section_is_emitted(BackupSection::ApiKeys, opts));
    }

    /// Mirrors `TestBackupService_Backup_WithAPIKeys`
    /// (`backup_apikey_test.go` lines 14-48): `IncludeAPIKeys: true`
    /// emits the api_keys section (which carries plaintext keys +
    /// `project_name`).
    #[test]
    fn p13_section_emission_api_keys_when_flagged() {
        let opts = BackupOptions {
            include_api_keys: true,
            ..BackupOptions::default()
        };
        assert!(section_is_emitted(BackupSection::ApiKeys, opts));
        let blank = BackupOptions::default();
        assert!(!section_is_emitted(BackupSection::ApiKeys, blank));
    }

    /// Parity: Go `BackupData` json tags (`types.go` lines 14-22) are
    /// snake_case; `channel_model_prices`, `api_keys`, `usage_requests`,
    /// `usage_logs` would be mis-converted by `rename_all = "camelCase"`.
    /// Re-asserting the canonical tags here protects against accidental
    /// future renaming.
    #[test]
    fn p13_section_json_tag_matches_go_snake_case_verbatim() {
        for section in BackupSection::emit_order() {
            let tag = section_json_tag(*section);
            // None of the canonical tags contain uppercase letters.
            assert!(
                !tag.chars().any(|c| c.is_ascii_uppercase()),
                "section {section:?} tag {tag:?} must be lowercase snake_case"
            );
        }
        assert_eq!(
            section_json_tag(BackupSection::ModelPrices),
            "channel_model_prices"
        );
        assert_eq!(section_json_tag(BackupSection::ApiKeys), "api_keys");
        assert_eq!(
            section_json_tag(BackupSection::UsageRequests),
            "usage_requests"
        );
        assert_eq!(section_json_tag(BackupSection::UsageLogs), "usage_logs");
    }

    /// Parity: Go `BackupData` only `channels` and `models` lack
    /// `omitempty` (`types.go` lines 17-18) — they are always serialized
    /// as `[]` even when empty. Every other section is dropped when nil.
    /// This is load-bearing for restore round-trip parity: a missing
    /// `channels` array would change the parsed `BackupData.Channels`
    /// from `[]` to `nil` and break the Go test invariants.
    #[test]
    fn p13_section_omits_when_empty_only_channels_and_models_keep_empty_arrays() {
        assert!(!section_omits_when_empty(BackupSection::Channels));
        assert!(!section_omits_when_empty(BackupSection::Models));
        assert!(section_omits_when_empty(BackupSection::Projects));
        assert!(section_omits_when_empty(BackupSection::ModelPrices));
        assert!(section_omits_when_empty(
            BackupSection::PricingConfiguration
        ));
        assert!(section_omits_when_empty(BackupSection::ApiKeys));
        assert!(section_omits_when_empty(BackupSection::UsageRequests));
        assert!(section_omits_when_empty(BackupSection::UsageLogs));
    }

    /// Parity: Go `doBackup` (`backup_ops.go` lines 162-166) uses
    /// `json.Marshal` (compact) when usage stats or request logs are
    /// included (large dumps), and `json.MarshalIndent` otherwise.
    #[test]
    fn p13_archive_compact_encoding_only_when_usage_data_present() {
        let stats = BackupOptions {
            include_usage_stats: true,
            ..BackupOptions::default()
        };
        let logs = BackupOptions {
            include_request_logs: true,
            ..BackupOptions::default()
        };
        let small = BackupOptions {
            include_channels: true,
            include_models: true,
            ..BackupOptions::default()
        };
        assert!(archive_use_compact_encoding(stats));
        assert!(archive_use_compact_encoding(logs));
        assert!(!archive_use_compact_encoding(small));
        assert!(!archive_use_compact_encoding(BackupOptions::default()));
    }

    /// Mirrors `TestBackupService_Backup_WithUsageStats` first invocation
    /// (`backup_test.go` lines 332-352): `IncludeAPIKeys=false,
    /// IncludeUsageStats=true` → projected `APIKeyKey` is empty even when
    /// the api-key map would otherwise contain the value. The Go test
    /// additionally asserts `NotContains("sk-test-key-1")` against the
    /// whole archive — the contract is "no plaintext key leaks when the
    /// flag is off".
    #[test]
    fn p13_projected_usage_log_api_key_key_empty_when_flag_off() {
        let mut keys = BTreeMap::new();
        keys.insert(1_i64, "sk-test-key-1".to_string());

        // Flag off → empty regardless of the map or id.
        assert_eq!(projected_usage_log_api_key_key(1, &keys, false), "");
        // Flag on → map value comes through.
        assert_eq!(
            projected_usage_log_api_key_key(1, &keys, true),
            "sk-test-key-1"
        );
    }

    /// Parity: Go `backupUsageLog` (`backup_ops.go` line 278): only
    /// resolves when `ul.APIKeyID != 0`. An ID of 0 means "no api-key
    /// associated" → empty string in the archive.
    #[test]
    fn p13_projected_usage_log_api_key_key_zero_id_returns_empty() {
        let keys = BTreeMap::new();
        assert_eq!(projected_usage_log_api_key_key(0, &keys, true), "");
    }

    /// Parity: Go map lookup yields "" when the ID isn't present (Go's
    /// zero value for `string`). Mirrors the defensive case where a usage
    /// log references an api-key that was deleted before backup.
    #[test]
    fn p13_projected_usage_log_api_key_key_unknown_id_returns_empty() {
        let keys = BTreeMap::<i64, String>::new();
        assert_eq!(projected_usage_log_api_key_key(42, &keys, true), "");
    }

    /// Mirrors `TestBackupService_Backup_WithUsageStats` second invocation
    /// (`backup_test.go` lines 354-363): `IncludeAPIKeys=true,
    /// IncludeUsageStats=true` → projected `APIKeyKey` carries the
    /// plaintext `"sk-test-key-1"`. The map is built by
    /// `build_api_key_keys_map`, which is the analogue of Go's
    /// pre-walking api-keys at `backup_ops.go` lines 227-238.
    #[test]
    fn p13_build_api_key_keys_map_populated_only_when_flag_on() {
        let api_keys = [
            (1_i64, "sk-test-key-1".to_string()),
            (2_i64, "sk-test-key-2".to_string()),
        ];

        // Flag off → empty map (Go skips the query entirely).
        let off = build_api_key_keys_map(api_keys.iter().map(|(k, v)| (k, v)), false);
        assert!(off.is_empty());

        // Flag on → fully populated.
        let on = build_api_key_keys_map(api_keys.iter().map(|(k, v)| (k, v)), true);
        assert_eq!(on.len(), 2);
        assert_eq!(on.get(&1).map(String::as_str), Some("sk-test-key-1"));
        assert_eq!(on.get(&2).map(String::as_str), Some("sk-test-key-2"));
    }

    /// Parity: Go `backupUsageRequest` (`backup_ops.go` lines 214-216)
    /// only writes `APIKeyKey` when both `includeAPIKeyValues` is true AND
    /// the loaded `req.Edges.APIKey` is non-nil. Mirrors the two
    /// invocations of `TestBackupService_Backup_WithRequestLogs`
    /// (`backup_test.go` lines 375-405): first emits empty
    /// (`IncludeAPIKeys=false`), second emits `"sk-test-key-1"`.
    #[test]
    fn p13_projected_usage_request_api_key_key_gated_by_flag_and_edge() {
        // Flag off → empty even when edge is present.
        assert_eq!(
            projected_usage_request_api_key_key(Some("sk-test-key-1"), false),
            ""
        );
        // Flag on, edge present → plaintext.
        assert_eq!(
            projected_usage_request_api_key_key(Some("sk-test-key-1"), true),
            "sk-test-key-1"
        );
        // Flag on, edge absent → empty.
        assert_eq!(projected_usage_request_api_key_key(None, true), "");
    }

    // =====================================================================
    // Backup dump (Go `doBackup`, `backup_ops.go:39-167`)
    //
    // These close the gap where the stored archive was a metadata placeholder
    // ("database dump is not implemented yet"): the service now assembles the
    // real per-section archive from a `BackupDataSource`.
    // =====================================================================

    /// Row source returning one canned row per section, recording which
    /// sections were actually asked for.
    #[derive(Debug, Default)]
    struct FakeDataSource {
        requested: Mutex<Vec<BackupSection>>,
    }

    impl FakeDataSource {
        async fn requested(&self) -> Vec<BackupSection> {
            self.requested.lock().await.clone()
        }
    }

    #[async_trait]
    impl BackupDataSource for FakeDataSource {
        async fn load_section(
            &self,
            _ctx: &RequestContext,
            section: BackupSection,
        ) -> BackupServiceResult<Value> {
            self.requested.lock().await.push(section);
            Ok(json!([{ "tag": section_json_tag(section) }]))
        }
    }

    fn all_sections_opts() -> BackupOptions {
        BackupOptions {
            include_projects: true,
            include_channels: true,
            include_models: true,
            include_api_keys: true,
            include_model_prices: true,
            include_usage_stats: true,
            include_request_logs: true,
        }
    }

    fn parse_archive(bytes: &[u8]) -> BackupServiceResult<Value> {
        serde_json::from_slice(bytes)
            .map_err(|e| BackupServiceError::Storage(StorageError::Serialization(e.to_string())))
    }

    /// Every enabled section is loaded and lands under its Go json tag, and the
    /// envelope carries the Go `version` / `timestamp` fields.
    #[tokio::test]
    async fn dump_emits_all_enabled_sections_under_go_tags() -> BackupServiceResult<()> {
        let source = Arc::new(FakeDataSource::default());
        let service = BackupService::new(
            Arc::new(FakeBackupRepo::default()),
            Arc::new(FakeStorage::default()),
        )
        .with_data_source(source.clone());

        let bytes = service.dump(&ctx(), all_sections_opts()).await?;
        let archive = parse_archive(&bytes)?;

        assert_eq!(archive["version"], json!(BACKUP_VERSION));
        assert!(
            archive["timestamp"].is_string(),
            "timestamp must be emitted"
        );

        for section in BackupSection::emit_order() {
            let tag = section_json_tag(*section);
            assert_eq!(
                archive[tag],
                json!([{ "tag": tag }]),
                "section {tag} must carry its loaded rows"
            );
        }

        // All seven sections asked for, in Go's emit order.
        assert_eq!(
            source.requested().await,
            BackupSection::emit_order().to_vec()
        );
        Ok(())
    }

    /// Disabled sections are never loaded. Go drops the omitempty ones entirely
    /// and emits `null` for `channels`/`models` (a nil slice marshals to null).
    #[tokio::test]
    async fn dump_skips_disabled_sections_and_applies_omitempty() -> BackupServiceResult<()> {
        let source = Arc::new(FakeDataSource::default());
        let service = BackupService::new(
            Arc::new(FakeBackupRepo::default()),
            Arc::new(FakeStorage::default()),
        )
        .with_data_source(source.clone());

        // Everything off — Go's `BackupOptions` zero value.
        let bytes = service.dump(&ctx(), BackupOptions::default()).await?;
        let archive = parse_archive(&bytes)?;

        assert!(
            source.requested().await.is_empty(),
            "no section may be loaded when every include flag is false"
        );

        let obj = match archive.as_object() {
            Some(obj) => obj,
            None => return Err(BackupServiceError::DataSourceUnavailable),
        };
        // `channels` + `models` have no omitempty in Go -> present as null.
        assert_eq!(obj.get("channels"), Some(&Value::Null));
        assert_eq!(obj.get("models"), Some(&Value::Null));
        // The omitempty sections are dropped outright.
        assert!(obj.get("projects").is_none());
        assert!(obj.get("channel_model_prices").is_none());
        assert!(obj.get("pricing_configuration").is_none());
        assert!(obj.get("api_keys").is_none());
        assert!(obj.get("usage_requests").is_none());
        assert!(obj.get("usage_logs").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn dump_model_prices_also_emits_pricing_configuration() -> BackupServiceResult<()> {
        let source = Arc::new(FakeDataSource::default());
        let service = BackupService::new(
            Arc::new(FakeBackupRepo::default()),
            Arc::new(FakeStorage::default()),
        )
        .with_data_source(source.clone());
        let opts = BackupOptions {
            include_model_prices: true,
            ..BackupOptions::default()
        };

        let archive = parse_archive(&service.dump(&ctx(), opts).await?)?;

        assert!(archive.get("channel_model_prices").is_some());
        assert!(archive.get("pricing_configuration").is_some());
        assert_eq!(
            source.requested().await,
            vec![
                BackupSection::ModelPrices,
                BackupSection::PricingConfiguration,
            ]
        );
        Ok(())
    }

    /// Go picks compact `json.Marshal` when a usage section is included and
    /// indented `MarshalIndent` otherwise (`backup_ops.go:163-167`).
    #[test]
    fn serialize_matches_go_indent_selection() -> BackupServiceResult<()> {
        let archive = json!({"version": BACKUP_VERSION, "channels": [{"id": 1}]});

        let pretty = serialize_backup_archive(&archive, BackupOptions::default())?;
        let pretty_text = String::from_utf8_lossy(&pretty).to_string();
        assert!(
            pretty_text.contains('\n'),
            "no-usage backup must be indented, got: {pretty_text}"
        );

        let with_usage = BackupOptions {
            include_usage_stats: true,
            ..BackupOptions::default()
        };
        let compact = serialize_backup_archive(&archive, with_usage)?;
        let compact_text = String::from_utf8_lossy(&compact).to_string();
        assert!(
            !compact_text.contains('\n'),
            "usage backup must be compact, got: {compact_text}"
        );

        // Same logical content either way.
        assert_eq!(parse_archive(&pretty)?, parse_archive(&compact)?);
        Ok(())
    }

    /// Without a wired source the dump refuses rather than silently writing a
    /// placeholder (the old behavior that made stored backups useless).
    #[tokio::test]
    async fn dump_without_data_source_is_an_error() {
        let service = BackupService::new(
            Arc::new(FakeBackupRepo::default()),
            Arc::new(FakeStorage::default()),
        );

        let result = service.dump(&ctx(), all_sections_opts()).await;

        assert!(
            matches!(result, Err(BackupServiceError::DataSourceUnavailable)),
            "expected DataSourceUnavailable, got {result:?}"
        );
    }

    /// `create_backup_with_dump` uploads the real archive (not a placeholder)
    /// under the job's storage key, with a real byte size.
    #[tokio::test]
    async fn create_backup_with_dump_uploads_real_archive() -> BackupServiceResult<()> {
        let storage = Arc::new(FakeStorage::default());
        let repo = Arc::new(FakeBackupRepo::default());
        let service = BackupService::new(repo.clone(), storage.clone())
            .with_data_source(Arc::new(FakeDataSource::default()));

        let job = service
            .create_backup_with_dump(&ctx(), "project-1", "nightly", all_sections_opts())
            .await?;

        assert_eq!(repo.backup_count().await, 1);
        let objects = storage.objects().await;
        assert_eq!(objects.len(), 1, "exactly one archive object");
        assert_eq!(objects[0].metadata.key, job.storage_key);
        assert!(
            objects[0].metadata.content_length > 0,
            "archive size must be real, got {}",
            objects[0].metadata.content_length
        );

        let archive = parse_archive(&objects[0].bytes)?;
        assert_eq!(archive["version"], json!(BACKUP_VERSION));
        assert_eq!(archive["channels"], json!([{ "tag": "channels" }]));
        // The old placeholder note must be gone.
        assert!(
            archive.get("note").is_none(),
            "archive must not be a placeholder"
        );
        Ok(())
    }
}
