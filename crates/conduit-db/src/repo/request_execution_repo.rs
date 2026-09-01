//! Request-execution repository — Rust port of the execution half of
//! `conduit/internal/server/biz/request.go`.
//!
//! Mirrors the Go `RequestService` execution data-access surface against the
//! `RequestExecution` Ent schema
//! (`conduit/internal/ent/schema/request_execution.go`): create, get-by-id,
//! update (status transition + response body / metrics / error fields), and
//! list-by-request ordered by `created_at` (mirrors the
//! `request_executions_by_request_id_created_at` index).
//!
//! ## No soft delete
//! The Go `RequestExecution` schema's `Mixin()` returns only `TimeMixin{}` —
//! there is **no `SoftDeleteMixin`**, hence no `deleted_at` column and no
//! `*_with_deleted` / restore surface here. All rows are live.
//!
//! ## Status enum (mirrors Go `requestexecution.Status`)
//! Go declares the enum values `pending`, `processing`, `completed`, `failed`,
//! `canceled`. The Go `CreateRequestExecution` (request.go:330) hard-codes the
//! initial status to `processing`, so `CreateRequestExecutionInput` has no
//! `status` field and `row_from_input` writes `STATUS_PROCESSING`.
//!
//! Legal transitions mirror the Go `request.Status` CAS path used by
//! `UpdateRequestExecutionStatus` (request.go:760): the trait does not enforce
//! a transition table at the repo layer (Go's `UpdateOneID(...).Save()` does
//! not gate on `expected_status` for executions — it is a direct write), so
//! `update_request_execution` accepts any next status the caller supplies.
//!
//! ## Storage model
//! `RequestExecutionRow` is a hand-written typed struct since RUST-P3-002 S13
//! batch 3 — every Go Ent column is a first-class field (see `row/mod.rs` for
//! the Go schema mapping). The former `extra`-map keys are gone.
//!
//! ## Stale-processing cleanup
//! `reclaim_stale_processing` mirrors Go `ClearStaleProcessingOnStartup` /
//! `cancelStaleRecords` (request.go:1064-1116) for executions: it finds rows
//! stuck in `processing` with `created_at` older than the supplied cutoff and
//! transitions them to `canceled`. The Go cutoff is `maxProcessingDuration =
//! 1 * time.Hour` (request.go:1089); the repo leaves the cutoff to the caller
//! so the same method serves startup cleanup and ad-hoc sweeps.

use crate::policy::ProjectAccess;
use crate::repo::{
    RepoError, RepoResult, RequestContext, guard_project_access, guard_repo_principal,
};
use crate::row::RequestExecutionRow;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

// Status values — mirror Go's `requestexecution.Status` enum (identical set to
// `request.Status`).
pub const STATUS_PENDING: &str = "pending";
pub const STATUS_PROCESSING: &str = "processing";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_CANCELED: &str = "canceled";

/// Fields a caller sets when creating a request execution. Mirrors the Go
/// `RequestExecution.Create()` builder inputs from
/// `RequestService.CreateRequestExecution` (request.go:323-344) minus the
/// server-assigned id/timestamps and the status (always `processing`).
///
/// `project_id` is taken from the parent request (request.go:326), `stream`
/// from the parent request (request.go:331), and `format` from the LLM API
/// format (request.go:324). `request_url` is optional (request.go:335).
#[derive(Debug, Clone)]
pub struct CreateRequestExecutionInput {
    pub id: String,
    /// Parent request's project (Go `SetProjectID(request.ProjectID)`).
    pub project_id: String,
    /// Parent request id (Go `SetRequestID(request.ID)`).
    pub request_id: String,
    /// Channel id; optional because the channel may be deleted
    /// (Go `field.Int("channel_id").Optional()`, schema:41).
    pub channel_id: Option<String>,
    /// One-way fingerprint of the credential selected for this attempt.
    pub credential_identity: Option<String>,
    pub data_storage_id: Option<String>,
    pub model_id: String,
    /// Go enum string, e.g. `openai/chat_completions`, `claude/messages`,
    /// `openai/response`. Default `openai/chat_completions` (schema:52).
    pub format: String,
    /// Masked, marshalled headers (Go `SetRequestHeaders(requestHeadersBytes)`).
    pub request_headers: Option<Value>,
    /// Provider-shaped request body (Go `SetRequestBody(requestBodyForDB)`).
    pub request_body: Value,
    /// Mirrors Go `request.Stream` (schema:74, request.go:331).
    pub stream: bool,
    /// Actual upstream URL (Go `SetRequestURL(channelRequest.URL)`, optional).
    pub request_url: Option<String>,
    /// Whether pass-through substitution was active (Go `SetPassThroughApplied`,
    /// request.go:333).
    pub pass_through_applied: bool,
    pub created_at: String,
}

/// Patch applied by `update_request_execution`. Every field is optional; `None`
/// means "leave unchanged". Mirrors the union of Go's
/// `UpdateRequestExecutionCompleted` (request.go:686-723) and
/// `UpdateRequestExecutionStatus` (request.go:769-777): status, external_id,
/// response body, response chunks, error message, response status code, and
/// the three latency metrics.
#[derive(Debug, Clone, Default)]
pub struct UpdateRequestExecutionInput {
    pub status: Option<String>,
    pub external_id: Option<String>,
    pub response_body: Option<Value>,
    pub response_chunks: Option<Vec<Value>>,
    pub error_message: Option<String>,
    pub response_status_code: Option<i64>,
    pub metrics_latency_ms: Option<i64>,
    pub metrics_first_token_latency_ms: Option<i64>,
    pub metrics_reasoning_duration_ms: Option<i64>,
    pub request_url: Option<String>,
    /// New `updated_at` value (callers always set this on the Go update path).
    pub updated_at: String,
}

// --- timestamp parsing -------------------------------------------------------

/// Parse an ISO-8601 / RFC-3339 / date-only string into `DateTime<Utc>`.
/// Falls back to the Unix epoch on failure so comparisons don't panic —
/// well-formed inputs are unaffected. Same recipe as `usage_repo.rs`.
fn parse_dt(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .map(|n| chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(n, chrono::Utc))
        })
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map(|d| {
                chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                    d.and_hms_opt(0, 0, 0).unwrap_or_default(),
                    chrono::Utc,
                )
            })
        })
        .unwrap_or_else(|_| chrono::DateTime::from_timestamp(0, 0).unwrap_or_default())
}

// --- row helpers -----------------------------------------------------------

/// Build a `RequestExecutionRow` from creation input. Mirrors Go
/// `CreateRequestExecution`'s `mut.Save` (request.go:344) — status is hard-coded
/// to `processing`, response/error fields are absent until the first update.
/// Both timestamps start at `created_at` (Go `TimeMixin` defaults both to the
/// creation instant).
fn row_from_input(input: &CreateRequestExecutionInput) -> RequestExecutionRow {
    let now = parse_dt(&input.created_at);
    RequestExecutionRow {
        id: input.id.clone(),
        project_id: input.project_id.clone(),
        request_id: input.request_id.clone(),
        channel_id: input.channel_id.clone(),
        credential_identity: input.credential_identity.clone(),
        data_storage_id: input.data_storage_id.clone(),
        external_id: None,
        model_id: input.model_id.clone(),
        format: input.format.clone(),
        request_body: input.request_body.clone(),
        response_body: None,
        response_chunks: None,
        error_message: None,
        response_status_code: None,
        // Go hard-codes processing on create (request.go:330).
        status: STATUS_PROCESSING.into(),
        stream: input.stream,
        metrics_latency_ms: None,
        metrics_first_token_latency_ms: None,
        metrics_reasoning_duration_ms: None,
        request_headers: input.request_headers.clone(),
        request_url: input.request_url.clone(),
        pass_through_applied: input.pass_through_applied,
        created_at: now,
        updated_at: now,
    }
}

// --- trait -----------------------------------------------------------------

/// Repository surface for the `request_executions` table.
///
/// Method naming convention (consistent with `RequestRepo`):
/// - `find_request_execution_*` / `list_*` — default scope.
/// - `*_unchecked` — skips the policy guard; callers must have already enforced
///   authorization. Public methods wrap these with the appropriate guard.
///
/// **No soft-delete variants**: the Go `RequestExecution` schema has no
/// `deleted_at`.
#[async_trait]
pub trait RequestExecutionRepo: Send + Sync {
    // --- create ---
    async fn create_request_execution_unchecked(
        &self,
        ctx: &RequestContext,
        row: RequestExecutionRow,
    ) -> RepoResult<RequestExecutionRow>;

    async fn create_request_execution(
        &self,
        ctx: &RequestContext,
        input: CreateRequestExecutionInput,
    ) -> RepoResult<RequestExecutionRow> {
        guard_project_access(ctx, &input.project_id, ProjectAccess::Write)?;
        let row = row_from_input(&input);
        self.create_request_execution_unchecked(ctx, row).await
    }

    // --- read: by id ---
    async fn find_request_execution_by_id_unchecked(
        &self,
        ctx: &RequestContext,
        execution_id: &str,
    ) -> RepoResult<Option<RequestExecutionRow>>;

    async fn find_request_execution_by_id(
        &self,
        ctx: &RequestContext,
        execution_id: &str,
    ) -> RepoResult<Option<RequestExecutionRow>> {
        guard_repo_principal(ctx)?;
        self.find_request_execution_by_id_unchecked(ctx, execution_id)
            .await
    }

    // --- read: list by request (created_at ASC, id ASC tie-breaker) ---
    async fn list_request_executions_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RepoResult<Vec<RequestExecutionRow>>;

    async fn list_all_request_executions_unchecked(
        &self,
        ctx: &RequestContext,
        limit: u32,
    ) -> RepoResult<Vec<RequestExecutionRow>>;

    /// List every execution attached to `channel_id`, optionally constrained
    /// to the project already authorized by the caller.
    async fn list_request_executions_by_channel_unchecked(
        &self,
        ctx: &RequestContext,
        authorized_project_id: Option<&str>,
        channel_id: &str,
    ) -> RepoResult<Vec<RequestExecutionRow>>;

    /// List every execution attached to `data_storage_id`, optionally
    /// constrained to the project already authorized by the caller.
    async fn list_request_executions_by_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
        authorized_project_id: Option<&str>,
        data_storage_id: &str,
    ) -> RepoResult<Vec<RequestExecutionRow>>;

    async fn list_request_executions(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RepoResult<Vec<RequestExecutionRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.list_request_executions_unchecked(ctx, project_id, request_id)
            .await
    }

    // --- update ---
    /// Direct status + field update. Mirrors Go's `UpdateOneID(...).Save()`
    /// (request.go:769): no CAS on `expected_status`. Returns the updated row,
    /// or `NotFound` when no row matches `(project_id, execution_id)`.
    async fn update_request_execution_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        execution_id: &str,
        input: UpdateRequestExecutionInput,
    ) -> RepoResult<RequestExecutionRow>;

    async fn update_request_execution(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        execution_id: &str,
        input: UpdateRequestExecutionInput,
    ) -> RepoResult<RequestExecutionRow> {
        guard_project_access(ctx, project_id, ProjectAccess::Write)?;
        self.update_request_execution_unchecked(ctx, project_id, execution_id, input)
            .await
    }

    // --- stale-processing cleanup ---
    /// Reclaim executions stuck in `processing` older than `cutoff_created_at`.
    /// Mirrors Go `cancelStaleRecords` for executions (request.go:1106-1113):
    /// `WHERE status = processing AND created_at < cutoff -> SET status =
    /// canceled`. Returns the ids of the rows reclaimed.
    ///
    /// The Go `ClearStaleProcessingOnStartup` (request.go:1091) runs under a
    /// system-bypass context; the caller is responsible for supplying an
    /// appropriately-authorized `RequestContext` (this method is guarded by
    /// `guard_repo_principal` only, mirroring the system-bypass startup path).
    async fn reclaim_stale_processing_unchecked(
        &self,
        ctx: &RequestContext,
        cutoff_created_at: &str,
        now: &str,
    ) -> RepoResult<Vec<String>>;

    async fn reclaim_stale_processing(
        &self,
        ctx: &RequestContext,
        cutoff_created_at: &str,
        now: &str,
    ) -> RepoResult<Vec<String>> {
        guard_repo_principal(ctx)?;
        self.reclaim_stale_processing_unchecked(ctx, cutoff_created_at, now)
            .await
    }
}

// --- in-memory implementation ----------------------------------------------

/// In-memory `RequestExecutionRepo` for unit tests and the bootstrap runtime
/// before sqlx lands. Storage is keyed by `id`. No soft-delete state (matches
/// the Go `RequestExecution` schema).
#[derive(Debug, Default)]
pub struct InMemoryRequestExecutionRepo {
    rows: Mutex<BTreeMap<String, RequestExecutionRow>>,
}

impl InMemoryRequestExecutionRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = RequestExecutionRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request execution repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl RequestExecutionRepo for InMemoryRequestExecutionRepo {
    async fn create_request_execution_unchecked(
        &self,
        _ctx: &RequestContext,
        row: RequestExecutionRow,
    ) -> RepoResult<RequestExecutionRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request execution repo"))?;
        if guard.contains_key(&row.id) {
            return Err(RepoError::NotFound("execution id already present"));
        }
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_request_execution_by_id_unchecked(
        &self,
        _ctx: &RequestContext,
        execution_id: &str,
    ) -> RepoResult<Option<RequestExecutionRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request execution repo"))?
            .get(execution_id)
            .cloned())
    }

    async fn list_request_executions_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
    ) -> RepoResult<Vec<RequestExecutionRow>> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request execution repo"))?;
        let mut matched: Vec<RequestExecutionRow> = guard
            .values()
            .filter(|r| r.project_id == project_id && r.request_id == request_id)
            .cloned()
            .collect();
        // created_at ASC with a stable id ASC tie-breaker (mirrors
        // request_executions_by_request_id_created_at).
        matched.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(matched)
    }

    async fn list_all_request_executions_unchecked(
        &self,
        _ctx: &RequestContext,
        limit: u32,
    ) -> RepoResult<Vec<RequestExecutionRow>> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request execution repo"))?;
        let mut rows = guard.values().cloned().collect::<Vec<_>>();
        rows.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        rows.truncate(limit as usize);
        Ok(rows)
    }

    async fn list_request_executions_by_channel_unchecked(
        &self,
        _ctx: &RequestContext,
        authorized_project_id: Option<&str>,
        channel_id: &str,
    ) -> RepoResult<Vec<RequestExecutionRow>> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request execution repo"))?;
        let mut matched = guard
            .values()
            .filter(|row| {
                row.channel_id.as_deref() == Some(channel_id)
                    && authorized_project_id.is_none_or(|project_id| row.project_id == project_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        matched.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(matched)
    }

    async fn list_request_executions_by_data_storage_unchecked(
        &self,
        _ctx: &RequestContext,
        authorized_project_id: Option<&str>,
        data_storage_id: &str,
    ) -> RepoResult<Vec<RequestExecutionRow>> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request execution repo"))?;
        let mut matched = guard
            .values()
            .filter(|row| {
                row.data_storage_id.as_deref() == Some(data_storage_id)
                    && authorized_project_id.is_none_or(|project_id| row.project_id == project_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        matched.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(matched)
    }

    async fn update_request_execution_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        execution_id: &str,
        input: UpdateRequestExecutionInput,
    ) -> RepoResult<RequestExecutionRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request execution repo"))?;
        let row = guard
            .get_mut(execution_id)
            .filter(|r| r.project_id == project_id)
            .ok_or(RepoError::NotFound("request execution"))?;

        if let Some(v) = input.status {
            row.status = v;
        }
        if let Some(v) = input.external_id {
            row.external_id = Some(v);
        }
        if let Some(v) = input.response_body {
            row.response_body = Some(v);
        }
        if let Some(v) = input.response_chunks {
            // Typed row stores the chunk list as one JSON array (Go
            // `[]objects.JSONRawMessage`).
            row.response_chunks = Some(Value::from(v));
        }
        if let Some(v) = input.error_message {
            row.error_message = Some(v);
        }
        if let Some(v) = input.response_status_code {
            row.response_status_code = Some(v);
        }
        if let Some(v) = input.metrics_latency_ms {
            row.metrics_latency_ms = Some(v);
        }
        if let Some(v) = input.metrics_first_token_latency_ms {
            row.metrics_first_token_latency_ms = Some(v);
        }
        if let Some(v) = input.metrics_reasoning_duration_ms {
            row.metrics_reasoning_duration_ms = Some(v);
        }
        if let Some(v) = input.request_url {
            row.request_url = Some(v);
        }
        row.updated_at = parse_dt(&input.updated_at);
        Ok(row.clone())
    }

    async fn reclaim_stale_processing_unchecked(
        &self,
        _ctx: &RequestContext,
        cutoff_created_at: &str,
        now: &str,
    ) -> RepoResult<Vec<String>> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request execution repo"))?;
        let cutoff = parse_dt(cutoff_created_at);
        let now_dt = parse_dt(now);
        let mut reclaimed = Vec::new();
        for row in guard.values_mut() {
            if row.status == STATUS_PROCESSING && row.created_at < cutoff {
                row.status = STATUS_CANCELED.into();
                row.updated_at = now_dt;
                reclaimed.push(row.id.clone());
            }
        }
        Ok(reclaimed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use crate::repo::RequestContext;
    use serde_json::json;

    fn ctx_allowed() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn ctx_anon() -> RequestContext {
        RequestContext::new(PolicyContext::anonymous())
    }

    fn input(
        id: &str,
        project: &str,
        request_id: &str,
        created_at: &str,
    ) -> CreateRequestExecutionInput {
        CreateRequestExecutionInput {
            id: id.into(),
            project_id: project.into(),
            request_id: request_id.into(),
            channel_id: Some("ch-1".into()),
            credential_identity: Some("sha256:test-credential".into()),
            data_storage_id: None,
            model_id: "claude-3-5-sonnet".into(),
            format: "claude/messages".into(),
            request_headers: Some(json!({"x-api-key": "***"})),
            request_body: json!({"model": "claude-3-5-sonnet", "messages": []}),
            stream: false,
            request_url: Some("https://api.anthropic.com/v1/messages".into()),
            pass_through_applied: false,
            created_at: created_at.into(),
        }
    }

    #[tokio::test]
    async fn create_hard_codes_processing_status() -> RepoResult<()> {
        // Mirrors Go CreateRequestExecution SetStatus(processing) (request.go:330).
        let repo = InMemoryRequestExecutionRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_request_execution(&ctx, input("e-1", "p-1", "r-1", "2024-01-01T00:00:00Z"))
            .await?;
        assert_eq!(created.id, "e-1");
        assert_eq!(created.status, STATUS_PROCESSING);
        assert_eq!(created.project_id, "p-1");
        assert_eq!(created.request_id, "r-1");
        assert_eq!(created.format, "claude/messages");
        assert_eq!(
            created.request_url.as_deref(),
            Some("https://api.anthropic.com/v1/messages")
        );
        // Response/error fields are absent until the first update.
        assert_eq!(created.response_body, None);
        assert_eq!(created.external_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn create_duplicate_id_is_rejected() -> RepoResult<()> {
        let repo = InMemoryRequestExecutionRepo::new();
        let ctx = ctx_allowed();
        repo.create_request_execution(&ctx, input("e-1", "p-1", "r-1", "t1"))
            .await?;

        let dup = repo
            .create_request_execution(&ctx, input("e-1", "p-1", "r-1", "t1"))
            .await;
        assert!(matches!(dup, Err(RepoError::NotFound(_))));
        Ok(())
    }

    #[tokio::test]
    async fn find_by_id_round_trip() -> RepoResult<()> {
        let repo = InMemoryRequestExecutionRepo::new();
        let ctx = ctx_allowed();
        repo.create_request_execution(&ctx, input("e-1", "p-1", "r-1", "t1"))
            .await?;

        let found = repo
            .find_request_execution_by_id(&ctx, "e-1")
            .await?
            .ok_or(RepoError::NotFound("e-1"))?;
        assert_eq!(found.id, "e-1");

        assert!(
            repo.find_request_execution_by_id(&ctx, "missing")
                .await?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_by_request_orders_by_created_at_then_id() -> RepoResult<()> {
        let repo = InMemoryRequestExecutionRepo::new();
        let ctx = ctx_allowed();
        // Insert out of order; e-3 shares a timestamp with e-2 to exercise the
        // id ASC tie-breaker.
        repo.create_request_execution(&ctx, input("e-3", "p-1", "r-1", "2024-01-02T00:00:00Z"))
            .await?;
        repo.create_request_execution(&ctx, input("e-1", "p-1", "r-1", "2024-01-01T00:00:00Z"))
            .await?;
        repo.create_request_execution(&ctx, input("e-2", "p-1", "r-1", "2024-01-02T00:00:00Z"))
            .await?;
        // Different request — must be excluded.
        repo.create_request_execution(&ctx, input("e-x", "p-1", "r-2", "2024-01-01T00:00:00Z"))
            .await?;
        // Different project — must be excluded.
        repo.create_request_execution(&ctx, input("e-y", "p-2", "r-1", "2024-01-01T00:00:00Z"))
            .await?;

        let rows = repo.list_request_executions(&ctx, "p-1", "r-1").await?;
        let ids: Vec<_> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["e-1", "e-2", "e-3"]);
        Ok(())
    }

    #[tokio::test]
    async fn list_by_channel_honors_optional_authorized_project() -> RepoResult<()> {
        let repo = InMemoryRequestExecutionRepo::new();
        let ctx = ctx_allowed();
        for (execution_id, project_id, channel_id, created_at) in [
            ("e-3", "p-1", "ch-shared", "2024-01-02T00:00:00Z"),
            ("e-1", "p-1", "ch-shared", "2024-01-01T00:00:00Z"),
            ("e-2", "p-2", "ch-shared", "2024-01-01T00:00:00Z"),
            ("e-x", "p-1", "ch-other", "2023-12-31T00:00:00Z"),
        ] {
            let mut execution = input(execution_id, project_id, "r-1", created_at);
            execution.channel_id = Some(channel_id.to_owned());
            repo.create_request_execution(&ctx, execution).await?;
        }

        let global = repo
            .list_request_executions_by_channel_unchecked(&ctx, None, "ch-shared")
            .await?;
        assert_eq!(
            global.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["e-1", "e-2", "e-3"]
        );

        let project = repo
            .list_request_executions_by_channel_unchecked(&ctx, Some("p-1"), "ch-shared")
            .await?;
        assert_eq!(
            project
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["e-1", "e-3"]
        );
        assert!(
            repo.list_request_executions_by_channel_unchecked(&ctx, Some("p-missing"), "ch-shared")
                .await?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_by_data_storage_honors_optional_authorized_project() -> RepoResult<()> {
        let repo = InMemoryRequestExecutionRepo::new();
        let ctx = ctx_allowed();
        for (execution_id, project_id, data_storage_id, created_at) in [
            ("e-3", "p-1", "ds-shared", "2024-01-02T00:00:00Z"),
            ("e-1", "p-1", "ds-shared", "2024-01-01T00:00:00Z"),
            ("e-2", "p-2", "ds-shared", "2024-01-01T00:00:00Z"),
            ("e-x", "p-1", "ds-other", "2023-12-31T00:00:00Z"),
        ] {
            let mut execution = input(execution_id, project_id, "r-1", created_at);
            execution.data_storage_id = Some(data_storage_id.to_owned());
            repo.create_request_execution(&ctx, execution).await?;
        }

        let global = repo
            .list_request_executions_by_data_storage_unchecked(&ctx, None, "ds-shared")
            .await?;
        assert_eq!(
            global.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec!["e-1", "e-2", "e-3"]
        );

        let project = repo
            .list_request_executions_by_data_storage_unchecked(&ctx, Some("p-1"), "ds-shared")
            .await?;
        assert_eq!(
            project
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["e-1", "e-3"]
        );
        assert!(
            repo.list_request_executions_by_data_storage_unchecked(
                &ctx,
                Some("p-missing"),
                "ds-shared",
            )
            .await?
            .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_sets_status_external_response_and_metrics() -> RepoResult<()> {
        // Mirrors the Go UpdateRequestExecutionCompleted path (request.go:686).
        let repo = InMemoryRequestExecutionRepo::new();
        let ctx = ctx_allowed();
        repo.create_request_execution(&ctx, input("e-1", "p-1", "r-1", "t1"))
            .await?;

        let updated = repo
            .update_request_execution(
                &ctx,
                "p-1",
                "e-1",
                UpdateRequestExecutionInput {
                    status: Some(STATUS_COMPLETED.into()),
                    external_id: Some("ext-abc".into()),
                    response_body: Some(json!({"content": "hi"})),
                    metrics_latency_ms: Some(1234),
                    metrics_first_token_latency_ms: Some(120),
                    metrics_reasoning_duration_ms: Some(800),
                    updated_at: "2024-01-02T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.status, STATUS_COMPLETED);
        assert_eq!(updated.external_id.as_deref(), Some("ext-abc"));
        assert_eq!(updated.response_body, Some(json!({"content": "hi"})));
        assert_eq!(updated.metrics_latency_ms, Some(1234));
        assert_eq!(updated.metrics_first_token_latency_ms, Some(120));
        assert_eq!(updated.metrics_reasoning_duration_ms, Some(800));
        Ok(())
    }

    #[tokio::test]
    async fn update_wrong_project_returns_not_found() -> RepoResult<()> {
        let repo = InMemoryRequestExecutionRepo::new();
        let ctx = ctx_allowed();
        repo.create_request_execution(&ctx, input("e-1", "p-1", "r-1", "t1"))
            .await?;

        let miss = repo
            .update_request_execution(
                &ctx,
                "p-other",
                "e-1",
                UpdateRequestExecutionInput {
                    status: Some(STATUS_FAILED.into()),
                    updated_at: "t2".into(),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(miss, Err(RepoError::NotFound(_))));
        Ok(())
    }

    #[tokio::test]
    async fn reclaim_stale_processing_cancels_old_processing_rows() -> RepoResult<()> {
        // Mirrors Go ClearStaleProcessingOnStartup for executions
        // (request.go:1106-1113): only processing rows older than the cutoff
        // are canceled; completed/processing-but-fresh rows are untouched.
        let repo = InMemoryRequestExecutionRepo::new();
        let ctx = ctx_allowed();
        // Stale processing row (created before cutoff).
        repo.create_request_execution(&ctx, input("e-stale", "p-1", "r-1", "2024-01-01T00:00:00Z"))
            .await?;
        // Fresh processing row (created at/after cutoff) — must survive.
        repo.create_request_execution(&ctx, input("e-fresh", "p-1", "r-1", "2024-01-02T00:30:00Z"))
            .await?;
        // Stale completed row — terminal, must survive.
        let mut completed_input = input("e-done", "p-1", "r-1", "2024-01-01T00:00:00Z");
        completed_input.created_at = "2024-01-01T00:00:00Z".into();
        repo.create_request_execution(&ctx, completed_input).await?;
        repo.update_request_execution(
            &ctx,
            "p-1",
            "e-done",
            UpdateRequestExecutionInput {
                status: Some(STATUS_COMPLETED.into()),
                updated_at: "2024-01-01T00:05:00Z".into(),
                ..Default::default()
            },
        )
        .await?;

        // Cutoff: anything created before 2024-01-02T00:00:00Z is stale.
        let reclaimed = repo
            .reclaim_stale_processing(&ctx, "2024-01-02T00:00:00Z", "2024-01-03T00:00:00Z")
            .await?;
        assert_eq!(reclaimed, vec!["e-stale".to_string()]);

        let stale = repo
            .find_request_execution_by_id(&ctx, "e-stale")
            .await?
            .ok_or(RepoError::NotFound("e-stale"))?;
        assert_eq!(stale.status, STATUS_CANCELED);

        let fresh = repo
            .find_request_execution_by_id(&ctx, "e-fresh")
            .await?
            .ok_or(RepoError::NotFound("e-fresh"))?;
        assert_eq!(fresh.status, STATUS_PROCESSING);

        let done = repo
            .find_request_execution_by_id(&ctx, "e-done")
            .await?
            .ok_or(RepoError::NotFound("e-done"))?;
        assert_eq!(done.status, STATUS_COMPLETED);
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous_caller() -> RepoResult<()> {
        let repo = InMemoryRequestExecutionRepo::new();
        let anon = ctx_anon();

        let denied_find = repo.find_request_execution_by_id(&anon, "e-1").await;
        assert!(matches!(denied_find, Err(RepoError::Policy(_))));

        let denied_list = repo.list_request_executions(&anon, "p-1", "r-1").await;
        assert!(matches!(denied_list, Err(RepoError::Policy(_))));

        let denied_create = repo
            .create_request_execution(&anon, input("e-1", "p-1", "r-1", "t1"))
            .await;
        assert!(matches!(denied_create, Err(RepoError::Policy(_))));
        Ok(())
    }
}
