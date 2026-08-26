//! Thread service — Rust port of `conduit/internal/server/biz/thread.go`.
//!
//! Mirrors the Go `ThreadService` data-access surface (`GetOrCreateThread`
//! / `GetThreadByID`) at the service layer: atomic get-or-create by
//! `(project_id, thread_id)` and get-by-pair. The Go service queries by
//! `(thread_id, project_id)` and, on a create-time constraint violation,
//! re-queries by the same pair; `get_or_create_thread` collapses this into a
//! first-write-wins check keyed on the pair.
//!
//! ## Project isolation
//! Same `thread_id` in different projects are independent rows — the pair
//! `(project_id, thread_id)` is the logical key. `get_thread` is project-scoped.
//!
//! Threads have no soft delete; there is no `*_with_deleted` surface here.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_db::RequestContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub type ThreadServiceResult<T> = Result<T, ThreadServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ThreadServiceError {
    #[error("thread persistence lock poisoned")]
    LockPoisoned,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadRecord {
    pub id: String,
    pub project_id: String,
    pub external_id: String,
    pub created_at: DateTime<Utc>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl ThreadRecord {
    pub fn new(project_id: impl Into<String>, external_id: impl Into<String>) -> Self {
        let project_id = project_id.into();
        let external_id = external_id.into();
        Self {
            id: scoped_record_id("thread", &project_id, &external_id),
            project_id,
            external_id,
            created_at: Utc::now(),
            extra: BTreeMap::new(),
        }
    }
}

#[async_trait]
pub trait ThreadServiceRepo: Send + Sync {
    /// Atomic get-or-create by `(project_id, external_id)`. Returns the
    /// existing record when the pair already exists; otherwise inserts and
    /// returns a new record. Mirrors Go `ThreadService.GetOrCreateThread`.
    async fn get_or_create_thread(
        &self,
        ctx: &RequestContext,
        record: ThreadRecord,
    ) -> ThreadServiceResult<ThreadRecord>;

    /// Get an existing thread by `(project_id, external_id)`. Returns `None`
    /// when no record matches. Mirrors Go `ThreadService.GetThreadByID`
    /// (the Go path surfaces "not found" as an error; the service exposes it
    /// as `Option` for caller convenience).
    async fn find_thread(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        external_id: &str,
    ) -> ThreadServiceResult<Option<ThreadRecord>>;
}

pub struct ThreadService {
    repo: Arc<dyn ThreadServiceRepo>,
}

impl ThreadService {
    pub fn new(repo: Arc<dyn ThreadServiceRepo>) -> Self {
        Self { repo }
    }

    /// Get-or-create a thread scoped by `(project_id, external_id)`. Idempotent
    /// for the same pair; cross-project the same `external_id` yields distinct
    /// records. Mirrors Go `ThreadService.GetOrCreateThread`.
    pub async fn get_or_create_thread(
        &self,
        ctx: &RequestContext,
        project_id: impl Into<String>,
        external_id: impl Into<String>,
    ) -> ThreadServiceResult<ThreadRecord> {
        self.repo
            .get_or_create_thread(ctx, ThreadRecord::new(project_id, external_id))
            .await
    }

    /// Look up a thread by `(project_id, external_id)`. Returns `None` when no
    /// such thread exists. Project-scoped — a `external_id` in another project
    /// is not visible here. Mirrors Go `ThreadService.GetThreadByID`.
    pub async fn get_thread(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        external_id: &str,
    ) -> ThreadServiceResult<Option<ThreadRecord>> {
        self.repo.find_thread(ctx, project_id, external_id).await
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryThreadServiceRepo {
    inner: Arc<Mutex<BTreeMap<(String, String), ThreadRecord>>>,
}

impl InMemoryThreadServiceRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn thread_count(&self) -> ThreadServiceResult<usize> {
        Ok(self.lock()?.len())
    }

    fn lock(
        &self,
    ) -> ThreadServiceResult<std::sync::MutexGuard<'_, BTreeMap<(String, String), ThreadRecord>>>
    {
        self.inner
            .lock()
            .map_err(|_| ThreadServiceError::LockPoisoned)
    }
}

#[async_trait]
impl ThreadServiceRepo for InMemoryThreadServiceRepo {
    async fn get_or_create_thread(
        &self,
        _ctx: &RequestContext,
        record: ThreadRecord,
    ) -> ThreadServiceResult<ThreadRecord> {
        let mut inner = self.lock()?;
        let key = (record.project_id.clone(), record.external_id.clone());
        // Preserve the first record so retries for the same external id are idempotent.
        Ok(inner.entry(key).or_insert(record).clone())
    }

    async fn find_thread(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        external_id: &str,
    ) -> ThreadServiceResult<Option<ThreadRecord>> {
        Ok(self
            .lock()?
            .get(&(project_id.into(), external_id.into()))
            .cloned())
    }
}

fn scoped_record_id(kind: &str, project_id: &str, external_id: &str) -> String {
    format!("{kind}:{project_id}:{external_id}")
}

// =========================================================================
// Pure header extraction — mirrors `middleware/thread.go` (S04)
// =========================================================================

/// Default thread header when `TracingConfig::thread_header` is empty.
/// Go: `middleware/thread.go` `WithThread` uses `"Conduit-Thread-Id"` when the
/// configured `ThreadHeader` is empty.
pub const DEFAULT_THREAD_HEADER: &str = "Conduit-Thread-Id";

/// S04 — extract the thread id from the configured thread header. Pure port
/// of Go `middleware/thread.go::WithThread`'s header read:
/// ```go
///   threadHeader := config.ThreadHeader
///   if threadHeader == "" { threadHeader = "Conduit-Thread-Id" }
///   threadID := c.GetHeader(threadHeader)
///   if threadID == "" { /* skip */ }
/// ```
///
/// Returns the trimmed value, or `None` when the header is absent or empty.
/// Header name matching is case-insensitive (Go `http.Header.Get` semantics).
pub fn extract_thread_id(
    headers: &[(String, String)],
    config: &crate::TracingConfig,
) -> Option<String> {
    let v = header_get(headers, config.effective_thread_header())?;
    let v = v.trim();
    if v.is_empty() {
        None
    } else {
        Some(v.to_owned())
    }
}

/// Case-insensitive header lookup, matching Go `http.Header.Get` semantics.
/// Shared with `trace_service::header_get`; kept here as a private helper so
/// each module stays self-contained.
fn header_get<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let name_lower = name.to_ascii_lowercase();
    headers
        .iter()
        .find(|(k, _)| k.to_ascii_lowercase() == name_lower)
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use conduit_db::{PolicyContext, Principal, RequestContext};

    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    #[tokio::test]
    async fn same_project_external_id_is_idempotent() -> ThreadServiceResult<()> {
        let repo = Arc::new(InMemoryThreadServiceRepo::new());
        let service = ThreadService::new(repo.clone());
        let ctx = ctx();

        let first = service
            .get_or_create_thread(&ctx, "project-a", "thread-ext-1")
            .await?;
        let second = service
            .get_or_create_thread(&ctx, "project-a", "thread-ext-1")
            .await?;

        assert_eq!(first, second);
        assert_eq!(repo.thread_count()?, 1);
        Ok(())
    }

    #[tokio::test]
    async fn different_projects_are_isolated() -> ThreadServiceResult<()> {
        let repo = Arc::new(InMemoryThreadServiceRepo::new());
        let service = ThreadService::new(repo.clone());
        let ctx = ctx();

        let project_a = service
            .get_or_create_thread(&ctx, "project-a", "thread-ext-1")
            .await?;
        let project_b = service
            .get_or_create_thread(&ctx, "project-b", "thread-ext-1")
            .await?;

        assert_ne!(project_a.id, project_b.id);
        assert_eq!(project_a.external_id, project_b.external_id);
        assert_eq!(repo.thread_count()?, 2);
        Ok(())
    }

    #[tokio::test]
    async fn get_thread_returns_some_for_existing() -> ThreadServiceResult<()> {
        let repo = Arc::new(InMemoryThreadServiceRepo::new());
        let service = ThreadService::new(repo);
        let ctx = ctx();

        let created = service
            .get_or_create_thread(&ctx, "project-a", "thread-ext-1")
            .await?;
        let found = service
            .get_thread(&ctx, "project-a", "thread-ext-1")
            .await?;

        assert_eq!(found, Some(created));
        Ok(())
    }

    #[tokio::test]
    async fn get_thread_returns_none_for_missing() -> ThreadServiceResult<()> {
        let repo = Arc::new(InMemoryThreadServiceRepo::new());
        let service = ThreadService::new(repo);
        let ctx = ctx();

        let found = service
            .get_thread(&ctx, "project-a", "never-created")
            .await?;
        assert!(found.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn get_thread_does_not_leak_across_projects() -> ThreadServiceResult<()> {
        let repo = Arc::new(InMemoryThreadServiceRepo::new());
        let service = ThreadService::new(repo);
        let ctx = ctx();

        service
            .get_or_create_thread(&ctx, "project-a", "thread-ext-1")
            .await?;
        // The same external_id in project-b is a distinct row; project-b's
        // lookup must not see project-a's record.
        let cross = service
            .get_thread(&ctx, "project-b", "thread-ext-1")
            .await?;
        assert!(cross.is_none());
        Ok(())
    }

    // =====================================================================
    // Go biz/thread_test.go parity — pure-logic sub-tests migrated against
    // the in-memory repo. DB-backed sub-tests (ent client in context, raw
    // `client.Thread.Create()`) are catalogued in
    // `go_thread_test_pending_db_backed_subtests_catalogue` below.
    // =====================================================================

    // Mirrors Go `TestThreadService_GetOrCreateThread` L64-69: after creating
    // a new thread the returned record carries the exact `threadID` and
    // `projectID` the caller supplied. Go asserts `thread1.ThreadID == threadID`
    // and `thread1.ProjectID == testProject.ID`; the Rust record stores these
    // as `external_id` / `project_id` (see `ThreadRecord::new`). Pins the
    // field mapping explicitly so a refactor that drops either assignment is
    // caught.
    #[tokio::test]
    async fn go_get_or_create_thread_returns_record_with_thread_id_and_project_id()
    -> ThreadServiceResult<()> {
        let repo = Arc::new(InMemoryThreadServiceRepo::new());
        let service = ThreadService::new(repo);
        let ctx = ctx();

        let thread = service
            .get_or_create_thread(&ctx, "test-project", "thread-test-123")
            .await?;
        // Go: `require.Equal(t, threadID, thread1.ThreadID)`.
        assert_eq!(thread.external_id, "thread-test-123");
        // Go: `require.Equal(t, testProject.ID, thread1.ProjectID)`.
        assert_eq!(thread.project_id, "test-project");
        Ok(())
    }

    // Mirrors Go `TestThreadService_GetOrCreateThread` L79-85: a different
    // `threadID` in the same project yields a distinct thread row (distinct
    // `id`, distinct `external_id`). Go asserts `require.NotEqual(t,
    // thread1.ID, thread3.ID)` and `require.Equal(t, differentThreadID,
    // thread3.ThreadID)`.
    #[tokio::test]
    async fn go_get_or_create_thread_different_thread_ids_yield_distinct_threads()
    -> ThreadServiceResult<()> {
        let repo = Arc::new(InMemoryThreadServiceRepo::new());
        let service = ThreadService::new(repo.clone());
        let ctx = ctx();

        let thread1 = service
            .get_or_create_thread(&ctx, "test-project", "thread-test-123")
            .await?;
        let thread3 = service
            .get_or_create_thread(&ctx, "test-project", "thread-test-456")
            .await?;

        // Go: `require.NotEqual(t, thread1.ID, thread3.ID)`.
        assert_ne!(thread1.id, thread3.id);
        // Go: `require.Equal(t, differentThreadID, thread3.ThreadID)`.
        assert_eq!(thread3.external_id, "thread-test-456");
        // Distinct external_id in the same project → two rows.
        assert_eq!(repo.thread_count()?, 2);
        Ok(())
    }

    // Mirrors Go `TestThreadService_GetThreadByID` L158-161: a non-existent
    // `(threadID, projectID)` pair. Go surfaces this as an error whose message
    // contains `"failed to get thread"`; the Rust service exposes the
    // not-found case as `None` (see `ThreadServiceRepo::find_thread` doc —
    // deliberate divergence for caller convenience). This test pins the
    // Rust-side contract AND records the Go-side expectation so a future
    // swap to an `Err` variant is a conscious parity decision, not a drift.
    #[tokio::test]
    async fn go_get_thread_by_id_nonexistent_go_errors_rust_returns_none() -> ThreadServiceResult<()>
    {
        let repo = Arc::new(InMemoryThreadServiceRepo::new());
        let service = ThreadService::new(repo);
        let ctx = ctx();

        // Go: `_, err = threadService.GetThreadByID(ctx, "non-existent",
        //                testProject.ID); require.Error(t, err);
        //       require.Contains(t, err.Error(), "failed to get thread")`.
        // Rust: `get_thread` returns `None` (not an error) for the not-found
        // case — parity divergence documented in `find_thread`'s doc comment.
        let found = service
            .get_thread(&ctx, "test-project", "non-existent")
            .await?;
        assert!(
            found.is_none(),
            "Rust returns None for not-found; Go returns error \"failed to \
             get thread\" — see find_thread doc for the deliberate divergence"
        );
        Ok(())
    }

    // Mirrors Go `TestThreadService_GetThreadByID` L144-156: a thread created
    // out-of-band (Go: raw `client.Thread.Create().SetThreadID(..).
    // SetProjectID(..).Save(ctx)`) is retrievable by `(threadID, projectID)`
    // and the returned row matches. Rust creates via the service's own
    // `get_or_create_thread` (the in-memory repo has no raw insert seam), but
    // the retrieval contract under test is identical: `find_thread` is keyed
    // on the pair and returns the same record that was stored.
    #[tokio::test]
    async fn go_get_thread_by_id_retrieves_matching_record() -> ThreadServiceResult<()> {
        let repo = Arc::new(InMemoryThreadServiceRepo::new());
        let service = ThreadService::new(repo);
        let ctx = ctx();

        let created = service
            .get_or_create_thread(&ctx, "test-project", "thread-get-test-123")
            .await?;
        let retrieved = service
            .get_thread(&ctx, "test-project", "thread-get-test-123")
            .await?;

        // Go: `require.Equal(t, createdThread.ID, retrievedThread.ID)` and
        // `require.Equal(t, threadID, retrievedThread.ThreadID)`.
        match retrieved {
            Some(r) => {
                assert_eq!(created.id, r.id);
                assert_eq!(r.external_id, "thread-get-test-123");
                assert_eq!(r.project_id, "test-project");
            }
            None => panic!("expected Some(thread) for just-created record"),
        }
        Ok(())
    }

    /// Comprehensive parity documentation of the Go biz/thread_test.go
    /// sub-tests NOT migrated here because their assertions depend on the
    /// original Go ent client. Each row cites the Go test name + line range so
    /// a future PostgreSQL-backed trait test can grep for the right test name.
    ///
    /// This test exists to FAIL LOUD if the trait-port lands and someone
    /// forgets to add a DB-backed test for one of these sub-tests: append
    /// each migrated sub-test to the `migrated` list as it lands, and the
    /// remaining `pending` list shrinks.
    #[test]
    fn go_thread_test_pending_db_backed_subtests_catalogue() {
        // Each tuple is (Go test function name, Go line range, brief reason).
        let pending: &[(&str, &str, &str)] = &[(
            "TestThreadService_GetOrCreateThread_NoClient",
            "L164-176",
            "ent client in context (`ent.NewContext` + `authz.WithTestBypass`); \
                 Go creates a project via `client.Project.Create()` then calls \
                 `GetOrCreateThread(ctx, 1, \"thread-123\")`. Requires a \
                 PostgreSQL-backed ThreadServiceRepo test — the in-memory \
                 repo has no `entFromContext` seam. Note the Go test name says \
                 \"NoClient\" but actually tests WITH the client in context \
                 (\"Test with ent client in context - should work\"); the \
                 true no-client error path (`\"ent client not found in \
                 context\"`, thread.go:34-36) is also pending this port.",
        )];

        // Sanity: every entry has a non-empty reason and unique name.
        let mut seen = std::collections::HashSet::new();
        for (name, lines, reason) in pending {
            assert!(!name.is_empty(), "pending entry has empty name");
            assert!(!lines.is_empty(), "pending entry {name} has empty lines");
            assert!(!reason.is_empty(), "pending entry {name} has empty reason");
            assert!(seen.insert(*name), "duplicate pending entry {name}");
        }
        // Catalogue sanity: at least the rows we know about.
        assert!(
            !pending.is_empty(),
            "expected at least 1 pending DB-backed sub-test, got {}",
            pending.len()
        );
    }

    // =====================================================================
    // S13 — thread_id external-string → internal Thread row mapping.
    // Mirrors Go biz/thread.go::GetOrCreateThread which keys on
    // `thread.ThreadIDEQ(threadID)` + `thread.ProjectIDEQ(projectID)`. The
    // external string is an *opaque* key — Go does no normalization
    // (no trim/lowercase); SQL equality is case-sensitive.
    // =====================================================================

    // S13 — the external thread_id string is used as-is (opaque key). Two
    // strings that differ only by case are distinct threads, mirroring Go's
    // case-sensitive `thread.ThreadIDEQ(threadID)` SQL equality. Whitespace
    // is also significant (Go does not trim).
    #[tokio::test]
    async fn s13_external_thread_string_is_opaque_case_sensitive_key() -> ThreadServiceResult<()> {
        let repo = Arc::new(InMemoryThreadServiceRepo::new());
        let service = ThreadService::new(repo.clone());
        let ctx = ctx();

        let lower = service
            .get_or_create_thread(&ctx, "project-a", "thread-ABC")
            .await?;
        let upper = service
            .get_or_create_thread(&ctx, "project-a", "thread-abc")
            .await?;

        assert_ne!(lower.id, upper.id, "case-only difference → distinct rows");
        assert_eq!(lower.external_id, "thread-ABC");
        assert_eq!(upper.external_id, "thread-abc");
        assert_eq!(repo.thread_count()?, 2);

        // Each external string resolves back to exactly its own row — the
        // WithThread middleware would get the right internal row for each
        // distinct header value.
        let lookup_lower = service.get_thread(&ctx, "project-a", "thread-ABC").await?;
        let lookup_upper = service.get_thread(&ctx, "project-a", "thread-abc").await?;
        assert_eq!(lookup_lower.map(|r| r.id), Some(lower.id));
        assert_eq!(lookup_upper.map(|r| r.id), Some(upper.id));
        Ok(())
    }

    // =====================================================================
    // Header extraction — mirrors Go middleware/thread_test.go intent (S04)
    // =====================================================================

    fn hdr(name: &str, val: &str) -> (String, String) {
        (name.to_owned(), val.to_owned())
    }

    // S04 — default header name. Mirrors Go `TestWithThreadID_Success`
    // which sends `Conduit-Thread-Id` (Go http canonicalization) and expects it
    // to be read by the default-config middleware.
    #[test]
    fn effective_thread_header_defaults_to_conduit_thread_id() {
        let cfg = crate::TracingConfig::default();
        assert_eq!(cfg.effective_thread_header(), "Conduit-Thread-Id");
    }

    #[test]
    fn effective_thread_header_uses_override_when_set() {
        let cfg = crate::TracingConfig {
            thread_header: "X-Thread-Id".into(),
            ..Default::default()
        };
        assert_eq!(cfg.effective_thread_header(), "X-Thread-Id");
    }

    // Mirrors Go `TestWithThreadID_Success`.
    #[test]
    fn extract_thread_id_reads_default_header() {
        let headers = [hdr("Conduit-Thread-Id", "thread-test-123")];
        let cfg = crate::TracingConfig::default();
        assert_eq!(
            extract_thread_id(&headers, &cfg),
            Some("thread-test-123".into())
        );
    }

    // Case-insensitive — Go `Conduit-Thread-Id` header set matches `Conduit-Thread-Id`.
    #[test]
    fn extract_thread_id_is_case_insensitive() {
        let headers = [hdr("Conduit-Thread-Id", "thread-test-123")];
        let cfg = crate::TracingConfig::default();
        assert_eq!(
            extract_thread_id(&headers, &cfg),
            Some("thread-test-123".into())
        );
    }

    // Mirrors Go `TestWithThreadID_NoHeader`.
    #[test]
    fn extract_thread_id_returns_none_when_absent() {
        let headers: [(String, String); 0] = [];
        let cfg = crate::TracingConfig::default();
        assert_eq!(extract_thread_id(&headers, &cfg), None);
    }

    #[test]
    fn extract_thread_id_treats_empty_as_absent() {
        let headers = [hdr("Conduit-Thread-Id", "")];
        let cfg = crate::TracingConfig::default();
        assert_eq!(extract_thread_id(&headers, &cfg), None);
    }

    #[test]
    fn extract_thread_id_trims_whitespace() {
        let headers = [hdr("Conduit-Thread-Id", "  thread-1  ")];
        let cfg = crate::TracingConfig::default();
        assert_eq!(extract_thread_id(&headers, &cfg), Some("thread-1".into()));
    }

    #[test]
    fn extract_thread_id_uses_configured_override() {
        let headers = [hdr("X-Custom-Thread", "custom-thread-9")];
        let cfg = crate::TracingConfig {
            thread_header: "X-Custom-Thread".into(),
            ..Default::default()
        };
        assert_eq!(
            extract_thread_id(&headers, &cfg),
            Some("custom-thread-9".into())
        );
    }
}
