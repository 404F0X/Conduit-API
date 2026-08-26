//! Thread repository — Rust port of `conduit/internal/server/biz/thread.go`.
//!
//! Mirrors the Go `ThreadService.GetOrCreateThread` data-access surface
//! against the `Thread` Ent schema (`internal/ent/schema/thread.go`): atomic
//! get-or-create by `(project_id, thread_id)` and get-by-id.
//!
//! ## No soft delete
//! The Go `Thread` schema's `Mixin()` returns only `TimeMixin{}` — there is
//! **no `SoftDeleteMixin`**, hence no `deleted_at` column and no
//! `*_with_deleted` / restore surface here. All rows are live; the schema has
//! no delete path at this layer.
//!
//! ## Uniqueness (mirrors Go `Thread.Indexes`)
//! `threads_by_thread_id` is a unique index on `thread_id` alone (global, not
//! project-scoped). The Go service queries by `(thread_id, project_id)` and,
//! on a create-time constraint violation, re-queries by the same pair — the
//! `get_or_create_thread` impl mirrors this by checking the pair first and
//! treating a racing insert as "already exists".
//!
//! ## Storage model (RUST-P3-002 S13 batch 2)
//! `ThreadRow` is a hand-written typed struct (`id`, `project_id`,
//! `thread_id`, `created_at`, `updated_at`) mirroring the Go entity columns
//! 1:1. The previous `minimal_row!` `extra`-map shape is retired.

use crate::repo::{RepoError, RepoResult, RequestContext, guard_project_access};
use crate::row::ThreadRow;
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

/// Build a `ThreadRow` from a get-or-create call. Both timestamps start at
/// `now` (Go `TimeMixin` defaults both to the creation instant).
fn new_thread_row(id: String, project_id: String, thread_id: String, now: &str) -> ThreadRow {
    let now_dt = parse_dt(now);
    ThreadRow {
        id,
        project_id,
        thread_id,
        created_at: now_dt,
        updated_at: now_dt,
    }
}

// --- trait -----------------------------------------------------------------

/// Repository surface for the `threads` table.
///
/// Method naming convention (mirrors other repos):
/// - `*_unchecked` — skips the policy guard; callers must have already enforced
///   authorization. Public methods wrap these with `guard_project_access`.
///
/// Threads have **no soft delete** — there is no `*_with_deleted` surface.
#[async_trait]
pub trait ThreadRepo: Send + Sync {
    /// Atomic get-or-create by `(project_id, thread_id)`. Returns the existing
    /// row if one already exists for this pair; otherwise inserts and returns a
    /// new row. Mirrors Go `ThreadService.GetOrCreateThread`.
    async fn get_or_create_thread_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        thread_id: &str,
        now: String,
    ) -> RepoResult<ThreadRow>;

    async fn get_or_create_thread(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        thread_id: &str,
        now: String,
    ) -> RepoResult<ThreadRow> {
        guard_project_access(ctx, project_id, crate::policy::ProjectAccess::Write)?;
        self.get_or_create_thread_unchecked(ctx, project_id, thread_id, now)
            .await
    }

    /// Get an existing thread by `(project_id, thread_id)`. Returns `None` when
    /// no row matches. Mirrors Go `ThreadService.GetThreadByID`.
    async fn find_thread_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        thread_id: &str,
    ) -> RepoResult<Option<ThreadRow>>;

    async fn find_thread(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        thread_id: &str,
    ) -> RepoResult<Option<ThreadRow>> {
        guard_project_access(ctx, project_id, crate::policy::ProjectAccess::Read)?;
        self.find_thread_unchecked(ctx, project_id, thread_id).await
    }
}

// --- in-memory impl --------------------------------------------------------

/// In-memory `ThreadRepo` backed by a `BTreeMap<String, ThreadRow>` keyed by
/// row `id`. Used by tests and the future fake backend; the SQL backend will
/// replace the storage layer without changing the trait surface.
#[derive(Debug, Default)]
pub struct InMemoryThreadRepo {
    rows: Mutex<BTreeMap<String, ThreadRow>>,
}

impl InMemoryThreadRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("thread repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl ThreadRepo for InMemoryThreadRepo {
    async fn get_or_create_thread_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        thread_id: &str,
        now: String,
    ) -> RepoResult<ThreadRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("thread repo"))?;

        // First-write-wins: a concurrent insert for the same pair is observed
        // as "already exists" and the original row is returned — mirrors the
        // Go `ConstraintError` re-query path.
        if let Some(existing) = guard
            .values()
            .find(|r| r.project_id == project_id && r.thread_id == thread_id)
        {
            return Ok(existing.clone());
        }

        // `thread_id` is globally unique in Go; derive a stable row id from the
        // pair so re-creates collapse deterministically even if a caller races.
        let id = format!("{project_id}:{thread_id}");
        let row = new_thread_row(id, project_id.into(), thread_id.into(), &now);
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_thread_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        thread_id: &str,
    ) -> RepoResult<Option<ThreadRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("thread repo"))?
            .values()
            .find(|r| r.project_id == project_id && r.thread_id == thread_id)
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
        let repo = InMemoryThreadRepo::new();
        let ctx = ctx_allowed();

        let row = repo
            .get_or_create_thread(&ctx, "proj-1", "th-a", "2024-01-01T00:00:00Z".into())
            .await?;

        assert_eq!(row.project_id, "proj-1");
        assert_eq!(row.thread_id, "th-a");
        // TimeMixin: both timestamps start at the creation instant.
        assert_eq!(row.created_at.to_rfc3339(), "2024-01-01T00:00:00+00:00");
        assert_eq!(row.updated_at, row.created_at);
        assert_eq!(repo.len()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn get_or_create_returns_existing_on_second_call() -> RepoResult<()> {
        let repo = InMemoryThreadRepo::new();
        let ctx = ctx_allowed();

        let first = repo
            .get_or_create_thread(&ctx, "proj-1", "th-a", "2024-01-01T00:00:00Z".into())
            .await?;
        let second = repo
            .get_or_create_thread(&ctx, "proj-1", "th-a", "2024-01-02T00:00:00Z".into())
            .await?;

        assert_eq!(first.id, second.id);
        assert_eq!(repo.len()?, 1);
        // Created-at is immutable post-create: the second call's distinct
        // `now` must NOT overwrite the original creation instant.
        assert_eq!(second.created_at, first.created_at);
        assert_eq!(second.created_at.to_rfc3339(), "2024-01-01T00:00:00+00:00");
        Ok(())
    }

    #[tokio::test]
    async fn get_or_create_isolates_by_project() -> RepoResult<()> {
        let repo = InMemoryThreadRepo::new();
        let ctx = ctx_allowed();

        let a = repo
            .get_or_create_thread(&ctx, "proj-1", "th-a", "t0".into())
            .await?;
        let b = repo
            .get_or_create_thread(&ctx, "proj-2", "th-a", "t1".into())
            .await?;

        assert_ne!(a.id, b.id);
        assert_eq!(repo.len()?, 2);

        // find_thread is project-scoped.
        let found = repo.find_thread(&ctx, "proj-2", "th-a").await?;
        assert_eq!(found.map(|r| r.id), Some(b.id));
        Ok(())
    }

    #[tokio::test]
    async fn find_returns_none_for_unknown_pair() -> RepoResult<()> {
        let repo = InMemoryThreadRepo::new();
        let ctx = ctx_allowed();

        let found = repo.find_thread(&ctx, "proj-1", "missing").await?;
        assert!(found.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_denies_anonymous() {
        let repo = InMemoryThreadRepo::new();
        let anon = ctx_anon();

        let denied = repo
            .get_or_create_thread(&anon, "proj-1", "th-a", "t0".into())
            .await;

        assert!(matches!(denied, Err(RepoError::Policy(_))));
        assert_eq!(repo.len(), Ok(0));
    }
}
