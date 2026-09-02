//! PostgreSQL-backed append-only usage/accounting repository.

use crate::repo::usage_repo::{UsageListOrderField, UsageListQuery, UsageListResult, UsageRepo};
use crate::repo::{RepoError, RepoResult, RequestContext, UsageAggregate, UsageAggregateQuery};
use crate::row::UsageLogRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

const COLUMNS: &str = "CAST(id AS TEXT) AS id,CAST(request_id AS TEXT) AS request_id,CAST(api_key_id AS TEXT) AS api_key_id,CAST(project_id AS TEXT) AS project_id,CAST(channel_id AS TEXT) AS channel_id,model_id,prompt_tokens,completion_tokens,total_tokens,COALESCE(prompt_audio_tokens,0) AS prompt_audio_tokens,COALESCE(prompt_cached_tokens,0) AS prompt_cached_tokens,COALESCE(prompt_write_cached_tokens,0) AS prompt_write_cached_tokens,COALESCE(prompt_write_cached_tokens_5m,0) AS prompt_write_cached_tokens_5m,COALESCE(prompt_write_cached_tokens_1h,0) AS prompt_write_cached_tokens_1h,COALESCE(completion_audio_tokens,0) AS completion_audio_tokens,COALESCE(completion_reasoning_tokens,0) AS completion_reasoning_tokens,COALESCE(completion_accepted_prediction_tokens,0) AS completion_accepted_prediction_tokens,COALESCE(completion_rejected_prediction_tokens,0) AS completion_rejected_prediction_tokens,\"source\",format,total_cost,COALESCE(cost_items,'[]'::jsonb) AS cost_items,cost_price_reference_id,created_at,updated_at";
const MICROS: f64 = 1_000_000.0;
#[derive(Debug, Clone)]
pub struct PgUsageRepo {
    pool: PgPool,
}
impl PgUsageRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn find_by_id(&self, value: i64) -> RepoResult<Option<UsageLogRow>> {
        sqlx::query_as::<_, UsageLogRow>(&format!("SELECT {COLUMNS} FROM usage_logs WHERE id=$1"))
            .bind(value)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| error("find id", e))
    }
}
fn id(v: &str) -> RepoResult<i64> {
    v.parse()
        .map_err(|_| RepoError::NotFound("usage edge id not a valid integer"))
}
fn opt_id(v: &Option<String>) -> RepoResult<Option<i64>> {
    v.as_deref().filter(|v| !v.is_empty()).map(id).transpose()
}
fn timestamp(v: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(v)
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or_default()
}
fn error(c: &str, e: sqlx::Error) -> RepoError {
    RepoError::Database(format!("postgres usage repo {c} failed: {e}"))
}

fn list_filters(b: &mut QueryBuilder<Postgres>, q: &UsageListQuery) -> RepoResult<()> {
    if !q.project_id.is_empty() {
        b.push(" AND project_id=").push_bind(id(&q.project_id)?);
    }
    if let Some(v) = &q.id {
        b.push(" AND id=").push_bind(id(v)?);
    }
    if let Some(v) = &q.api_key_id {
        b.push(" AND api_key_id=").push_bind(id(v)?);
    }
    if let Some(v) = &q.channel_id {
        b.push(" AND channel_id=").push_bind(id(v)?);
    }
    if let Some(v) = &q.model_id {
        b.push(" AND model_id=").push_bind(v.clone());
    }
    if let Some(v) = &q.source {
        b.push(" AND \"source\"=").push_bind(v.clone());
    }
    if let Some(v) = &q.request_id {
        b.push(" AND request_id=").push_bind(id(v)?);
    }
    if let Some(v) = &q.start_at {
        b.push(" AND created_at>=").push_bind(timestamp(v));
    }
    if let Some(v) = &q.end_at {
        b.push(" AND created_at<=").push_bind(timestamp(v));
    }
    Ok(())
}
fn aggregate_filters(b: &mut QueryBuilder<Postgres>, q: &UsageAggregateQuery) -> RepoResult<()> {
    if !q.project_id.is_empty() {
        b.push(" AND project_id=").push_bind(id(&q.project_id)?);
    }
    if let Some(v) = &q.api_key_id {
        b.push(" AND api_key_id=").push_bind(id(v)?);
    }
    if let Some(v) = &q.channel_id {
        b.push(" AND channel_id=").push_bind(id(v)?);
    }
    if let Some(v) = &q.model_id {
        b.push(" AND model_id=").push_bind(v.clone());
    }
    if let Some(v) = &q.source {
        b.push(" AND \"source\"=").push_bind(v.clone());
    }
    if let Some(v) = &q.start_at {
        b.push(" AND created_at>=").push_bind(timestamp(v));
    }
    if let Some(v) = &q.end_at {
        b.push(" AND created_at<=").push_bind(timestamp(v));
    }
    Ok(())
}

#[async_trait]
impl UsageRepo for PgUsageRepo {
    async fn insert_usage_unchecked(
        &self,
        _: &RequestContext,
        r: UsageLogRow,
    ) -> RepoResult<UsageLogRow> {
        let new_id=sqlx::query_scalar::<_,i64>("INSERT INTO usage_logs(request_id,api_key_id,project_id,channel_id,model_id,prompt_tokens,completion_tokens,total_tokens,prompt_audio_tokens,prompt_cached_tokens,prompt_write_cached_tokens,prompt_write_cached_tokens_5m,prompt_write_cached_tokens_1h,completion_audio_tokens,completion_reasoning_tokens,completion_accepted_prediction_tokens,completion_rejected_prediction_tokens,\"source\",format,total_cost,cost_items,cost_price_reference_id,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24) RETURNING id")
   .bind(id(&r.request_id)?).bind(opt_id(&r.api_key_id)?).bind(id(&r.project_id)?).bind(opt_id(&r.channel_id)?).bind(&r.model_id).bind(r.prompt_tokens).bind(r.completion_tokens).bind(r.total_tokens).bind(r.prompt_audio_tokens).bind(r.prompt_cached_tokens).bind(r.prompt_write_cached_tokens).bind(r.prompt_write_cached_tokens_5m).bind(r.prompt_write_cached_tokens_1h).bind(r.completion_audio_tokens).bind(r.completion_reasoning_tokens).bind(r.completion_accepted_prediction_tokens).bind(r.completion_rejected_prediction_tokens).bind(&r.source).bind(&r.format).bind(r.total_cost).bind(sqlx::types::Json(r.cost_items)).bind(r.cost_price_reference_id).bind(r.created_at).bind(r.updated_at).fetch_one(&self.pool).await.map_err(|e|error("insert",e))?;
        sqlx::query_as::<_, UsageLogRow>(&format!("SELECT {COLUMNS} FROM usage_logs WHERE id=$1"))
            .bind(new_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| error("readback", e))
    }
    async fn aggregate_usage_unchecked(
        &self,
        _: &RequestContext,
        q: UsageAggregateQuery,
    ) -> RepoResult<UsageAggregate> {
        let mut b = QueryBuilder::<Postgres>::new(
            "SELECT COALESCE(SUM(prompt_tokens),0)::BIGINT,COALESCE(SUM(completion_tokens),0)::BIGINT,COALESCE(SUM(total_tokens),0)::BIGINT,COALESCE(SUM(total_cost),0.0)::DOUBLE PRECISION,COUNT(*)::BIGINT FROM usage_logs WHERE TRUE",
        );
        aggregate_filters(&mut b, &q)?;
        let (input, output, total, cost, count): (i64, i64, i64, f64, i64) = b
            .build_query_as()
            .fetch_one(&self.pool)
            .await
            .map_err(|e| error("aggregate", e))?;
        Ok(UsageAggregate {
            project_id: q.project_id,
            request_count: count.max(0) as u64,
            input_tokens: input.max(0) as u64,
            output_tokens: output.max(0) as u64,
            total_tokens: total.max(0) as u64,
            total_cost_micros: (cost * MICROS).round() as i64,
        })
    }
    async fn list_usage_unchecked(
        &self,
        _: &RequestContext,
        q: &UsageListQuery,
    ) -> RepoResult<UsageListResult> {
        let mut b =
            QueryBuilder::<Postgres>::new(format!("SELECT {COLUMNS} FROM usage_logs WHERE TRUE"));
        list_filters(&mut b, q)?;
        b.push(" ORDER BY ").push(match q.order_field {
            UsageListOrderField::Id => "id",
            UsageListOrderField::CreatedAt => "created_at",
            UsageListOrderField::UpdatedAt => "updated_at",
        });
        b.push(if q.descending {
            " DESC,id DESC"
        } else {
            " ASC,id ASC"
        });
        b.push(" LIMIT ")
            .push_bind(i64::from(q.limit) + 1)
            .push(" OFFSET ")
            .push_bind(i64::from(q.offset));
        let mut rows = b
            .build_query_as::<UsageLogRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| error("list", e))?;
        let has_more = rows.len() > q.limit as usize;
        rows.truncate(q.limit as usize);
        Ok(UsageListResult { rows, has_more })
    }
    async fn count_usage_unchecked(
        &self,
        _: &RequestContext,
        q: &UsageListQuery,
    ) -> RepoResult<u64> {
        let mut b =
            QueryBuilder::<Postgres>::new("SELECT COUNT(*)::BIGINT FROM usage_logs WHERE TRUE");
        list_filters(&mut b, q)?;
        let count = b
            .build_query_scalar::<i64>()
            .fetch_one(&self.pool)
            .await
            .map_err(|e| error("count", e))?;
        Ok(count.max(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use crate::repo::usage_repo::CreateUsageLogInput;

    #[tokio::test]
    async fn postgres_usage_insert_list_and_aggregate_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let repo = PgUsageRepo::new(database.pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let seed = chrono::Utc::now().timestamp_micros().to_string();
        let row = repo
            .insert_usage(
                &ctx,
                CreateUsageLogInput {
                    id: "ignored".into(),
                    project_id: seed.clone(),
                    request_id: seed.clone(),
                    api_key_id: Some(seed.clone()),
                    channel_id: Some(seed.clone()),
                    model_id: "mock-chat".into(),
                    prompt_tokens: 100,
                    completion_tokens: 25,
                    total_tokens: 125,
                    prompt_audio_tokens: 0,
                    prompt_cached_tokens: 10,
                    prompt_write_cached_tokens: 0,
                    prompt_write_cached_tokens_5m: 0,
                    prompt_write_cached_tokens_1h: 0,
                    completion_audio_tokens: 0,
                    completion_reasoning_tokens: 5,
                    completion_accepted_prediction_tokens: 0,
                    completion_rejected_prediction_tokens: 0,
                    source: "api".into(),
                    format: "openai/chat_completions".into(),
                    total_cost: Some(0.125),
                    cost_items: serde_json::json!([{"type":"input","cost":0.1}]),
                    cost_price_reference_id: Some("price-v1".into()),
                    created_at: "2026-08-15T00:00:00Z".into(),
                },
            )
            .await?;
        assert_eq!(row.prompt_cached_tokens, 10);
        let list = repo
            .list_usage(&ctx, &UsageListQuery::for_project(&seed))
            .await?;
        assert_eq!(list.rows.len(), 1);
        let total = repo
            .aggregate_usage(&ctx, UsageAggregateQuery::new(&seed))
            .await?;
        assert_eq!(total.total_tokens, 125);
        assert_eq!(total.total_cost_micros, 125_000);
        sqlx::query("DELETE FROM usage_logs WHERE id = $1")
            .bind(row.id.parse::<i64>()?)
            .execute(repo.pool())
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
