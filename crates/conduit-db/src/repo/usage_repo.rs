//! Usage-log repository — Rust port of `conduit/internal/server/biz/usage_log.go`.
//!
//! Mirrors the Go `UsageLogService` data-access surface against the `UsageLog`
//! Ent schema (`internal/ent/schema/usage_log.go`): usage-log insert, dashboard
//! aggregation (token + cost totals over a date range + multi-dimension
//! filter), and paginated listing.
//!
//! ## No soft delete
//! The Go `UsageLog` schema's `Mixin()` returns only `TimeMixin{}` — there is
//! **no `SoftDeleteMixin`**, hence no `deleted_at` column and no
//! `*_with_deleted` / restore surface here. Usage logs are append-only; the
//! dashboard reads them directly.
//!
//! ## Storage model (RUST-P3-002 S13 batch 2)
//! `UsageLogRow` is a hand-written typed struct carrying the real Go entity
//! columns (edges, token counters incl. the nine prompt/completion breakdown
//! fields, source/format, cost fields, timestamps). The previous
//! `minimal_row!` `extra`-map shape is retired. The breakdown fields were
//! missing from `CreateUsageLogInput` under the placeholder row — the Go
//! `CreateUsageLog` sets them from `llm.Usage.PromptTokensDetails` /
//! `CompletionTokensDetails` (usage_log.go:123-136) — so they are now part of
//! the input (0 mirrors Go `Default(0)`).
//!
//! ## Cost representation
//! `total_cost` is the procurement cost normalized into the configured
//! real-world accounting currency. The row keeps the raw float
//! (`Option<f64>`, `Nillable()` in Go) and the aggregate exposes
//! it as integer micros (`total_cost_micros = round(total_cost * 1_000_000)`)
//! so the dashboard can sum without floating-point drift. `cost_items` is
//! stored as-is (JSON array of `objects.CostItem`).

use crate::policy::ProjectAccess;
use crate::repo::{
    RepoError, RepoResult, RequestContext, UsageAggregate, UsageAggregateQuery,
    guard_project_access,
};
use crate::row::UsageLogRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Micros-per-accounting-unit scaling factor for converting `total_cost` into
/// the integer aggregate domain.
const COST_MICROS_PER_UNIT: f64 = 1_000_000.0;

/// Fields a caller sets when creating a usage log. Mirrors the Go
/// `UsageLogService.CreateUsageLog` builder inputs (usage_log.go:104-153).
/// Token counts default to 0 when unset (matching Go's
/// `field.Int64().Default(0)`).
#[derive(Debug, Clone)]
pub struct CreateUsageLogInput {
    pub id: String,
    pub project_id: String,
    pub request_id: String,
    pub api_key_id: Option<String>,
    pub channel_id: Option<String>,
    pub model_id: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// Prompt breakdown (Go `SetPromptAudioTokens` etc., usage_log.go:123-127).
    pub prompt_audio_tokens: i64,
    pub prompt_cached_tokens: i64,
    pub prompt_write_cached_tokens: i64,
    pub prompt_write_cached_tokens_5m: i64,
    pub prompt_write_cached_tokens_1h: i64,
    /// Completion breakdown (Go `SetCompletionAudioTokens` etc.,
    /// usage_log.go:133-136).
    pub completion_audio_tokens: i64,
    pub completion_reasoning_tokens: i64,
    pub completion_accepted_prediction_tokens: i64,
    pub completion_rejected_prediction_tokens: i64,
    /// `api` | `playground` | `test` (Go enum `usagelog.Source`).
    pub source: String,
    pub format: String,
    /// Procurement cost in the configured accounting currency. `None` means
    /// the cost is unavailable (for example, because an FX quote is missing).
    pub total_cost: Option<f64>,
    /// JSON array of `objects.CostItem` breakdown.
    pub cost_items: Value,
    pub cost_price_reference_id: Option<String>,
    pub created_at: String,
}

/// Dashboard list filter. Every field is optional; `None` means "no filter".
/// `project_id` is **required** (dashboard is project-scoped). Date range is
/// inclusive on both bounds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UsageListOrderField {
    Id,
    #[default]
    CreatedAt,
    UpdatedAt,
}

#[derive(Debug, Clone, Default)]
pub struct UsageListQuery {
    pub project_id: String,
    pub id: Option<String>,
    pub api_key_id: Option<String>,
    pub channel_id: Option<String>,
    pub model_id: Option<String>,
    pub source: Option<String>,
    pub request_id: Option<String>,
    pub start_at: Option<String>,
    pub end_at: Option<String>,
    pub limit: u32,
    pub offset: u32,
    pub order_field: UsageListOrderField,
    pub descending: bool,
}

impl UsageListQuery {
    pub fn for_project(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            limit: 20,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct UsageListResult {
    pub rows: Vec<UsageLogRow>,
    pub has_more: bool,
}

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

// --- row helpers -----------------------------------------------------------

/// Build a `UsageLogRow` from creation input. Both timestamps start at
/// `created_at` (Go `TimeMixin` defaults both to the creation instant).
fn row_from_input(input: &CreateUsageLogInput) -> UsageLogRow {
    let now = parse_dt(&input.created_at);
    UsageLogRow {
        id: input.id.clone(),
        request_id: input.request_id.clone(),
        api_key_id: input.api_key_id.clone(),
        project_id: input.project_id.clone(),
        channel_id: input.channel_id.clone(),
        model_id: input.model_id.clone(),
        prompt_tokens: input.prompt_tokens,
        completion_tokens: input.completion_tokens,
        total_tokens: input.total_tokens,
        prompt_audio_tokens: input.prompt_audio_tokens,
        prompt_cached_tokens: input.prompt_cached_tokens,
        prompt_write_cached_tokens: input.prompt_write_cached_tokens,
        prompt_write_cached_tokens_5m: input.prompt_write_cached_tokens_5m,
        prompt_write_cached_tokens_1h: input.prompt_write_cached_tokens_1h,
        completion_audio_tokens: input.completion_audio_tokens,
        completion_reasoning_tokens: input.completion_reasoning_tokens,
        completion_accepted_prediction_tokens: input.completion_accepted_prediction_tokens,
        completion_rejected_prediction_tokens: input.completion_rejected_prediction_tokens,
        source: input.source.clone(),
        format: input.format.clone(),
        total_cost: input.total_cost,
        cost_items: input.cost_items.clone(),
        cost_price_reference_id: input.cost_price_reference_id.clone(),
        created_at: now,
        updated_at: now,
    }
}

/// Apply the `UsageListQuery` filter to a row. `project_id` is always enforced.
fn matches_query(row: &UsageLogRow, query: &UsageListQuery) -> bool {
    if !query.project_id.is_empty() && row.project_id != query.project_id {
        return false;
    }
    if let Some(v) = &query.id
        && row.id != *v
    {
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
    if let Some(v) = &query.request_id
        && row.request_id != *v
    {
        return false;
    }
    if let Some(start) = &query.start_at
        && row.created_at < parse_dt(start)
    {
        return false;
    }
    if let Some(end) = &query.end_at
        && row.created_at > parse_dt(end)
    {
        return false;
    }
    true
}

/// Apply the `UsageAggregateQuery` filter to a row. `project_id` is always
/// enforced; the dashboard filters narrow alongside the date range.
fn matches_aggregate_query(row: &UsageLogRow, query: &UsageAggregateQuery) -> bool {
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
    if let Some(start) = &query.start_at
        && row.created_at < parse_dt(start)
    {
        return false;
    }
    if let Some(end) = &query.end_at
        && row.created_at > parse_dt(end)
    {
        return false;
    }
    true
}

/// Sort by `(created_at, id)` (stable tie-breaker), then return the
/// offset/limit window.
fn sort_and_paginate(
    rows: &mut [UsageLogRow],
    query: &UsageListQuery,
) -> RepoResult<UsageListResult> {
    rows.sort_by(|a, b| {
        let id_ordering = || match (a.id.parse::<i64>(), b.id.parse::<i64>()) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            _ => a.id.cmp(&b.id),
        };
        let ordering = match query.order_field {
            UsageListOrderField::Id => id_ordering(),
            UsageListOrderField::CreatedAt => a.created_at.cmp(&b.created_at),
            UsageListOrderField::UpdatedAt => a.updated_at.cmp(&b.updated_at),
        }
        .then_with(id_ordering);
        if query.descending {
            ordering.reverse()
        } else {
            ordering
        }
    });

    let limit = query.limit as usize;
    let offset = query.offset as usize;
    let window_start = offset.min(rows.len());
    let window_end = (window_start + limit).min(rows.len());
    let out = rows[window_start..window_end].to_vec();
    let has_more = window_end < rows.len();

    Ok(UsageListResult {
        rows: out,
        has_more,
    })
}

// --- trait -----------------------------------------------------------------

/// Repository surface for the `usage_logs` table.
///
/// Method naming convention (consistent with `RequestRepo` etc.):
/// - `*_unchecked` — skips the policy guard; callers must have already enforced
///   authorization. Public methods wrap these with the appropriate guard.
///
/// **No soft-delete variants**: the Go `UsageLog` schema has no `deleted_at`.
#[async_trait]
pub trait UsageRepo: Send + Sync {
    // --- insert ---
    async fn insert_usage_unchecked(
        &self,
        ctx: &RequestContext,
        row: UsageLogRow,
    ) -> RepoResult<UsageLogRow>;

    async fn insert_usage(
        &self,
        ctx: &RequestContext,
        input: CreateUsageLogInput,
    ) -> RepoResult<UsageLogRow> {
        guard_project_access(ctx, &input.project_id, ProjectAccess::Write)?;
        let row = row_from_input(&input);
        self.insert_usage_unchecked(ctx, row).await
    }

    // --- dashboard aggregation ---
    /// Aggregate token + cost totals over a `(project_id, date-range)` bucket,
    /// narrowed by the optional dashboard filters. Mirrors the Go dashboard
    /// `SUM(prompt_tokens)`, `SUM(completion_tokens)`, `SUM(total_tokens)`,
    /// `SUM(total_cost)`, `COUNT(*)` query.
    async fn aggregate_usage_unchecked(
        &self,
        ctx: &RequestContext,
        query: UsageAggregateQuery,
    ) -> RepoResult<UsageAggregate>;

    async fn aggregate_usage(
        &self,
        ctx: &RequestContext,
        query: UsageAggregateQuery,
    ) -> RepoResult<UsageAggregate> {
        guard_project_access(ctx, &query.project_id, ProjectAccess::Read)?;
        self.aggregate_usage_unchecked(ctx, query).await
    }

    // --- list (paginated) ---
    async fn list_usage_unchecked(
        &self,
        ctx: &RequestContext,
        query: &UsageListQuery,
    ) -> RepoResult<UsageListResult>;

    async fn count_usage_unchecked(
        &self,
        ctx: &RequestContext,
        query: &UsageListQuery,
    ) -> RepoResult<u64> {
        let mut page_query = query.clone();
        page_query.limit = 1_000;
        page_query.offset = 0;
        let mut total = 0_u64;
        loop {
            let page = self.list_usage_unchecked(ctx, &page_query).await?;
            let fetched = u32::try_from(page.rows.len()).unwrap_or(u32::MAX);
            total = total.saturating_add(u64::from(fetched));
            if !page.has_more || fetched == 0 {
                break;
            }
            let next_offset = page_query.offset.saturating_add(fetched);
            if next_offset == page_query.offset {
                break;
            }
            page_query.offset = next_offset;
        }
        Ok(total)
    }

    async fn count_usage(&self, ctx: &RequestContext, query: &UsageListQuery) -> RepoResult<u64> {
        guard_project_access(ctx, &query.project_id, ProjectAccess::Read)?;
        self.count_usage_unchecked(ctx, query).await
    }

    async fn list_usage(
        &self,
        ctx: &RequestContext,
        query: &UsageListQuery,
    ) -> RepoResult<UsageListResult> {
        guard_project_access(ctx, &query.project_id, ProjectAccess::Read)?;
        self.list_usage_unchecked(ctx, query).await
    }
}

// --- in-memory implementation ----------------------------------------------

/// In-memory `UsageRepo` for unit tests and the bootstrap runtime before sqlx
/// lands. Storage is keyed by `id`. No soft-delete state (matches the Go
/// `UsageLog` schema).
#[derive(Debug, Default)]
pub struct InMemoryUsageRepo {
    rows: Mutex<BTreeMap<String, UsageLogRow>>,
}

impl InMemoryUsageRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_rows(rows: impl IntoIterator<Item = UsageLogRow>) -> Self {
        let rows = rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        Self {
            rows: Mutex::new(rows),
        }
    }

    pub fn len(&self) -> RepoResult<usize> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("usage repo"))?
            .len())
    }

    pub fn is_empty(&self) -> RepoResult<bool> {
        Ok(self.len()? == 0)
    }
}

#[async_trait]
impl UsageRepo for InMemoryUsageRepo {
    async fn insert_usage_unchecked(
        &self,
        _ctx: &RequestContext,
        row: UsageLogRow,
    ) -> RepoResult<UsageLogRow> {
        let mut guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("usage repo"))?;
        if guard.contains_key(&row.id) {
            return Err(RepoError::NotFound("usage_log id already present"));
        }
        guard.insert(row.id.clone(), row.clone());
        Ok(row)
    }

    async fn aggregate_usage_unchecked(
        &self,
        _ctx: &RequestContext,
        query: UsageAggregateQuery,
    ) -> RepoResult<UsageAggregate> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("usage repo"))?;

        let mut agg = UsageAggregate {
            project_id: query.project_id.clone(),
            ..Default::default()
        };
        // Sum cost in f64 first to match Go's `SUM(total_cost)` semantics,
        // then round to micros once at the end to avoid per-row rounding drift.
        // Rows with NULL total_cost (Go `Nillable()`) contribute 0.
        let mut total_cost_f64: f64 = 0.0;

        for row in guard.values() {
            if !matches_aggregate_query(row, &query) {
                continue;
            }
            agg.request_count += 1;
            agg.input_tokens += row.prompt_tokens.max(0) as u64;
            agg.output_tokens += row.completion_tokens.max(0) as u64;
            agg.total_tokens += row.total_tokens.max(0) as u64;
            if let Some(cost) = row.total_cost {
                total_cost_f64 += cost;
            }
        }
        agg.total_cost_micros = (total_cost_f64 * COST_MICROS_PER_UNIT).round() as i64;
        Ok(agg)
    }

    async fn list_usage_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &UsageListQuery,
    ) -> RepoResult<UsageListResult> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("usage repo"))?;
        let mut matched: Vec<UsageLogRow> = guard
            .values()
            .filter(|r| matches_query(r, query))
            .cloned()
            .collect();
        sort_and_paginate(&mut matched, query)
    }

    async fn count_usage_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &UsageListQuery,
    ) -> RepoResult<u64> {
        let guard = self
            .rows
            .lock()
            .map_err(|_| RepoError::LockPoisoned("usage repo"))?;
        Ok(guard
            .values()
            .filter(|row| matches_query(row, query))
            .count() as u64)
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
        prompt: i64,
        completion: i64,
        cost: Option<f64>,
        created_at: &str,
    ) -> CreateUsageLogInput {
        CreateUsageLogInput {
            id: id.into(),
            project_id: project.into(),
            request_id: "req-1".into(),
            api_key_id: Some("key-1".into()),
            channel_id: Some("ch-1".into()),
            model_id: "gpt-4".into(),
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            prompt_audio_tokens: 0,
            prompt_cached_tokens: 0,
            prompt_write_cached_tokens: 0,
            prompt_write_cached_tokens_5m: 0,
            prompt_write_cached_tokens_1h: 0,
            completion_audio_tokens: 0,
            completion_reasoning_tokens: 0,
            completion_accepted_prediction_tokens: 0,
            completion_rejected_prediction_tokens: 0,
            source: "api".into(),
            format: "openai/chat_completions".into(),
            total_cost: cost,
            cost_items: json!([]),
            cost_price_reference_id: None,
            created_at: created_at.into(),
        }
    }

    #[tokio::test]
    async fn insert_then_list_finds_row() -> RepoResult<()> {
        let repo = InMemoryUsageRepo::new();
        let ctx = ctx_allowed();

        let created = repo
            .insert_usage(
                &ctx,
                input("u-1", "p-1", 100, 50, Some(0.012), "2024-01-01T00:00:00Z"),
            )
            .await?;
        assert_eq!(created.id, "u-1");
        assert_eq!(created.project_id, "p-1");

        let listed = repo
            .list_usage(&ctx, &UsageListQuery::for_project("p-1"))
            .await?;
        assert_eq!(listed.rows.len(), 1);
        assert_eq!(listed.rows[0].id, "u-1");
        Ok(())
    }

    /// The typed row surfaces the nine breakdown counters the placeholder row
    /// omitted — Go sets them from `llm.Usage` details (usage_log.go:123-136).
    #[tokio::test]
    async fn insert_persists_token_breakdown_fields() -> RepoResult<()> {
        let repo = InMemoryUsageRepo::new();
        let ctx = ctx_allowed();

        let mut i = input("u-1", "p-1", 100, 50, None, "2024-01-01T00:00:00Z");
        i.prompt_audio_tokens = 1;
        i.prompt_cached_tokens = 2;
        i.prompt_write_cached_tokens = 3;
        i.prompt_write_cached_tokens_5m = 4;
        i.prompt_write_cached_tokens_1h = 5;
        i.completion_audio_tokens = 6;
        i.completion_reasoning_tokens = 7;
        i.completion_accepted_prediction_tokens = 8;
        i.completion_rejected_prediction_tokens = 9;

        let created = repo.insert_usage(&ctx, i).await?;
        assert_eq!(created.prompt_audio_tokens, 1);
        assert_eq!(created.prompt_cached_tokens, 2);
        assert_eq!(created.prompt_write_cached_tokens, 3);
        assert_eq!(created.prompt_write_cached_tokens_5m, 4);
        assert_eq!(created.prompt_write_cached_tokens_1h, 5);
        assert_eq!(created.completion_audio_tokens, 6);
        assert_eq!(created.completion_reasoning_tokens, 7);
        assert_eq!(created.completion_accepted_prediction_tokens, 8);
        assert_eq!(created.completion_rejected_prediction_tokens, 9);
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_sums_tokens_and_cost() -> RepoResult<()> {
        let repo = InMemoryUsageRepo::new();
        let ctx = ctx_allowed();

        repo.insert_usage(
            &ctx,
            input("u-1", "p-1", 100, 50, Some(0.010), "2024-01-01T00:00:00Z"),
        )
        .await?;
        repo.insert_usage(
            &ctx,
            input("u-2", "p-1", 200, 100, Some(0.020), "2024-01-02T00:00:00Z"),
        )
        .await?;
        // Different project — excluded.
        repo.insert_usage(
            &ctx,
            input("u-3", "p-2", 999, 999, Some(9.0), "2024-01-01T00:00:00Z"),
        )
        .await?;
        // Same project, outside window — excluded.
        repo.insert_usage(
            &ctx,
            input("u-4", "p-1", 999, 999, None, "2024-02-01T00:00:00Z"),
        )
        .await?;

        let agg = repo
            .aggregate_usage(
                &ctx,
                UsageAggregateQuery {
                    project_id: "p-1".into(),
                    start_at: Some("2024-01-01T00:00:00Z".into()),
                    end_at: Some("2024-01-31T23:59:59Z".into()),
                    ..UsageAggregateQuery::new("p-1")
                },
            )
            .await?;

        assert_eq!(agg.project_id, "p-1");
        assert_eq!(agg.request_count, 2);
        assert_eq!(agg.input_tokens, 300);
        assert_eq!(agg.output_tokens, 150);
        assert_eq!(agg.total_tokens, 450);
        // 0.010 + 0.020 = 0.030 accounting units -> 30000 micros.
        assert_eq!(agg.total_cost_micros, 30_000);
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_filters_by_channel_and_model() -> RepoResult<()> {
        let repo = InMemoryUsageRepo::new();
        let ctx = ctx_allowed();

        let mut a = input("u-1", "p-1", 100, 50, Some(0.01), "2024-01-01");
        a.channel_id = Some("ch-1".into());
        a.model_id = "gpt-4".into();
        repo.insert_usage(&ctx, a).await?;

        let mut b = input("u-2", "p-1", 200, 100, Some(0.02), "2024-01-01");
        b.channel_id = Some("ch-2".into());
        b.model_id = "claude-3".into();
        repo.insert_usage(&ctx, b).await?;

        // Filter to ch-1 only.
        let agg_ch1 = repo
            .aggregate_usage(
                &ctx,
                UsageAggregateQuery {
                    project_id: "p-1".into(),
                    channel_id: Some("ch-1".into()),
                    ..UsageAggregateQuery::new("p-1")
                },
            )
            .await?;
        assert_eq!(agg_ch1.request_count, 1);
        assert_eq!(agg_ch1.input_tokens, 100);

        // Filter to claude-3 only.
        let agg_claude = repo
            .aggregate_usage(
                &ctx,
                UsageAggregateQuery {
                    project_id: "p-1".into(),
                    model_id: Some("claude-3".into()),
                    ..UsageAggregateQuery::new("p-1")
                },
            )
            .await?;
        assert_eq!(agg_claude.request_count, 1);
        assert_eq!(agg_claude.input_tokens, 200);
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_null_cost_contributes_zero() -> RepoResult<()> {
        let repo = InMemoryUsageRepo::new();
        let ctx = ctx_allowed();

        // One row with a cost, one without (Go `Nillable()` NULL).
        repo.insert_usage(&ctx, input("u-1", "p-1", 10, 5, Some(0.005), "2024-01-01"))
            .await?;
        repo.insert_usage(&ctx, input("u-2", "p-1", 10, 5, None, "2024-01-01"))
            .await?;

        let agg = repo
            .aggregate_usage(&ctx, UsageAggregateQuery::new("p-1"))
            .await?;
        assert_eq!(agg.request_count, 2);
        // Only the priced row contributes; 0.005 accounting units = 5000 micros.
        assert_eq!(agg.total_cost_micros, 5_000);
        Ok(())
    }

    #[tokio::test]
    async fn list_filters_and_paginates() -> RepoResult<()> {
        let repo = InMemoryUsageRepo::new();
        let ctx = ctx_allowed();

        for n in 1..=4 {
            let ts = format!("2024-01-{:02}T00:00:00Z", n);
            repo.insert_usage(
                &ctx,
                input(&format!("u-{n}"), "p-1", n * 10, n * 5, Some(0.01), &ts),
            )
            .await?;
        }
        // Different project.
        repo.insert_usage(&ctx, input("u-x", "p-2", 999, 999, Some(9.0), "2024-01-01"))
            .await?;

        // Page 1.
        let page1 = repo
            .list_usage(
                &ctx,
                &UsageListQuery {
                    project_id: "p-1".into(),
                    limit: 2,
                    offset: 0,
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = page1.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["u-1", "u-2"]);
        assert!(page1.has_more);

        // Page 2.
        let page2 = repo
            .list_usage(
                &ctx,
                &UsageListQuery {
                    project_id: "p-1".into(),
                    limit: 2,
                    offset: 2,
                    ..Default::default()
                },
            )
            .await?;
        let ids: Vec<_> = page2.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["u-3", "u-4"]);
        assert!(!page2.has_more);

        // Filter by request_id.
        let by_req = repo
            .list_usage(
                &ctx,
                &UsageListQuery {
                    project_id: "p-1".into(),
                    request_id: Some("req-1".into()),
                    limit: 10,
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(by_req.rows.len(), 4);
        Ok(())
    }

    #[tokio::test]
    async fn policy_guard_blocks_anonymous_caller() -> RepoResult<()> {
        let repo = InMemoryUsageRepo::new();
        let anon = ctx_anon();

        let denied_insert = repo
            .insert_usage(&anon, input("u-1", "p-1", 1, 1, None, "t1"))
            .await;
        assert!(matches!(denied_insert, Err(RepoError::Policy(_))));

        let denied_list = repo
            .list_usage(&anon, &UsageListQuery::for_project("p-1"))
            .await;
        assert!(matches!(denied_list, Err(RepoError::Policy(_))));

        let denied_agg = repo
            .aggregate_usage(&anon, UsageAggregateQuery::new("p-1"))
            .await;
        assert!(matches!(denied_agg, Err(RepoError::Policy(_))));
        Ok(())
    }
}
