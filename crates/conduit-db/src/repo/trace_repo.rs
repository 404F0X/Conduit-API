//! Trace repository — Rust port of `conduit/internal/server/biz/trace.go`.
//!
//! Mirrors the Go `TraceService.GetOrCreateTrace` data-access surface against
//! the `Trace` Ent schema (`internal/ent/schema/trace.go`): atomic get-or-create
//! by `(project_id, trace_id)`, get-by-id, and first-trace-of-thread lookup.
//!
//! ## No soft delete
//! The Go `Trace` schema's `Mixin()` returns only `TimeMixin{}` — there is
//! **no `SoftDeleteMixin`**, hence no `deleted_at` column and no
//! `*_with_deleted` / restore surface here. All rows are live; the schema has
//! no delete path at this layer.
//!
//! ## Uniqueness (mirrors Go `Trace.Indexes`)
//! `traces_by_trace_id` is a unique index on `trace_id` alone (global, not
//! project-scoped). The Go service queries by `(trace_id, project_id)` and, on
//! a create-time constraint violation, re-queries by the same pair — the
//! `get_or_create_trace` impl mirrors this by checking the pair first and
//! treating a racing insert as "already exists".
//!
//! ## Storage model (RUST-P3-002 S13 batch 2)
//! `TraceRow` is a hand-written typed struct (`id`, `project_id`, `trace_id`,
//! optional `thread_id`, `created_at`, `updated_at`) mirroring the Go entity
//! columns 1:1. The previous `minimal_row!` `extra`-map shape is retired.

use crate::repo::{RepoError, RepoResult, RequestContext, guard_project_access};
use crate::row::TraceRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::Mutex;

// --- timestamp parsing -------------------------------------------------------

/// Parse an ISO-8601 / RFC-3339 / date-only string into `DateTime<Utc>`.
/// Falls back to the Unix epoch on failure so comparisons don't panic —
/// well-formed inputs are unaffected. Same recipe as `user_repo.rs`.
fn parse_dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .map(|n| DateTime::<Utc>::from_naive_utc_and_offset(n, Utc))
        })
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map(|d| {
                DateTime::<Utc>::from_naive_utc_and_offset(
                    d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                    Utc,
                )
            })
        })
        .unwrap_or_else(|_| DateTime::from_timestamp(0, 0).unwrap_or_default())
}

/// Build a `TraceRow` from a get-or-create call. `id` is the caller-supplied
/// stable identifier; `thread_id` is optional (mirrors Go's
/// `SetNillableThreadID`). Both timestamps start at `now` (Go `TimeMixin`
/// defaults both to the creation instant).
fn new_trace_row(
    id: String,
    project_id: String,
    trace_id: String,
    thread_id: Option<String>,
    now: &str,
) -> TraceRow {
    let now_dt = parse_dt(now);
    TraceRow {
        id,
        project_id,
        trace_id,
        thread_id,
        created_at: now_dt,
        updated_at: now_dt,
    }
}

// --- trait -----------------------------------------------------------------

/// Repository surface for the `traces` table.
///
/// Method naming convention (mirrors other repos):
/// - `*_unchecked` — skips the policy guard; callers must have already enforced
///   authorization. Public methods wrap these with `guard_project_access`.
///
/// Traces have **no soft delete** — there is no `*_with_deleted` surface.
#[async_trait]
pub trait TraceRepo: Send + Sync {
    /// Atomic get-or-create by `(project_id, trace_id)`. Returns the existing
    /// row if one already exists for this pair; otherwise inserts and returns a
    /// new row. Mirrors Go `TraceService.GetOrCreateTrace`.
    ///
    /// `thread_id` is only applied on the create path (the Go schema marks
    /// `thread_id` as `Immutable`); an existing row keeps its original value.
    async fn get_or_create_trace_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        trace_id: &str,
        thread_id: Option<String>,
        now: String,
    ) -> RepoResult<TraceRow>;

    async fn get_or_create_trace(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        trace_id: &str,
        thread_id: Option<String>,
        now: String,
    ) -> RepoResult<TraceRow> {
        guard_project_access(ctx, project_id, crate::policy::ProjectAccess::Write)?;
        self.get_or_create_trace_unchecked(ctx, project_id, trace_id, thread_id, now)
            .await
    }

    /// Get an existing trace by `(project_id, trace_id)`. Returns `None` when
    /// no row matches. Mirrors Go `TraceService.GetTraceByID`.
    async fn find_trace_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        trace_id: &str,
    ) -> RepoResult<Option<TraceRow>>;

    async fn find_trace(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        trace_id: &str,
    ) -> RepoResult<Option<TraceRow>> {
        guard_project_access(ctx, project_id, crate::policy::ProjectAccess::Read)?;
        self.find_trace_unchecked(ctx, project_id, trace_id).await
    }
}

// --- in-memory impl --------------------------------------------------------

/// In-memory `TraceRepo` backed by a `BTreeMap<String, TraceRow>` keyed by row
/// `id`. Used by tests and the future fake backend; the SQL backend will
/// replace the storage layer without changing the trait surface.
#[derive(Debug, Default)]
pub struct InMemoryTraceRepo {
    rows: Mutex<BTreeMap<String, TraceRow>>,
}

impl InMemoryTraceRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("trace repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl TraceRepo for InMemoryTraceRepo {
    async fn get_or_create_trace_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        trace_id: &str,
        thread_id: Option<String>,
        now: String,
    ) -> RepoResult<TraceRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("trace repo"))?;

        // First-write-wins: a concurrent insert for the same pair is observed
        // as "already exists" and the original row is returned — mirrors the
        // Go `ConstraintError` re-query path.
        if let Some(existing) = guard
            .values()
            .find(|r| r.project_id == project_id && r.trace_id == trace_id)
        {
            return Ok(existing.clone());
        }

        // `trace_id` is globally unique in Go; derive a stable row id from the
        // pair so re-creates collapse deterministically even if a caller races.
        let id = format!("{project_id}:{trace_id}");
        let row = new_trace_row(id, project_id.into(), trace_id.into(), thread_id, &now);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_trace_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        trace_id: &str,
    ) -> RepoResult<Option<TraceRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("trace repo"))?
            .values()
            .find(|r| r.project_id == project_id && r.trace_id == trace_id)
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};

    fn ctx_allowed() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn ctx_anon() -> RequestContext {
        RequestContext::new(PolicyContext::anonymous())
    }

    #[tokio::test]
    async fn get_or_create_creates_on_first_call() -> RepoResult<()> {
        let repo = InMemoryTraceRepo::new();
        let ctx = ctx_allowed();

        let row = repo
            .get_or_create_trace(
                &ctx,
                "proj-1",
                "trace-a",
                Some("th-1".into()),
                "2024-01-01T00:00:00Z".into(),
            )
            .await?;

        assert_eq!(row.project_id, "proj-1");
        assert_eq!(row.trace_id, "trace-a");
        assert_eq!(row.thread_id.as_deref(), Some("th-1"));
        // TimeMixin: both timestamps start at the creation instant.
        assert_eq!(row.created_at.to_rfc3339(), "2024-01-01T00:00:00+00:00");
        assert_eq!(row.updated_at, row.created_at);
        assert_eq!(repo.len()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn get_or_create_returns_existing_on_second_call() -> RepoResult<()> {
        let repo = InMemoryTraceRepo::new();
        let ctx = ctx_allowed();

        let first = repo
            .get_or_create_trace(
                &ctx,
                "proj-1",
                "trace-a",
                Some("th-1".into()),
                "2024-01-01T00:00:00Z".into(),
            )
            .await?;
        // Second call supplies a different thread_id; the existing row keeps
        // its original (immutable) thread_id.
        let second = repo
            .get_or_create_trace(
                &ctx,
                "proj-1",
                "trace-a",
                Some("th-2".into()),
                "2024-01-02T00:00:00Z".into(),
            )
            .await?;

        assert_eq!(first.id, second.id);
        assert_eq!(second.thread_id.as_deref(), Some("th-1"));
        // created_at is immutable post-create (stays at the first call's now).
        assert_eq!(second.created_at, first.created_at);
        assert_eq!(repo.len()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn get_or_create_isolates_by_project() -> RepoResult<()> {
        let repo = InMemoryTraceRepo::new();
        let ctx = ctx_allowed();

        let a = repo
            .get_or_create_trace(&ctx, "proj-1", "trace-a", None, "t0".into())
            .await?;
        let b = repo
            .get_or_create_trace(&ctx, "proj-2", "trace-a", None, "t1".into())
            .await?;

        assert_ne!(a.id, b.id);
        assert_eq!(repo.len()?, 2);

        // find_trace is project-scoped.
        let found = repo.find_trace(&ctx, "proj-2", "trace-a").await?;
        assert!(found.is_some());
        let found = repo.find_trace(&ctx, "proj-1", "trace-a").await?;
        assert_eq!(found.map(|r| r.id), Some(a.id));
        Ok(())
    }

    #[tokio::test]
    async fn find_returns_none_for_unknown_pair() -> RepoResult<()> {
        let repo = InMemoryTraceRepo::new();
        let ctx = ctx_allowed();

        let found = repo.find_trace(&ctx, "proj-1", "missing").await?;
        assert!(found.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_denies_anonymous() {
        let repo = InMemoryTraceRepo::new();
        let anon = ctx_anon();

        let denied = repo
            .get_or_create_trace(&anon, "proj-1", "trace-a", None, "t0".into())
            .await;

        assert!(matches!(denied, Err(RepoError::Policy(_))));
        assert_eq!(repo.len(), Ok(0));
    }
}
