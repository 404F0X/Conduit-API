use crate::policy::{
    PolicyContext, PolicyError, ProjectAccess, require_principal, require_project_access,
};
use crate::row::{BackupRow, OidcIdentityRow, ProviderQuotaStatusRow, SystemRow};
use crate::tx::TxSlot;
use async_trait::async_trait;
use std::{collections::BTreeMap, sync::Mutex};
use thiserror::Error;

pub mod api_key_repo;
pub mod channel_model_price_repo;
pub mod channel_override_template_repo;
pub mod channel_repo;
pub mod data_storage_repo;
pub mod model_repo;
pub mod pg_api_key_repo;
pub mod pg_channel_model_price_repo;
pub mod pg_channel_override_template_repo;
pub mod pg_channel_repo;
pub mod pg_data_storage_repo;
pub mod pg_model_repo;
pub mod pg_oidc_repo;
pub mod pg_profile_template_repo;
pub mod pg_project_repo;
pub mod pg_prompt_protection_repo;
pub mod pg_prompt_repo;
pub mod pg_request_execution_repo;
pub mod pg_request_repo;
pub mod pg_role_repo;
pub mod pg_route_affinity_repo;
pub mod pg_system_repo;
pub mod pg_thread_repo;
pub mod pg_trace_repo;
pub mod pg_usage_repo;
pub mod pg_user_project_repo;
pub mod pg_user_repo;
pub mod profile_template_repo;
pub mod project_repo;
pub mod prompt_protection_repo;
pub mod prompt_repo;
pub mod request_execution_repo;
pub mod request_repo;
pub mod role_repo;
pub mod route_affinity_repo;
pub mod thread_repo;
pub mod trace_repo;
pub mod usage_repo;
pub mod user_project_repo;
pub mod user_repo;

// ---- Ambient transaction slot (RUST-P3-003 S11) ----------------------------
//
// A type-erased `TxSlot` can carry an ambient transaction while repository
// signatures remain `&RequestContext`. Concrete transaction managers own the
// backend-specific handle.
//
// The slot is intentionally `#[serde(skip)]` — it is a live, non-serializable
// runtime handle, not part of the request's logical identity. Default / Clone /
// PartialEq / Eq semantics all still hold (`TxSlot` has ptr-equality, so two
// cloned contexts sharing the same tx are equal, two with different txs are
// not — exactly what "same ctx, same tx" means in Go).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    pub policy: PolicyContext,
    pub request_id: Option<String>,
    pub project_id: Option<String>,
    pub source: String,
    /// Ambient DB transaction handle. `None` for the default pool-bound path.
    pub tx_slot: Option<TxSlot>,
}

impl RequestContext {
    pub fn new(policy: PolicyContext) -> Self {
        Self {
            policy,
            request_id: None,
            project_id: None,
            source: "internal".to_string(),
            tx_slot: None,
        }
    }

    pub fn with_project_id(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }

    /// Attach an ambient transaction slot.
    pub fn set_tx_slot(&mut self, slot: TxSlot) {
        self.tx_slot = Some(slot);
    }

    /// True when this context carries an ambient transaction (RUST-P3-003 S11).
    /// Convenience for callers that want to short-circuit pool acquisition.
    pub fn has_tx_slot(&self) -> bool {
        self.tx_slot.is_some()
    }
}

pub type RepoResult<T> = Result<T, RepoError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RepoError {
    #[error(transparent)]
    Policy(#[from] PolicyError),
    /// Reserved for repository surfaces that genuinely have no backend
    /// implementation yet (e.g. the `RequestRepo::find_request_by_cache_signature`
    /// trait default at `request_repo.rs:216`). A *failed* database call must
    /// use [`RepoError::Database`] instead — conflating the two hides real
    /// outages (disk full, table lock, constraint violation, dropped
    /// connection) behind "backend not implemented" and makes callers that
    /// branch on `NotImplemented` degrade instead of retry.
    #[error("repository backend not implemented: {0}")]
    NotImplemented(&'static str),
    /// A driver-level failure from the SQL backend. Carries an owned string of
    /// the form `"{context}: {source error}"` so the log line has both the
    /// call-site locator (which repo method issued the query) and the root
    /// cause (the sqlx / PostgreSQL message). `String` rather than
    /// `#[from] sqlx::Error` keeps the enum backend-agnostic and preserves the
    /// `PartialEq`/`Eq` derive the repo tests rely on.
    #[error("database error: {0}")]
    Database(String),
    #[error("repository lock poisoned: {0}")]
    LockPoisoned(&'static str),
    /// Returned by mutations when no row matches the supplied id (after applying
    /// the default soft-delete filter). Mirrors the Go service's "user not found"
    /// path; the caller decides whether this is a 404 or a no-op.
    #[error("entity not found: {0}")]
    NotFound(&'static str),
    /// Returned by `create_user`/`update_user` when the email collides with a
    /// live (deleted_at IS NULL) row. Mirrors the Go `(email, deleted_at)`
    /// unique index.
    #[error("email already in use")]
    EmailConflict,
    /// Returned by `create_project`/`create_role` (and updates) when the row's
    /// name (or `(project_id, name)` for roles) collides with a live row.
    /// Mirrors Go's `projects_by_name` and `roles_by_project_id_name` unique
    /// indexes — a soft-deleted row frees its key for re-registration.
    #[error("name already in use")]
    NameConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageAggregateQuery {
    pub project_id: String,
    pub start_at: Option<String>,
    pub end_at: Option<String>,
    /// Optional dashboard filters (RUST-P3-005 S17). When set, they narrow the
    /// aggregation alongside the date range and `project_id`.
    pub api_key_id: Option<String>,
    pub channel_id: Option<String>,
    pub model_id: Option<String>,
    pub source: Option<String>,
}

impl UsageAggregateQuery {
    pub fn new(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            start_at: None,
            end_at: None,
            api_key_id: None,
            channel_id: None,
            model_id: None,
            source: None,
        }
    }
}

/// Aggregated usage totals for a `(project_id, date-range)` bucket, mirroring
/// the Go dashboard aggregation (`SUM(prompt_tokens)`, `SUM(total_tokens)`,
/// `SUM(total_cost)`). `total_cost_micros` is the integer-micros representation
/// of `total_cost` in the configured accounting currency (accounting unit ×
/// 1_000_000) so the value stays in the integer domain — JSON serialization
/// exposes it as a 64-bit fixed-point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageAggregate {
    pub project_id: String,
    pub request_count: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub total_cost_micros: i64,
}

/// Typed dashboard filter (RUST-P3-005 S17). Consolidates the multi-dimension
/// filter shared by the request and usage-log dashboard queries: date range +
/// project/channel/api_key/model/source/status. Mirrors the dimensions the Go
/// `gql/dashboard.resolvers.go` aggregations narrow by (project scoping from
/// `authz` + per-resolver `timeWindow` + the group-by dimensions channel/model/
/// api_key), surfaced as a single typed struct so the request and usage repos
/// share one filter shape.
///
/// `project_id` is **required** — the dashboard is always project-scoped
/// (mirrors the Go `authz.RequireScope` + per-project index usage). Every other
/// field is optional; `None` means "no filter on this dimension". The date range
/// is inclusive on both bounds and uses ISO-8601 string compare (consistent with
/// `RequestListQuery` / `UsageListQuery`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DashboardFilter {
    /// Required project scope. The dashboard never returns rows from other
    /// projects, mirroring the Go `requests_by_project_id` /
    /// `usage_logs_by_project_id` indexes plus `authz` scope enforcement.
    pub project_id: String,
    /// Inclusive lower bound on `created_at` (ISO-8601 string compare).
    pub start_at: Option<String>,
    /// Inclusive upper bound on `created_at`.
    pub end_at: Option<String>,
    pub channel_id: Option<String>,
    pub api_key_id: Option<String>,
    pub model_id: Option<String>,
    /// `api` | `playground` | `test` (Go `request.Source` / `usagelog.Source`).
    pub source: Option<String>,
    /// Go `request.Status` (`pending`/`processing`/`completed`/`failed`/
    /// `canceled`). Only meaningful for request dashboards; usage-log queries
    /// ignore it (usage rows have no status).
    pub status: Option<String>,
}

impl DashboardFilter {
    /// Construct a project-scoped filter with no other dimensions set.
    pub fn for_project(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            ..Default::default()
        }
    }

    /// Chainable setter for the inclusive start bound.
    pub fn with_start_at(mut self, start_at: impl Into<String>) -> Self {
        self.start_at = Some(start_at.into());
        self
    }

    /// Chainable setter for the inclusive end bound.
    pub fn with_end_at(mut self, end_at: impl Into<String>) -> Self {
        self.end_at = Some(end_at.into());
        self
    }

    /// Chainable setter for the channel dimension.
    pub fn with_channel_id(mut self, channel_id: impl Into<String>) -> Self {
        self.channel_id = Some(channel_id.into());
        self
    }

    /// Chainable setter for the api-key dimension.
    pub fn with_api_key_id(mut self, api_key_id: impl Into<String>) -> Self {
        self.api_key_id = Some(api_key_id.into());
        self
    }

    /// Chainable setter for the model dimension.
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }

    /// Chainable setter for the source dimension.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Chainable setter for the request-status dimension.
    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    /// True when no narrowing dimension is set beyond the required `project_id`.
    /// Useful for the SQL backend to skip emitting no-op `WHERE` clauses.
    pub fn is_unscoped(&self) -> bool {
        self.start_at.is_none()
            && self.end_at.is_none()
            && self.channel_id.is_none()
            && self.api_key_id.is_none()
            && self.model_id.is_none()
            && self.source.is_none()
            && self.status.is_none()
    }

    /// Build a `RequestListQuery` from this filter. The caller supplies the
    /// pagination window — the filter itself carries no pagination because the
    /// request and usage queries may paginate differently for the same filter.
    pub fn to_request_query(
        &self,
        limit: u32,
        offset: u32,
    ) -> crate::repo::request_repo::RequestListQuery {
        crate::repo::request_repo::RequestListQuery {
            project_id: self.project_id.clone(),
            api_key_id: self.api_key_id.clone(),
            channel_id: self.channel_id.clone(),
            model_id: self.model_id.clone(),
            source: self.source.clone(),
            status: self.status.clone(),
            start_at: self.start_at.clone(),
            end_at: self.end_at.clone(),
            limit,
            offset,
        }
    }

    /// Build a `UsageListQuery` from this filter. `status` is dropped — the Go
    /// `UsageLog` schema has no status column.
    pub fn to_usage_query(
        &self,
        limit: u32,
        offset: u32,
    ) -> crate::repo::usage_repo::UsageListQuery {
        crate::repo::usage_repo::UsageListQuery {
            project_id: self.project_id.clone(),
            api_key_id: self.api_key_id.clone(),
            channel_id: self.channel_id.clone(),
            model_id: self.model_id.clone(),
            source: self.source.clone(),
            request_id: None,
            start_at: self.start_at.clone(),
            end_at: self.end_at.clone(),
            limit,
            offset,
        }
    }

    /// Build a `UsageAggregateQuery` from this filter. `status` is dropped —
    /// usage aggregation is over the `UsageLog` table which has no status.
    pub fn to_usage_aggregate_query(&self) -> UsageAggregateQuery {
        UsageAggregateQuery {
            project_id: self.project_id.clone(),
            start_at: self.start_at.clone(),
            end_at: self.end_at.clone(),
            api_key_id: self.api_key_id.clone(),
            channel_id: self.channel_id.clone(),
            model_id: self.model_id.clone(),
            source: self.source.clone(),
        }
    }
}

/// Pure read-modify-write JSON merge helper (RUST-P3-005 S16). Mirrors the Go
/// service-layer JSON-column update strategy (`internal/server/biz/api_key.go`
/// `UpdateAPIKeyProfiles` and `internal/server/biz/channel.go` `UpdateChannel`
/// both `Get` the row, mutate the typed struct, then `Set*` the whole column
/// back) without inventing a dialect-specific JSON-patch language — see the
/// TODO_SMALL RUST-P3-004 S16 note: "JSON update uses full read-modify-write;
/// dialect-specific JSON patch is deferred to avoid cross-DB divergence".
///
/// Semantics:
/// - If `patch` is `Value::Null`, the existing value is returned unchanged
///   (treat Null as "no-op" so callers can pass through `Option::None` as
///   `Value::Null` without clobbering the column).
/// - Otherwise, if both `existing` and `patch` are objects, returns a new
///   object that is `existing` with each key from `patch` overwritten
///   (shallow merge at the top level — matches Go's `map[k] = v` merge the
///   services actually do). Nested objects are *replaced*, not deep-mergeded,
///   matching Go's "Set the whole column" behavior.
/// - Otherwise (scalars, arrays, mismatched kinds), the `patch` wins outright
///   — same as Go's `SetSettings(input.Settings)` replacing the whole column.
///
/// `existing` of `Value::Null` is treated as an empty object when `patch` is
/// an object, so a merge into a "null column" still produces a well-formed
/// object.
pub fn merge_json_field(
    existing: serde_json::Value,
    patch: serde_json::Value,
) -> serde_json::Value {
    // Null patch is a no-op (callers passed `Option::None`).
    if patch.is_null() {
        return existing;
    }
    // Object-on-object: shallow top-level merge.
    if patch.is_object() {
        let base = if existing.is_object() {
            existing
        } else {
            // Merge into a null/non-object column yields a fresh object.
            serde_json::Value::Object(Default::default())
        };
        let mut out = base;
        if let (Some(base_map), Some(patch_map)) = (out.as_object_mut(), patch.as_object()) {
            for (k, v) in patch_map {
                base_map.insert(k.clone(), v.clone());
            }
        }
        return out;
    }
    // Anything else: patch replaces existing outright (scalar/array/kind-change).
    patch
}

pub fn guard_repo_principal(ctx: &RequestContext) -> RepoResult<()> {
    require_principal(&ctx.policy)
        .into_result()
        .map_err(Into::into)
}

pub fn guard_project_access(
    ctx: &RequestContext,
    project_id: &str,
    access: ProjectAccess,
) -> RepoResult<()> {
    require_project_access(&ctx.policy, project_id, access)
        .into_result()
        .map_err(Into::into)
}

#[derive(Debug, Default)]
pub struct InMemorySystemRepo {
    rows: Mutex<BTreeMap<String, SystemRow>>,
}

impl InMemorySystemRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = SystemRow>) -> Self {
        let rows = rows
            .into_iter()
            // The trait looks settings up by the unique `key` column (mirrors
            // Go `system.KeyEQ(...)`), so the map is keyed by `row.key`.
            .map(|row| (row.key.clone(), row))
            .collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("system repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("system repo"))?
            .is_empty())
    }
}

#[async_trait]
impl SystemRepo for InMemorySystemRepo {
    async fn get_system_value_unchecked(
        &self,
        _ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<SystemRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("system repo"))?
            .get(key)
            .cloned())
    }

    async fn set_system_value_unchecked(
        &self,
        _ctx: &RequestContext,
        row: SystemRow,
    ) -> RepoResult<SystemRow> {
        self.rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("system repo"))?
            // Upsert keyed by the unique `key` column (Go `systems_key_key`).
            .insert(row.key.clone(), row.clone());
        Ok(row)
    }
}

#[async_trait]
pub trait SystemRepo: Send + Sync {
    async fn get_system_value_unchecked(
        &self,
        ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<SystemRow>>;

    async fn set_system_value_unchecked(
        &self,
        ctx: &RequestContext,
        row: SystemRow,
    ) -> RepoResult<SystemRow>;

    async fn get_system_value(
        &self,
        ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<SystemRow>> {
        guard_repo_principal(ctx)?;
        self.get_system_value_unchecked(ctx, key).await
    }

    async fn set_system_value(
        &self,
        ctx: &RequestContext,
        row: SystemRow,
    ) -> RepoResult<SystemRow> {
        guard_repo_principal(ctx)?;
        self.set_system_value_unchecked(ctx, row).await
    }
}

// The full `UserRepo` trait + `InMemoryUserRepo` live in `user_repo.rs`.
// See `user_repo.rs` for soft-delete, pagination, and email-uniqueness semantics.
pub use user_repo::UserRepo;

// The full `ProjectRepo` trait + `InMemoryProjectRepo` live in `project_repo.rs`.
// See `project_repo.rs` for soft-delete, pagination, and name-uniqueness semantics.
pub use project_repo::ProjectRepo;

// The `UserProjectRepo` trait + `InMemoryUserProjectRepo` live in
// `user_project_repo.rs`. Ports the owner/member assignment Go writes inside
// `ProjectService.CreateProject` (`client.UserProject.Create()`). Create-only
// surface today — no soft delete (Go `UserProject` has TimeMixin only).
pub use user_project_repo::{CreateUserProjectInput, InMemoryUserProjectRepo, UserProjectRepo};

// The full `RoleRepo` trait + `InMemoryRoleRepo` live in `role_repo.rs`.
// See `role_repo.rs` for (project_id, name) uniqueness, system/project levels,
// and soft-delete semantics.
pub use role_repo::RoleRepo;

// The full `ApiKeyRepo` trait + `InMemoryApiKeyRepo` live in `api_key_repo.rs`.
// See `api_key_repo.rs` for key uniqueness (single-column, spans soft-deleted
// rows), per-project name uniqueness, find_by_key cache semantics, and
// soft-delete/restore.
pub use api_key_repo::ApiKeyRepo;

// The full `ModelRepo` trait + `InMemoryModelRepo` live in `model_repo.rs`.
// See `model_repo.rs` for `(name, deleted_at)` and `(model_id, deleted_at)`
// uniqueness, status transitions, and soft-delete/restore semantics.
pub use model_repo::ModelRepo;

// The full `DataStorageRepo` trait + `InMemoryDataStorageRepo` live in
// `data_storage_repo.rs`. See it for `(name, deleted_at)` uniqueness,
// primary-record lookup, and soft-delete/restore semantics.
pub use data_storage_repo::DataStorageRepo;

// The full `ChannelRepo` trait + `InMemoryChannelRepo` live in
// `channel_repo.rs`. See it for `(name, deleted_at)` uniqueness, the
// `enabled` projection that backs the candidate-selection cache, cache
// signature derivation, tag filtering, status semantics, and
// soft-delete/restore. Channels are global (not project-scoped), mirroring
// the Go `ChannelService` which queries with no project filter.
pub use channel_repo::ChannelRepo;

// The full `RequestRepo` trait + `InMemoryRequestRepo` live in
// `request_repo.rs`. See it for the dashboard list filter
// (date range + project/channel/api_key/model/source/status, RUST-P3-005 S17),
// status transitions (pending -> processing -> completed/failed/canceled),
// external-id lookup, and content_saved marking. Requests have **no
// soft delete** — the Go `Request` schema uses only `TimeMixin`, not
// `SoftDeleteMixin`, so there is no `deleted_at` column and no
// `*_with_deleted` surface.
pub use request_repo::{
    CreateRequestInput, InMemoryRequestRepo, RequestListQuery, RequestListResult, RequestRepo,
    UpdateRequestInput,
};

// The full `RequestExecutionRepo` trait + `InMemoryRequestExecutionRepo` live
// in `request_execution_repo.rs`. See it for the execution CRUD (RUST-P3-005
// S10), status semantics (mirrors Go `requestexecution.Status`), the
// `created_at` ASC ordering (mirrors the
// `request_executions_by_request_id_created_at` index), and the
// `reclaim_stale_processing` cleanup that mirrors Go
// `ClearStaleProcessingOnStartup` for executions (request.go:1106-1113).
pub use request_execution_repo::{
    CreateRequestExecutionInput, InMemoryRequestExecutionRepo, RequestExecutionRepo,
    UpdateRequestExecutionInput,
};

// The full `UsageRepo` trait + `InMemoryUsageRepo` live in `usage_repo.rs`.
// See it for usage-log insert, dashboard aggregation (token/cost totals over a
// date range + project/channel/api_key/model/source filter), and paginated
// listing. UsageLogs have **no soft delete** — the Go `UsageLog` schema uses
// only `TimeMixin`, not `SoftDeleteMixin`. `total_cost` is f64 in Go; the repo
// stores the raw float in `extra` and exposes micros via the aggregate.
pub use usage_repo::{
    CreateUsageLogInput, InMemoryUsageRepo, UsageListQuery, UsageListResult, UsageRepo,
};

// The full `TraceRepo` trait + `InMemoryTraceRepo` live in `trace_repo.rs`.
// See `trace_repo.rs` for the atomic get-or-create by `(project_id, trace_id)`
// (no soft delete — the Go `Trace` schema uses only `TimeMixin`), find-by-pair,
// and the `thread_id` immutable-on-create semantics.
pub use trace_repo::{InMemoryTraceRepo, TraceRepo};

// The full `ThreadRepo` trait + `InMemoryThreadRepo` live in `thread_repo.rs`.
// See `thread_repo.rs` for the atomic get-or-create by `(project_id,
// thread_id)` and find-by-pair. Threads have **no soft delete** — the Go
// `Thread` schema uses only `TimeMixin`, not `SoftDeleteMixin`.
pub use thread_repo::{InMemoryThreadRepo, ThreadRepo};

#[async_trait]
pub trait OidcRepo: Send + Sync {
    async fn find_oidc_identity_unchecked(
        &self,
        ctx: &RequestContext,
        identity_id: &str,
    ) -> RepoResult<Option<OidcIdentityRow>>;

    async fn find_oidc_identity(
        &self,
        ctx: &RequestContext,
        identity_id: &str,
    ) -> RepoResult<Option<OidcIdentityRow>> {
        guard_repo_principal(ctx)?;
        self.find_oidc_identity_unchecked(ctx, identity_id).await
    }

    async fn list_oidc_identities_by_user_unchecked(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Vec<OidcIdentityRow>>;

    async fn list_oidc_identities_by_user(
        &self,
        ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Vec<OidcIdentityRow>> {
        guard_repo_principal(ctx)?;
        self.list_oidc_identities_by_user_unchecked(ctx, user_id)
            .await
    }
}

#[async_trait]
pub trait ProviderQuotaRepo: Send + Sync {
    async fn get_provider_quota_unchecked(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<Option<ProviderQuotaStatusRow>>;

    async fn get_provider_quota(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<Option<ProviderQuotaStatusRow>> {
        guard_repo_principal(ctx)?;
        self.get_provider_quota_unchecked(ctx, channel_id).await
    }
}

#[async_trait]
pub trait BackupRepo: Send + Sync {
    async fn create_backup_unchecked(
        &self,
        ctx: &RequestContext,
        row: BackupRow,
    ) -> RepoResult<BackupRow>;

    async fn create_backup(&self, ctx: &RequestContext, row: BackupRow) -> RepoResult<BackupRow> {
        guard_repo_principal(ctx)?;
        self.create_backup_unchecked(ctx, row).await
    }
}

// --- RUST-P3-005 S14: InMemory impls for OidcRepo / ProviderQuotaRepo /
//     BackupRepo ----------------------------------------------------------
//
// The three traits above previously had zero InMemory impls, so they could not
// be used in tests/services. The impls below mirror the
// `InMemoryDataStorageRepo` / `InMemorySystemRepo` pattern: a `BTreeMap`
// behind a `Mutex`, a `Default`, and a full trait surface. Since S13 batch 3,
// `OidcIdentityRow` / `ProviderQuotaStatusRow` are hand-written typed structs
// (see `row/mod.rs` for the Go schema mapping):
// - OidcIdentityRow: issuer/subject/email/idp_name/last_login_at/user_id
//   (Go `internal/ent/schema/oidc_identity.go`). Lookup is by identity id.
// - ProviderQuotaStatusRow: channel_id/provider_type/status/quota_data/
//   next_reset_at/ready/next_check_at
//   (Go `internal/ent/schema/provider_quota_status.go`). Lookup is by
//   `channel_id` (unique index).
// - BackupRow: no Go Ent schema exists yet (the migration has no `backups`
//   table); the row is a forward-looking placeholder (still `minimal_row!`),
//   so the impl simply stores the row keyed by `id`. The trait surface is
//   `create_backup` only.

/// In-memory `OidcRepo` for unit tests and the bootstrap runtime. Keyed by the
/// `OidcIdentityRow.id`. The Go `OIDCIdentity` schema is soft-deleted
/// (`SoftDeleteMixin`) with a `(issuer, subject, deleted_at)` unique index, but
/// the trait surface is a single `find_oidc_identity(identity_id)` lookup, so
/// the impl just stores rows by id.
#[derive(Debug, Default)]
pub struct InMemoryOidcRepo {
    rows: Mutex<BTreeMap<String, OidcIdentityRow>>,
}

impl InMemoryOidcRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = OidcIdentityRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("oidc repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl OidcRepo for InMemoryOidcRepo {
    async fn find_oidc_identity_unchecked(
        &self,
        _ctx: &RequestContext,
        identity_id: &str,
    ) -> RepoResult<Option<OidcIdentityRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("oidc repo"))?
            .get(identity_id)
            .cloned())
    }

    async fn list_oidc_identities_by_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Vec<OidcIdentityRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("oidc repo"))?
            .values()
            .filter(|row| row.user_id == user_id)
            .cloned()
            .collect())
    }
}

/// In-memory `ProviderQuotaRepo` for unit tests. Keyed by `channel_id` (the Go
/// `ProviderQuotaStatus` schema declares `index.Fields("channel_id").Unique()`,
/// schema:25). The row's `id` field is unused as a key here — `channel_id` is
/// the lookup dimension per the trait surface.
#[derive(Debug, Default)]
pub struct InMemoryProviderQuotaRepo {
    by_channel: Mutex<BTreeMap<String, ProviderQuotaStatusRow>>,
}

impl InMemoryProviderQuotaRepo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert rows keyed by their typed `channel_id` column (the Go schema
    /// declares `index.Fields("channel_id").Unique()`, schema:25).
    pub fn from_rows(rows: impl IntoIterator<Item = ProviderQuotaStatusRow>) -> Self {
        let by_channel = rows
            .into_iter()
            .map(|row| (row.channel_id.clone(), row))
            .collect();
        Self {
            by_channel: Mutex::new(by_channel),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .by_channel
            .lock()
            .map_err(|_| RepoError::LockPoisoned("provider quota repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl ProviderQuotaRepo for InMemoryProviderQuotaRepo {
    async fn get_provider_quota_unchecked(
        &self,
        _ctx: &RequestContext,
        channel_id: &str,
    ) -> RepoResult<Option<ProviderQuotaStatusRow>> {
        Ok(self
            .by_channel
            .lock()
            .map_err(|_| RepoError::LockPoisoned("provider quota repo"))?
            .get(channel_id)
            .cloned())
    }
}

/// In-memory `BackupRepo` for unit tests. Keyed by `BackupRow.id`. No Go Ent
/// schema exists yet, so the impl is a plain id-keyed store.
#[derive(Debug, Default)]
pub struct InMemoryBackupRepo {
    rows: Mutex<BTreeMap<String, BackupRow>>,
}

impl InMemoryBackupRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = BackupRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("backup repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl BackupRepo for InMemoryBackupRepo {
    async fn create_backup_unchecked(
        &self,
        _ctx: &RequestContext,
        row: BackupRow,
    ) -> RepoResult<BackupRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("backup repo"))?;
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }
}

// The full `PromptRepo` trait + `InMemoryPromptRepo` live in `prompt_repo.rs`.
// See `prompt_repo.rs` for `(project_id, name, deleted_at)` uniqueness, CRUD,
// soft-delete/restore, status transitions, `order` field handling (SQL keyword
// — backend must quote the column), `settings` JSON, and per-project listing
// ordered by `order` ASC with an `id` ASC tie-breaker.
pub use prompt_repo::{CreatePromptInput, InMemoryPromptRepo, PromptRepo, UpdatePromptInput};

#[async_trait]
pub trait PromptProtectionRepo: Send + Sync {
    async fn list_prompt_rules_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<crate::row::PromptProtectionRuleRow>>;

    async fn list_prompt_rules(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<crate::row::PromptProtectionRuleRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.list_prompt_rules_unchecked(ctx, project_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyDecision, Principal};
    use crate::row::ProjectRow;
    use serde_json::json;

    /// Epoch timestamp for typed-row test fixtures (established crate pattern —
    /// see `usage_repo.rs::parse_dt` fallback).
    fn epoch() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(0, 0).unwrap_or_default()
    }

    #[tokio::test]
    async fn in_memory_system_repo_uses_unique_key_and_overwrites_value()
    -> Result<(), Box<dyn std::error::Error>> {
        let repo = InMemorySystemRepo::new();
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));

        // Typed rows (S13 batch 3): same unique `key`, different `value`.
        let row = |value: &str| SystemRow {
            id: "1".to_string(),
            key: "brand".to_string(),
            value: value.to_string(),
            created_at: epoch(),
            updated_at: epoch(),
            deleted_at: None,
        };

        repo.set_system_value(&ctx, row("old")).await?;
        repo.set_system_value(&ctx, row("new")).await?;

        let saved = repo
            .get_system_value(&ctx, "brand")
            .await?
            .ok_or("brand setting should exist")?;

        assert_eq!(repo.len()?, 1);
        assert!(!repo.is_empty()?);
        assert_eq!(saved.value, "new");
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_system_repo_keeps_policy_guard_on_default_methods() {
        let repo = InMemorySystemRepo::new();
        let anonymous = RequestContext::new(PolicyContext::anonymous());

        let denied = repo.get_system_value(&anonymous, "brand").await;

        assert!(matches!(denied, Err(RepoError::Policy(_))));
        assert_eq!(repo.len(), Ok(0));
        assert_eq!(repo.is_empty(), Ok(true));
    }

    #[test]
    fn project_row_json_round_trip_preserves_unknown_fields() -> Result<(), serde_json::Error> {
        let input = serde_json::json!({
            "id": "project-a",
            "name": "Project A",
            "status": "active",
            "description": "test",
            "profiles": {"default": true},
            "createdAt": "2024-01-01T00:00:00Z",
            "updatedAt": "2024-01-01T00:00:00Z"
        });

        let row: ProjectRow = serde_json::from_value(input)?;
        let output = serde_json::to_value(row)?;

        assert_eq!(output["profiles"]["default"], true);
        Ok(())
    }

    #[test]
    fn guard_helper_maps_policy_denial_to_repo_error() {
        let ctx = RequestContext::new(PolicyContext::anonymous());

        let err = guard_repo_principal(&ctx).err();

        assert!(matches!(err, Some(RepoError::Policy(_))));
    }

    #[test]
    fn decision_helper_maps_allow_to_ok() -> RepoResult<()> {
        PolicyDecision::Allow.into_result().map_err(Into::into)
    }

    // --- RUST-P3-005 S17: DashboardFilter ---------------------------------

    #[test]
    fn dashboard_filter_for_project_is_unscoped_beyond_project() {
        let f = DashboardFilter::for_project("p-1");
        assert_eq!(f.project_id, "p-1");
        assert!(f.is_unscoped());
    }

    #[test]
    fn dashboard_filter_builder_chains_all_dimensions() {
        let f = DashboardFilter::for_project("p-1")
            .with_start_at("2024-01-01T00:00:00Z")
            .with_end_at("2024-01-31T23:59:59Z")
            .with_channel_id("ch-1")
            .with_api_key_id("k-1")
            .with_model_id("gpt-4")
            .with_source("api")
            .with_status("completed");

        assert_eq!(f.project_id, "p-1");
        assert_eq!(f.start_at.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(f.end_at.as_deref(), Some("2024-01-31T23:59:59Z"));
        assert_eq!(f.channel_id.as_deref(), Some("ch-1"));
        assert_eq!(f.api_key_id.as_deref(), Some("k-1"));
        assert_eq!(f.model_id.as_deref(), Some("gpt-4"));
        assert_eq!(f.source.as_deref(), Some("api"));
        assert_eq!(f.status.as_deref(), Some("completed"));
        assert!(!f.is_unscoped());
    }

    #[test]
    fn dashboard_filter_to_request_query_preserves_all_dimensions() {
        let f = DashboardFilter::for_project("p-1")
            .with_start_at("s")
            .with_end_at("e")
            .with_channel_id("ch-1")
            .with_api_key_id("k-1")
            .with_model_id("gpt-4")
            .with_source("api")
            .with_status("completed");

        let q = f.to_request_query(50, 10);

        assert_eq!(q.project_id, "p-1");
        assert_eq!(q.start_at.as_deref(), Some("s"));
        assert_eq!(q.end_at.as_deref(), Some("e"));
        assert_eq!(q.channel_id.as_deref(), Some("ch-1"));
        assert_eq!(q.api_key_id.as_deref(), Some("k-1"));
        assert_eq!(q.model_id.as_deref(), Some("gpt-4"));
        assert_eq!(q.source.as_deref(), Some("api"));
        assert_eq!(q.status.as_deref(), Some("completed"));
        assert_eq!(q.limit, 50);
        assert_eq!(q.offset, 10);
    }

    #[test]
    fn dashboard_filter_to_usage_query_drops_status() {
        // The Go `UsageLog` schema has no status column; the conversion must
        // drop the status dimension rather than silently retaining it.
        let f = DashboardFilter::for_project("p-1")
            .with_status("completed")
            .with_channel_id("ch-1");

        let q = f.to_usage_query(20, 0);

        assert_eq!(q.project_id, "p-1");
        assert_eq!(q.channel_id.as_deref(), Some("ch-1"));
        // status is not a field on UsageListQuery — the test enforces this by
        // construction (it would not compile if the field existed).
        assert_eq!(q.limit, 20);
        assert_eq!(q.offset, 0);
    }

    #[test]
    fn dashboard_filter_to_usage_aggregate_query_drops_status() {
        let f = DashboardFilter::for_project("p-1")
            .with_status("completed")
            .with_model_id("gpt-4");

        let q = f.to_usage_aggregate_query();

        assert_eq!(q.project_id, "p-1");
        assert_eq!(q.model_id.as_deref(), Some("gpt-4"));
    }

    // --- RUST-P3-005 S16: merge_json_field --------------------------------

    #[test]
    fn merge_json_field_null_patch_is_no_op() {
        let existing = json!({"a": 1, "b": 2});
        let merged = merge_json_field(existing, json!(null));
        assert_eq!(merged, json!({"a": 1, "b": 2}));
    }

    #[test]
    fn merge_json_field_object_patch_overwrites_top_level_keys() {
        // Mirrors Go's `SetProfiles`/`SetSettings` overwrite at the top level:
        // existing keys are replaced, new keys are added, untouched keys
        // survive.
        let existing = json!({"a": 1, "b": {"x": 1}});
        let patch = json!({"b": {"y": 2}, "c": 3});
        let merged = merge_json_field(existing, patch);
        assert_eq!(merged, json!({"a": 1, "b": {"y": 2}, "c": 3}));
    }

    #[test]
    fn merge_json_field_object_patch_into_null_column_yields_object() {
        // Mirrors initializing a JSON column that was previously NULL/empty.
        let merged = merge_json_field(json!(null), json!({"k": "v"}));
        assert_eq!(merged, json!({"k": "v"}));
    }

    #[test]
    fn merge_json_field_scalar_patch_replaces_outright() {
        // Mirrors Go's `SetX(input.X)` whole-column replacement for scalars
        // and arrays.
        let merged = merge_json_field(json!({"old": true}), json!(42));
        assert_eq!(merged, json!(42));
    }

    #[test]
    fn merge_json_field_array_patch_replaces_not_concatenated() {
        // Array is a "replace" not "concatenate" — matches Go's whole-column
        // Set semantics, not a JSON-patch append.
        let existing = json!([1, 2, 3]);
        let patch = json!([4, 5]);
        let merged = merge_json_field(existing, patch);
        assert_eq!(merged, json!([4, 5]));
    }

    #[test]
    fn merge_json_field_kind_change_lets_patch_win() {
        // Object -> scalar: patch wins outright.
        let merged = merge_json_field(json!({"a": 1}), json!("string-now"));
        assert_eq!(merged, json!("string-now"));
    }

    // --- RUST-P3-005 S14: InMemoryOidcRepo / ProviderQuota / Backup --------

    #[tokio::test]
    async fn in_memory_oidc_repo_find_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go OIDCIdentity (internal/ent/schema/oidc_identity.go):
        // issuer/subject/email/idp_name/last_login_at/user_id. Lookup is by
        // identity id.
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let row = OidcIdentityRow {
            id: "oidc-1".into(),
            issuer: "https://issuer.example.com".into(),
            subject: "sub-abc".into(),
            email: Some("user@example.com".into()),
            idp_name: Some("keycloak".into()),
            last_login_at: None,
            user_id: "42".into(),
            created_at: epoch(),
            updated_at: epoch(),
            deleted_at: None,
        };
        let repo = InMemoryOidcRepo::from_rows([row]);

        let found = repo
            .find_oidc_identity(&ctx, "oidc-1")
            .await?
            .ok_or(RepoError::NotFound("oidc-1"))?;
        assert_eq!(found.issuer, "https://issuer.example.com");
        assert_eq!(found.subject, "sub-abc");
        assert_eq!(found.user_id, "42");
        assert_eq!(found.email.as_deref(), Some("user@example.com"));

        assert!(repo.find_oidc_identity(&ctx, "missing").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_oidc_repo_policy_guard_blocks_anonymous() {
        let repo = InMemoryOidcRepo::new();
        let anon = RequestContext::new(PolicyContext::anonymous());
        let denied = repo.find_oidc_identity(&anon, "oidc-1").await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));
    }

    #[tokio::test]
    async fn in_memory_provider_quota_repo_get_by_channel_round_trip()
    -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go ProviderQuotaStatus (internal/ent/schema/
        // provider_quota_status.go): channel_id (unique), provider_type enum
        // (claudecode/codex/...), status enum
        // (available/warning/exhausted/unknown), quota_data JSON,
        // next_reset_at, ready, next_check_at.
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let row = ProviderQuotaStatusRow {
            id: "1".into(),
            // The trait takes channel_id as &str; numeric ids are stringified.
            channel_id: "42".into(),
            provider_type: "codex".into(),
            status: "available".into(),
            quota_data: json!({"remaining": 100}),
            next_reset_at: None,
            ready: true,
            next_check_at: epoch(),
            created_at: epoch(),
            updated_at: epoch(),
            deleted_at: None,
        };
        let repo = InMemoryProviderQuotaRepo::from_rows([row]);

        let found = repo
            .get_provider_quota(&ctx, "42")
            .await?
            .ok_or(RepoError::NotFound("channel 42"))?;
        assert_eq!(found.status, "available");
        assert_eq!(found.provider_type, "codex");
        assert!(found.ready);
        assert_eq!(found.quota_data, json!({"remaining": 100}));

        assert!(repo.get_provider_quota(&ctx, "999").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_provider_quota_repo_policy_guard_blocks_anonymous() {
        let repo = InMemoryProviderQuotaRepo::new();
        let anon = RequestContext::new(PolicyContext::anonymous());
        let denied = repo.get_provider_quota(&anon, "42").await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));
    }

    #[tokio::test]
    async fn in_memory_backup_repo_create_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        // No Go Ent schema exists yet for backups; the row is a forward-looking
        // placeholder, so this test just exercises the create + read-back path.
        let repo = InMemoryBackupRepo::new();
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));

        let row = BackupRow::minimal("bk-1", "nightly", "completed");
        let created = repo.create_backup(&ctx, row).await?;
        assert_eq!(created.id, "bk-1");
        assert_eq!(repo.len()?, 1);

        // Re-creating the same id overwrites (no uniqueness guard on backups —
        // mirrors the trait surface, which has only create).
        let row2 = BackupRow::minimal("bk-1", "nightly-v2", "completed");
        repo.create_backup_unchecked(&ctx, row2).await?;
        assert_eq!(repo.len()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_backup_repo_policy_guard_blocks_anonymous() {
        let repo = InMemoryBackupRepo::new();
        let anon = RequestContext::new(PolicyContext::anonymous());
        let row = BackupRow::minimal("bk-1", "nightly", "completed");
        let denied = repo.create_backup(&anon, row).await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));
    }
}
