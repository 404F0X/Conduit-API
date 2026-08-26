//! Request repository — Rust port of `conduit/internal/server/biz/request.go`.
//!
//! RUST-P3-002 S13: `RequestRow` is now a hand-written typed struct.
//!
//! ## No soft delete
//! The Go `Request` schema uses only `TimeMixin{}` — no `SoftDeleteMixin`,
//! hence no `deleted_at` column. All rows are live.
//!
//! ## Status enum (mirrors Go `request.Status`)
//! `pending`|`processing`|`completed`|`failed`|`canceled`. CAS transitions
//! enforced by `transition_request_status`.

use crate::policy::ProjectAccess;
use crate::repo::{
    RepoError, RepoResult, RequestContext, guard_project_access, guard_repo_principal,
};
use crate::row::RequestRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

// Status values — mirror Go's `request.Status` enum.
pub const STATUS_PENDING: &str = "pending";
pub const STATUS_PROCESSING: &str = "processing";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_CANCELED: &str = "canceled";

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

/// Fields a caller sets when creating a request.
#[derive(Debug, Clone)]
pub struct CreateRequestInput {
    pub id: String,
    pub project_id: String,
    pub api_key_id: Option<String>,
    pub trace_id: Option<String>,
    pub data_storage_id: Option<String>,
    pub source: String,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub format: String,
    pub request_headers: Option<Value>,
    pub request_body: Value,
    pub channel_id: Option<String>,
    pub external_id: Option<String>,
    pub stream: bool,
    pub client_ip: String,
    pub created_at: String,
}

/// Dashboard list filter (RUST-P3-005 S17).
#[derive(Debug, Clone, Default)]
pub struct RequestListQuery {
    pub project_id: String,
    pub api_key_id: Option<String>,
    pub channel_id: Option<String>,
    pub model_id: Option<String>,
    pub source: Option<String>,
    pub status: Option<String>,
    pub start_at: Option<String>,
    pub end_at: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

impl RequestListQuery {
    pub fn for_project(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            limit: 20,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct RequestListResult {
    pub rows: Vec<RequestRow>,
    pub has_more: bool,
}

/// Patch applied by `mark_content_saved`.
#[derive(Debug, Clone, Default)]
pub struct ContentSavedInput {
    pub content_storage_id: Option<String>,
    pub content_storage_key: Option<String>,
    pub content_saved_at: Option<String>,
    pub updated_at: String,
}

/// Patch applied after an upstream response is received. Every optional field
/// leaves the existing column unchanged when absent; request status continues
/// to use the CAS transition methods below.
#[derive(Debug, Clone, Default)]
pub struct UpdateRequestInput {
    pub response_body: Option<Value>,
    pub response_chunks: Option<Value>,
    pub channel_id: Option<String>,
    pub metrics_latency_ms: Option<i64>,
    pub metrics_first_token_latency_ms: Option<i64>,
    pub metrics_reasoning_duration_ms: Option<i64>,
    pub updated_at: String,
}

fn row_from_input(input: &CreateRequestInput) -> RequestRow {
    let now = parse_dt(&input.created_at);
    RequestRow {
        id: input.id.clone(),
        project_id: input.project_id.clone(),
        status: STATUS_PENDING.into(),
        source: input.source.clone(),
        model_id: input.model_id.clone(),
        format: input.format.clone(),
        stream: input.stream,
        client_ip: input.client_ip.clone(),
        content_saved: false,
        api_key_id: input.api_key_id.clone(),
        trace_id: input.trace_id.clone(),
        data_storage_id: input.data_storage_id.clone(),
        reasoning_effort: input.reasoning_effort.clone(),
        request_headers: input.request_headers.clone(),
        request_body: input.request_body.clone(),
        response_body: None,
        response_chunks: None,
        channel_id: input.channel_id.clone(),
        external_id: input.external_id.clone(),
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

/// True when `(expected -> next)` is a legal status transition.
fn is_legal_transition(expected: &str, next: &str) -> bool {
    matches!(
        (expected, next),
        (STATUS_PENDING, STATUS_PROCESSING)
            | (STATUS_PENDING, STATUS_COMPLETED)
            | (STATUS_PENDING, STATUS_FAILED)
            | (STATUS_PENDING, STATUS_CANCELED)
            | (STATUS_PROCESSING, STATUS_COMPLETED)
            | (STATUS_PROCESSING, STATUS_FAILED)
            | (STATUS_PROCESSING, STATUS_CANCELED)
    )
}

// --- trait -----------------------------------------------------------------

#[async_trait]
pub trait RequestRepo: Send + Sync {
    async fn create_request_unchecked(
        &self,
        ctx: &RequestContext,
        row: RequestRow,
    ) -> RepoResult<RequestRow>;

    async fn create_request(
        &self,
        ctx: &RequestContext,
        input: CreateRequestInput,
    ) -> RepoResult<RequestRow> {
        guard_project_access(ctx, &input.project_id, ProjectAccess::Write)?;
        let row = row_from_input(&input);
        self.create_request_unchecked(ctx, row).await
    }

    async fn find_request_by_id_unchecked(
        &self,
        ctx: &RequestContext,
        request_id: &str,
    ) -> RepoResult<Option<RequestRow>>;

    async fn find_request_by_id(
        &self,
        ctx: &RequestContext,
        request_id: &str,
    ) -> RepoResult<Option<RequestRow>> {
        guard_repo_principal(ctx)?;
        self.find_request_by_id_unchecked(ctx, request_id).await
    }

    async fn find_request_by_external_id_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        external_id: &str,
    ) -> RepoResult<Option<RequestRow>>;

    async fn find_request_by_external_id(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        external_id: &str,
    ) -> RepoResult<Option<RequestRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.find_request_by_external_id_unchecked(ctx, project_id, external_id)
            .await
    }

    /// Return the channel used by the newest completed request linked to the
    /// trace. Failed/canceled requests and rows without a channel are ignored.
    async fn find_last_successful_channel_id_by_trace_unchecked(
        &self,
        _ctx: &RequestContext,
        _trace_id: &str,
    ) -> RepoResult<Option<String>> {
        Err(RepoError::NotImplemented(
            "RequestRepo::find_last_successful_channel_id_by_trace",
        ))
    }

    async fn find_last_successful_channel_id_by_trace(
        &self,
        ctx: &RequestContext,
        trace_id: &str,
    ) -> RepoResult<Option<String>> {
        guard_repo_principal(ctx)?;
        self.find_last_successful_channel_id_by_trace_unchecked(ctx, trace_id)
            .await
    }

    async fn find_request_by_cache_signature_unchecked(
        &self,
        _ctx: &RequestContext,
        _project_id: &str,
        _cache_signature: &str,
    ) -> RepoResult<Option<RequestRow>> {
        Err(RepoError::NotImplemented(
            "RequestRepo::find_request_by_cache_signature",
        ))
    }

    async fn find_request_by_cache_signature(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        cache_signature: &str,
    ) -> RepoResult<Option<RequestRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Read)?;
        self.find_request_by_cache_signature_unchecked(ctx, project_id, cache_signature)
            .await
    }

    async fn list_requests_unchecked(
        &self,
        ctx: &RequestContext,
        query: &RequestListQuery,
    ) -> RepoResult<RequestListResult>;

    async fn list_requests(
        &self,
        ctx: &RequestContext,
        query: &RequestListQuery,
    ) -> RepoResult<RequestListResult> {
        guard_project_access(ctx, &query.project_id, ProjectAccess::Read)?;
        self.list_requests_unchecked(ctx, query).await
    }

    async fn transition_request_status_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        expected_status: &str,
        next_status: &str,
    ) -> RepoResult<Option<RequestRow>>;

    async fn transition_request_status(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        expected_status: &str,
        next_status: &str,
    ) -> RepoResult<Option<RequestRow>> {
        guard_project_access(ctx, project_id, ProjectAccess::Write)?;
        self.transition_request_status_unchecked(
            ctx,
            project_id,
            request_id,
            expected_status,
            next_status,
        )
        .await
    }

    /// Persist response fields independently from the status CAS. This mirrors
    /// Go's completion update, which writes the response body and latency
    /// metrics on the request row as well as on the execution row.
    async fn update_request_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        input: UpdateRequestInput,
    ) -> RepoResult<RequestRow>;

    async fn update_request(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        input: UpdateRequestInput,
    ) -> RepoResult<RequestRow> {
        guard_project_access(ctx, project_id, ProjectAccess::Write)?;
        self.update_request_unchecked(ctx, project_id, request_id, input)
            .await
    }

    async fn mark_content_saved_unchecked(
        &self,
        ctx: &RequestContext,
        request_id: &str,
        input: ContentSavedInput,
    ) -> RepoResult<RequestRow>;

    async fn mark_content_saved(
        &self,
        ctx: &RequestContext,
        request_id: &str,
        input: ContentSavedInput,
    ) -> RepoResult<RequestRow> {
        guard_repo_principal(ctx)?;
        self.mark_content_saved_unchecked(ctx, request_id, input)
            .await
    }

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

#[derive(Debug, Default)]
pub struct InMemoryRequestRepo {
    rows: Mutex<BTreeMap<String, RequestRow>>,
}

impl InMemoryRequestRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = RequestRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl RequestRepo for InMemoryRequestRepo {
    async fn create_request_unchecked(
        &self,
        _ctx: &RequestContext,
        row: RequestRow,
    ) -> RepoResult<RequestRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request repo"))?;
        if guard.contains_key(&row.id) {
            return Err(RepoError::NotFound("request id already present"));
        }
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn find_request_by_id_unchecked(
        &self,
        _ctx: &RequestContext,
        request_id: &str,
    ) -> RepoResult<Option<RequestRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request repo"))?
            .get(request_id)
            .cloned())
    }

    async fn find_request_by_external_id_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        external_id: &str,
    ) -> RepoResult<Option<RequestRow>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request repo"))?
            .values()
            .find(|r| r.project_id == project_id && r.external_id.as_deref() == Some(external_id))
            .cloned())
    }

    async fn find_last_successful_channel_id_by_trace_unchecked(
        &self,
        _ctx: &RequestContext,
        trace_id: &str,
    ) -> RepoResult<Option<String>> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request repo"))?
            .values()
            .filter(|row| {
                row.trace_id.as_deref() == Some(trace_id)
                    && row.status == STATUS_COMPLETED
                    && row.channel_id.is_some()
            })
            .max_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.id.cmp(&b.id))
            })
            .and_then(|row| row.channel_id.clone()))
    }

    async fn list_requests_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &RequestListQuery,
    ) -> RepoResult<RequestListResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request repo"))?;
        let mut matched: Vec<RequestRow> = guard
            .values()
            .filter(|r| matches_query(r, query))
            .cloned()
            .collect();
        sort_and_paginate(&mut matched, query)
    }

    async fn transition_request_status_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        expected_status: &str,
        next_status: &str,
    ) -> RepoResult<Option<RequestRow>> {
        if !is_legal_transition(expected_status, next_status) {
            return Ok(None);
        }
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request repo"))?;
        let row = match guard.get_mut(request_id) {
            Some(r) if r.project_id == project_id && r.status == expected_status => r,
            _ => return Ok(None),
        };
        row.status = next_status.to_string();
        Ok(Some(row.clone()))
    }

    async fn update_request_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        request_id: &str,
        input: UpdateRequestInput,
    ) -> RepoResult<RequestRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request repo"))?;
        let row = guard
            .get_mut(request_id)
            .filter(|row| row.project_id == project_id)
            .ok_or(RepoError::NotFound("request"))?;

        if let Some(value) = input.response_body {
            row.response_body = Some(value);
        }
        if let Some(value) = input.response_chunks {
            row.response_chunks = Some(value);
        }
        if let Some(value) = input.channel_id {
            row.channel_id = Some(value);
        }
        if let Some(value) = input.metrics_latency_ms {
            row.metrics_latency_ms = Some(value);
        }
        if let Some(value) = input.metrics_first_token_latency_ms {
            row.metrics_first_token_latency_ms = Some(value);
        }
        if let Some(value) = input.metrics_reasoning_duration_ms {
            row.metrics_reasoning_duration_ms = Some(value);
        }
        row.updated_at = parse_dt(&input.updated_at);
        Ok(row.clone())
    }

    async fn mark_content_saved_unchecked(
        &self,
        _ctx: &RequestContext,
        request_id: &str,
        input: ContentSavedInput,
    ) -> RepoResult<RequestRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request repo"))?;
        let row = guard
            .get_mut(request_id)
            .ok_or(RepoError::NotFound("request"))?;
        row.content_saved = true;
        if let Some(v) = input.content_storage_id {
            row.content_storage_id = Some(v);
        }
        if let Some(v) = input.content_storage_key {
            row.content_storage_key = Some(v);
        }
        if let Some(v) = input.content_saved_at {
            row.content_saved_at = Some(parse_dt(&v));
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
        let cutoff = parse_dt(cutoff_created_at);
        let now_dt = parse_dt(now);
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("request repo"))?;
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

// --- shared helpers --------------------------------------------------------

fn matches_query(row: &RequestRow, query: &RequestListQuery) -> bool {
    if row.project_id != query.project_id {
        return false;
    }
    if let Some(v) = &query.api_key_id
        && row.api_key_id.as_deref() != Some(v.as_str())
    {
        return false;
    }
    if let Some(v) = &query.channel_id
        && row.channel_id.as_deref() != Some(v.as_str())
    {
        return false;
    }
    if let Some(v) = &query.model_id
        && row.model_id != *v
    {
        return false;
    }
    if let Some(v) = &query.source
        && row.source != *v
    {
        return false;
    }
    if let Some(v) = &query.status
        && row.status != *v
    {
        return false;
    }
    if let Some(start) = &query.start_at {
        let start_dt = parse_dt(start);
        if row.created_at < start_dt {
            return false;
        }
    }
    if let Some(end) = &query.end_at {
        let end_dt = parse_dt(end);
        if row.created_at > end_dt {
            return false;
        }
    }
    true
}

fn sort_and_paginate(
    rows: &mut [RequestRow],
    query: &RequestListQuery,
) -> RepoResult<RequestListResult> {
    rows.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });

    let limit = query.limit as usize;
    let offset = query.offset as usize;
    let window_start = offset.min(rows.len());
    let window_end = (window_start + limit).min(rows.len());
    let out = rows[window_start..window_end].to_vec();
    let has_more = window_end < rows.len();

    Ok(RequestListResult {
        rows: out,
        has_more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use serde_json::json;

    fn ctx_allowed() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn ctx_anon() -> RequestContext {
        RequestContext::new(PolicyContext::anonymous())
    }

    fn input(id: &str, project: &str, created_at: &str) -> CreateRequestInput {
        CreateRequestInput {
            id: id.into(),
            project_id: project.into(),
            api_key_id: Some("key-1".into()),
            trace_id: None,
            data_storage_id: None,
            source: "api".into(),
            model_id: "gpt-4".into(),
            reasoning_effort: None,
            format: "openai/chat_completions".into(),
            request_headers: None,
            request_body: json!({"messages": []}),
            channel_id: Some("ch-1".into()),
            external_id: None,
            stream: false,
            client_ip: "127.0.0.1".into(),
            created_at: created_at.into(),
        }
    }

    #[tokio::test]
    async fn create_then_find_by_id() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .create_request(&ctx, input("r-1", "p-1", "2024-01-01T00:00:00Z"))
            .await?;
        assert_eq!(created.id, "r-1");
        assert_eq!(created.status, STATUS_PENDING);
        assert_eq!(created.project_id, "p-1");

        let found = repo
            .find_request_by_id(&ctx, "r-1")
            .await?
            .ok_or(RepoError::NotFound("r-1"))?;
        assert_eq!(found.id, "r-1");
        assert!(!found.content_saved);
        Ok(())
    }

    #[tokio::test]
    async fn find_by_id_miss_returns_none() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();
        assert!(repo.find_request_by_id(&ctx, "missing").await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn find_by_external_id_scoped_to_project() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();

        let mut i = input("r-1", "p-1", "2024-01-01T00:00:00Z");
        i.external_id = Some("ext-abc".into());
        repo.create_request(&ctx, i).await?;

        assert!(
            repo.find_request_by_external_id(&ctx, "p-1", "ext-abc")
                .await?
                .is_some()
        );
        assert!(
            repo.find_request_by_external_id(&ctx, "p-2", "ext-abc")
                .await?
                .is_none()
        );
        assert!(
            repo.find_request_by_external_id(&ctx, "p-1", "ext-missing")
                .await?
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_dashboard_filters_by_project_status_and_date() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();

        repo.create_request(&ctx, input("r-1", "p-1", "2024-01-01T00:00:00Z"))
            .await?;
        repo.create_request(&ctx, input("r-2", "p-1", "2024-01-02T00:00:00Z"))
            .await?;
        repo.create_request(&ctx, input("r-3", "p-2", "2024-01-02T00:00:00Z"))
            .await?;

        repo.transition_request_status(&ctx, "p-1", "r-1", STATUS_PENDING, STATUS_PROCESSING)
            .await?;
        repo.transition_request_status(&ctx, "p-1", "r-1", STATUS_PROCESSING, STATUS_COMPLETED)
            .await?;

        let p1 = repo
            .list_requests(&ctx, &RequestListQuery::for_project("p-1"))
            .await?;
        let ids: Vec<_> = p1.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["r-1", "r-2"]);

        let completed = repo
            .list_requests(
                &ctx,
                &RequestListQuery {
                    project_id: "p-1".into(),
                    status: Some(STATUS_COMPLETED.into()),
                    limit: 10,
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = completed.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["r-1"]);

        let window = repo
            .list_requests(
                &ctx,
                &RequestListQuery {
                    project_id: "p-1".into(),
                    start_at: Some("2024-01-02T00:00:00Z".into()),
                    end_at: Some("2024-01-02T23:59:59Z".into()),
                    limit: 10,
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = window.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["r-2"]);
        Ok(())
    }

    #[tokio::test]
    async fn last_successful_channel_uses_newest_completed_request_for_trace() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();

        let mut earlier = input("r-1", "p-1", "2024-01-01T00:00:00Z");
        earlier.trace_id = Some("7".into());
        earlier.channel_id = Some("11".into());
        repo.create_request(&ctx, earlier).await?;
        repo.transition_request_status(&ctx, "p-1", "r-1", STATUS_PENDING, STATUS_COMPLETED)
            .await?;

        let mut latest = input("r-2", "p-1", "2024-01-02T00:00:00Z");
        latest.trace_id = Some("7".into());
        latest.channel_id = Some("22".into());
        repo.create_request(&ctx, latest).await?;
        repo.transition_request_status(&ctx, "p-1", "r-2", STATUS_PENDING, STATUS_COMPLETED)
            .await?;

        let mut failed = input("r-3", "p-1", "2024-01-03T00:00:00Z");
        failed.trace_id = Some("7".into());
        failed.channel_id = Some("33".into());
        repo.create_request(&ctx, failed).await?;
        repo.transition_request_status(&ctx, "p-1", "r-3", STATUS_PENDING, STATUS_FAILED)
            .await?;

        assert_eq!(
            repo.find_last_successful_channel_id_by_trace(&ctx, "7")
                .await?,
            Some("22".into())
        );
        assert_eq!(
            repo.find_last_successful_channel_id_by_trace(&ctx, "missing")
                .await?,
            None
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_transition_legal_path() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();
        repo.create_request(&ctx, input("r-1", "p-1", "2024-01-01T00:00:00Z"))
            .await?;

        let step1 = repo
            .transition_request_status(&ctx, "p-1", "r-1", STATUS_PENDING, STATUS_PROCESSING)
            .await?
            .ok_or(RepoError::NotFound("transition step1"))?;
        assert_eq!(step1.status, STATUS_PROCESSING);

        let step2 = repo
            .transition_request_status(&ctx, "p-1", "r-1", STATUS_PROCESSING, STATUS_COMPLETED)
            .await?
            .ok_or(RepoError::NotFound("transition step2"))?;
        assert_eq!(step2.status, STATUS_COMPLETED);
        Ok(())
    }

    #[tokio::test]
    async fn status_transition_illegal_pair_returns_none() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();
        repo.create_request(&ctx, input("r-1", "p-1", "2024-01-01T00:00:00Z"))
            .await?;

        let miss = repo
            .transition_request_status(&ctx, "p-1", "r-1", STATUS_COMPLETED, STATUS_PENDING)
            .await?;
        assert!(miss.is_none());

        let found = repo
            .find_request_by_id(&ctx, "r-1")
            .await?
            .ok_or(RepoError::NotFound("r-1"))?;
        assert_eq!(found.status, STATUS_PENDING);
        Ok(())
    }

    #[tokio::test]
    async fn status_transition_wrong_expected_returns_none() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();
        repo.create_request(&ctx, input("r-1", "p-1", "2024-01-01T00:00:00Z"))
            .await?;

        let miss = repo
            .transition_request_status(&ctx, "p-1", "r-1", STATUS_PROCESSING, STATUS_COMPLETED)
            .await?;
        assert!(miss.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn mark_content_saved_sets_flag_and_storage_fields() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();
        repo.create_request(&ctx, input("r-1", "p-1", "2024-01-01T00:00:00Z"))
            .await?;

        let updated = repo
            .mark_content_saved(
                &ctx,
                "r-1",
                ContentSavedInput {
                    content_storage_id: Some("ds-1".into()),
                    content_storage_key: Some("video/r-1.mp4".into()),
                    content_saved_at: Some("2024-02-01T00:00:00Z".into()),
                    updated_at: "2024-02-01T00:00:00Z".into(),
                },
            )
            .await?;
        assert!(updated.content_saved);
        assert_eq!(updated.content_storage_key, Some("video/r-1.mp4".into()));
        Ok(())
    }

    #[tokio::test]
    async fn pagination_limit_and_offset() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();
        for n in 1..=5 {
            let ts = format!("2024-01-{:02}T00:00:00Z", n);
            repo.create_request(&ctx, input(&format!("r-{n}"), "p-1", &ts))
                .await?;
        }

        let page1 = repo
            .list_requests(
                &ctx,
                &RequestListQuery {
                    project_id: "p-1".into(),
                    limit: 2,
                    offset: 0,
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = page1.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["r-1", "r-2"]);
        assert!(page1.has_more);

        let page2 = repo
            .list_requests(
                &ctx,
                &RequestListQuery {
                    project_id: "p-1".into(),
                    limit: 2,
                    offset: 2,
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = page2.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["r-3", "r-4"]);
        assert!(page2.has_more);

        let page3 = repo
            .list_requests(
                &ctx,
                &RequestListQuery {
                    project_id: "p-1".into(),
                    limit: 2,
                    offset: 4,
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(page3.rows.len(), 1);
        assert!(!page3.has_more);
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous_caller() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let anon = ctx_anon();

        let denied = repo.find_request_by_id(&anon, "r-1").await;
        assert!(matches!(denied, Err(RepoError::Policy(_))));

        let denied_list = repo
            .list_requests(&anon, &RequestListQuery::for_project("p-1"))
            .await;
        assert!(matches!(denied_list, Err(RepoError::Policy(_))));

        let denied_create = repo
            .create_request(&anon, input("r-1", "p-1", "2024-01-01T00:00:00Z"))
            .await;
        assert!(matches!(denied_create, Err(RepoError::Policy(_))));
        Ok(())
    }

    #[tokio::test]
    async fn reclaim_stale_processing_cancels_old_processing_requests() -> RepoResult<()> {
        let repo = InMemoryRequestRepo::new();
        let ctx = ctx_allowed();

        repo.create_request(&ctx, input("r-stale", "p-1", "2024-01-01T00:00:00Z"))
            .await?;
        repo.transition_request_status(&ctx, "p-1", "r-stale", STATUS_PENDING, STATUS_PROCESSING)
            .await?;

        repo.create_request(&ctx, input("r-fresh", "p-1", "2024-01-02T00:30:00Z"))
            .await?;
        repo.transition_request_status(&ctx, "p-1", "r-fresh", STATUS_PENDING, STATUS_PROCESSING)
            .await?;

        repo.create_request(&ctx, input("r-pending", "p-1", "2024-01-01T00:00:00Z"))
            .await?;

        let reclaimed = repo
            .reclaim_stale_processing(&ctx, "2024-01-02T00:00:00Z", "2024-01-03T00:00:00Z")
            .await?;
        assert_eq!(reclaimed, vec!["r-stale".to_string()]);

        let stale = repo
            .find_request_by_id(&ctx, "r-stale")
            .await?
            .ok_or(RepoError::NotFound("r-stale"))?;
        assert_eq!(stale.status, STATUS_CANCELED);

        let fresh = repo
            .find_request_by_id(&ctx, "r-fresh")
            .await?
            .ok_or(RepoError::NotFound("r-fresh"))?;
        assert_eq!(fresh.status, STATUS_PROCESSING);

        let pending = repo
            .find_request_by_id(&ctx, "r-pending")
            .await?
            .ok_or(RepoError::NotFound("r-pending"))?;
        assert_eq!(pending.status, STATUS_PENDING);
        Ok(())
    }
}
