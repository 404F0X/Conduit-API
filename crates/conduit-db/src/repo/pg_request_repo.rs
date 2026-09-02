//! PostgreSQL-backed request lifecycle repository.
use crate::repo::request_repo::{
    ContentSavedInput, RequestListOrderField, RequestListQuery, RequestListResult, RequestRepo,
    UpdateRequestInput,
};
use crate::repo::{RepoError, RepoResult, RequestContext};
use crate::row::RequestRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

const C: &str = "CAST(id AS TEXT) AS id,CAST(project_id AS TEXT) AS project_id,\"status\",\"source\",model_id,format,stream,client_ip,content_saved,CAST(api_key_id AS TEXT) AS api_key_id,CAST(trace_id AS TEXT) AS trace_id,CAST(data_storage_id AS TEXT) AS data_storage_id,reasoning_effort,request_headers,request_body,response_body,response_chunks,CAST(channel_id AS TEXT) AS channel_id,external_id,metrics_latency_ms,metrics_first_token_latency_ms,metrics_reasoning_duration_ms,CAST(content_storage_id AS TEXT) AS content_storage_id,content_storage_key,content_saved_at,created_at,updated_at";
#[derive(Debug, Clone)]
pub struct PgRequestRepo {
    pool: PgPool,
}
impl PgRequestRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}
fn id(v: &str) -> RepoResult<i64> {
    v.parse()
        .map_err(|_| RepoError::NotFound("request edge id not a valid integer"))
}
fn opt(v: &Option<String>) -> RepoResult<Option<i64>> {
    v.as_deref().filter(|v| !v.is_empty()).map(id).transpose()
}
fn ts(v: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(v)
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or_default()
}
fn err(c: &str, e: sqlx::Error) -> RepoError {
    RepoError::Database(format!("postgres request repo {c} failed: {e}"))
}

fn list_filters(b: &mut QueryBuilder<Postgres>, q: &RequestListQuery) -> RepoResult<()> {
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
    if let Some(v) = &q.trace_id {
        b.push(" AND trace_id=").push_bind(id(v)?);
    }
    for (column, is_nil) in [
        ("trace_id", q.trace_id_is_nil),
        ("api_key_id", q.api_key_id_is_nil),
        ("channel_id", q.channel_id_is_nil),
    ] {
        if let Some(is_nil) = is_nil {
            b.push(" AND ").push(column);
            b.push(if is_nil { " IS NULL" } else { " IS NOT NULL" });
        }
    }
    if let Some(v) = &q.model_id {
        b.push(" AND model_id=").push_bind(v.clone());
    }
    if let Some(v) = &q.source {
        b.push(" AND \"source\"=").push_bind(v.clone());
    }
    if let Some(v) = &q.status {
        b.push(" AND \"status\"=").push_bind(v.clone());
    }
    if let Some(v) = &q.start_at {
        b.push(" AND created_at>=").push_bind(ts(v));
    }
    if let Some(v) = &q.end_at {
        b.push(" AND created_at<=").push_bind(ts(v));
    }
    Ok(())
}
#[async_trait]
impl RequestRepo for PgRequestRepo {
    async fn create_request_unchecked(
        &self,
        ctx: &RequestContext,
        r: RequestRow,
    ) -> RepoResult<RequestRow> {
        let n=sqlx::query_scalar::<_,i64>("INSERT INTO requests(project_id,api_key_id,trace_id,data_storage_id,\"source\",model_id,reasoning_effort,format,request_headers,request_body,response_body,response_chunks,channel_id,external_id,\"status\",stream,client_ip,metrics_latency_ms,metrics_first_token_latency_ms,metrics_reasoning_duration_ms,content_saved,content_storage_id,content_storage_key,content_saved_at,created_at,updated_at)VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26)RETURNING id").bind(id(&r.project_id)?).bind(opt(&r.api_key_id)?).bind(opt(&r.trace_id)?).bind(opt(&r.data_storage_id)?).bind(r.source).bind(r.model_id).bind(r.reasoning_effort).bind(r.format).bind(r.request_headers.map(sqlx::types::Json)).bind(sqlx::types::Json(r.request_body)).bind(r.response_body.map(sqlx::types::Json)).bind(r.response_chunks.map(sqlx::types::Json)).bind(opt(&r.channel_id)?).bind(r.external_id).bind(r.status).bind(r.stream).bind(r.client_ip).bind(r.metrics_latency_ms).bind(r.metrics_first_token_latency_ms).bind(r.metrics_reasoning_duration_ms).bind(r.content_saved).bind(opt(&r.content_storage_id)?).bind(r.content_storage_key).bind(r.content_saved_at).bind(r.created_at).bind(r.updated_at).fetch_one(&self.pool).await.map_err(|e|err("create",e))?;
        self.find_request_by_id_unchecked(ctx, &n.to_string())
            .await?
            .ok_or(RepoError::NotFound("request"))
    }
    async fn find_request_by_id_unchecked(
        &self,
        _: &RequestContext,
        v: &str,
    ) -> RepoResult<Option<RequestRow>> {
        sqlx::query_as::<_, RequestRow>(&format!("SELECT {C} FROM requests WHERE id=$1"))
            .bind(id(v)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| err("find", e))
    }
    async fn find_request_by_external_id_unchecked(
        &self,
        _: &RequestContext,
        p: &str,
        e: &str,
    ) -> RepoResult<Option<RequestRow>> {
        sqlx::query_as::<_,RequestRow>(&format!("SELECT {C} FROM requests WHERE project_id=$1 AND external_id=$2 ORDER BY id DESC LIMIT 1")).bind(id(p)?).bind(e).fetch_optional(&self.pool).await.map_err(|e|err("find external",e))
    }
    async fn find_last_successful_channel_id_by_trace_unchecked(
        &self,
        _: &RequestContext,
        trace_id: &str,
    ) -> RepoResult<Option<String>> {
        sqlx::query_scalar::<_, i64>(
            "SELECT channel_id FROM requests \
             WHERE trace_id=$1 AND status='completed' AND channel_id IS NOT NULL \
             ORDER BY created_at DESC,id DESC LIMIT 1",
        )
        .bind(id(trace_id)?)
        .fetch_optional(&self.pool)
        .await
        .map(|channel_id| channel_id.map(|value| value.to_string()))
        .map_err(|error| err("find last successful channel by trace", error))
    }
    async fn list_requests_unchecked(
        &self,
        _: &RequestContext,
        q: &RequestListQuery,
    ) -> RepoResult<RequestListResult> {
        let mut b = QueryBuilder::<Postgres>::new(format!("SELECT {C} FROM requests WHERE TRUE"));
        list_filters(&mut b, q)?;
        b.push(" ORDER BY ").push(match q.order_field {
            RequestListOrderField::Id => "id",
            RequestListOrderField::CreatedAt => "created_at",
            RequestListOrderField::UpdatedAt => "updated_at",
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
            .build_query_as::<RequestRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| err("list", e))?;
        let has_more = rows.len() > q.limit as usize;
        rows.truncate(q.limit as usize);
        Ok(RequestListResult { rows, has_more })
    }
    async fn count_requests_unchecked(
        &self,
        _: &RequestContext,
        q: &RequestListQuery,
    ) -> RepoResult<u64> {
        let mut b =
            QueryBuilder::<Postgres>::new("SELECT COUNT(*)::BIGINT FROM requests WHERE TRUE");
        list_filters(&mut b, q)?;
        let count = b
            .build_query_scalar::<i64>()
            .fetch_one(&self.pool)
            .await
            .map_err(|e| err("count", e))?;
        Ok(count.max(0) as u64)
    }
    async fn transition_request_status_unchecked(
        &self,
        ctx: &RequestContext,
        p: &str,
        r: &str,
        expected: &str,
        next: &str,
    ) -> RepoResult<Option<RequestRow>> {
        let legal = matches!(
            (expected, next),
            ("pending", "processing")
                | ("pending", "completed")
                | ("pending", "failed")
                | ("pending", "canceled")
                | ("processing", "completed")
                | ("processing", "failed")
                | ("processing", "canceled")
        );
        if !legal {
            return Ok(None);
        }
        let changed=sqlx::query("UPDATE requests SET \"status\"=$4,updated_at=now() WHERE project_id=$1 AND id=$2 AND \"status\"=$3").bind(id(p)?).bind(id(r)?).bind(expected).bind(next).execute(&self.pool).await.map_err(|e|err("transition",e))?.rows_affected();
        if changed == 0 {
            Ok(None)
        } else {
            self.find_request_by_id_unchecked(ctx, r).await
        }
    }
    async fn update_request_unchecked(
        &self,
        ctx: &RequestContext,
        p: &str,
        r: &str,
        i: UpdateRequestInput,
    ) -> RepoResult<RequestRow> {
        let mut b = QueryBuilder::<Postgres>::new("UPDATE requests SET ");
        let mut s = b.separated(", ");
        macro_rules! set {
            ($field:expr, $value:expr) => {{
                s.push($field).push_bind_unseparated($value);
            }};
        }
        if let Some(value) = i.response_body {
            set!("response_body=", sqlx::types::Json(value));
        }
        if let Some(value) = i.response_chunks {
            set!("response_chunks=", sqlx::types::Json(value));
        }
        if let Some(value) = i.channel_id {
            set!("channel_id=", id(&value)?);
        }
        if let Some(value) = i.metrics_latency_ms {
            set!("metrics_latency_ms=", value);
        }
        if let Some(value) = i.metrics_first_token_latency_ms {
            set!("metrics_first_token_latency_ms=", value);
        }
        if let Some(value) = i.metrics_reasoning_duration_ms {
            set!("metrics_reasoning_duration_ms=", value);
        }
        set!("updated_at=", ts(&i.updated_at));
        drop(s);
        b.push(" WHERE project_id=")
            .push_bind(id(p)?)
            .push(" AND id=")
            .push_bind(id(r)?);
        if b.build()
            .execute(&self.pool)
            .await
            .map_err(|error| err("update", error))?
            .rows_affected()
            == 0
        {
            return Err(RepoError::NotFound("request"));
        }
        self.find_request_by_id_unchecked(ctx, r)
            .await?
            .ok_or(RepoError::NotFound("request"))
    }
    async fn mark_content_saved_unchecked(
        &self,
        ctx: &RequestContext,
        r: &str,
        i: ContentSavedInput,
    ) -> RepoResult<RequestRow> {
        let changed=sqlx::query("UPDATE requests SET content_saved=TRUE,content_storage_id=$2,content_storage_key=$3,content_saved_at=$4,updated_at=now() WHERE id=$1").bind(id(r)?).bind(opt(&i.content_storage_id)?).bind(i.content_storage_key).bind(i.content_saved_at.as_deref().map(ts)).execute(&self.pool).await.map_err(|e|err("content saved",e))?.rows_affected();
        if changed == 0 {
            return Err(RepoError::NotFound("request"));
        }
        self.find_request_by_id_unchecked(ctx, r)
            .await?
            .ok_or(RepoError::NotFound("request"))
    }
    async fn reclaim_stale_processing_unchecked(
        &self,
        _: &RequestContext,
        cutoff: &str,
        now: &str,
    ) -> RepoResult<Vec<String>> {
        sqlx::query_scalar::<_,i64>("UPDATE requests SET \"status\"='canceled',updated_at=$2 WHERE \"status\"='processing' AND created_at<$1 RETURNING id").bind(ts(cutoff)).bind(ts(now)).fetch_all(&self.pool).await.map(|v|v.into_iter().map(|v|v.to_string()).collect()).map_err(|e|err("reclaim",e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use crate::repo::request_repo::CreateRequestInput;
    use crate::repo::trace_repo::TraceRepo;

    #[tokio::test]
    async fn postgres_request_status_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let repo = PgRequestRepo::new(database.pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let trace = crate::PgTraceRepo::new(database.pool.clone())
            .get_or_create_trace(
                &ctx,
                "1",
                "pg-request-sticky-trace",
                None,
                "2026-08-15T00:00:00Z".into(),
            )
            .await?;
        let row = repo
            .create_request(
                &ctx,
                CreateRequestInput {
                    id: "ignored".into(),
                    project_id: "1".into(),
                    api_key_id: Some("1".into()),
                    trace_id: Some(trace.id.clone()),
                    data_storage_id: None,
                    source: "api".into(),
                    model_id: "mock-chat".into(),
                    reasoning_effort: None,
                    format: "openai/chat_completions".into(),
                    request_headers: Some(serde_json::json!({"x-test":"1"})),
                    request_body: serde_json::json!({"model":"mock-chat"}),
                    channel_id: None,
                    external_id: Some("req-ext-1".into()),
                    stream: true,
                    client_ip: "127.0.0.1".into(),
                    created_at: "2026-08-15T00:00:00Z".into(),
                },
            )
            .await?;
        assert_eq!(row.status, "pending");
        assert!(
            repo.transition_request_status(&ctx, "1", &row.id, "pending", "processing")
                .await?
                .is_some()
        );
        let updated = repo
            .update_request(
                &ctx,
                "1",
                &row.id,
                UpdateRequestInput {
                    response_body: Some(serde_json::json!({"id": "response-1"})),
                    channel_id: Some("1".into()),
                    metrics_latency_ms: Some(125),
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    ..UpdateRequestInput::default()
                },
            )
            .await?;
        assert_eq!(
            updated.response_body,
            Some(serde_json::json!({"id": "response-1"}))
        );
        assert_eq!(updated.metrics_latency_ms, Some(125));
        assert_eq!(updated.channel_id.as_deref(), Some("1"));
        assert!(
            repo.transition_request_status(&ctx, "1", &row.id, "processing", "completed")
                .await?
                .is_some()
        );
        let found = repo
            .find_request_by_external_id(&ctx, "1", "req-ext-1")
            .await?
            .ok_or("created request was not found")?;
        assert_eq!(found.status, "completed");
        assert_eq!(
            repo.find_last_successful_channel_id_by_trace(&ctx, &trace.id)
                .await?,
            Some("1".into())
        );

        let all_projects = repo
            .list_requests_unchecked(
                &ctx,
                &RequestListQuery {
                    project_id: String::new(),
                    limit: 10,
                    ..RequestListQuery::default()
                },
            )
            .await?;
        assert_eq!(all_projects.rows.len(), 1);
        assert_eq!(all_projects.rows[0].id, row.id);

        let other_project = repo
            .list_requests_unchecked(
                &ctx,
                &RequestListQuery {
                    project_id: "999999".into(),
                    limit: 10,
                    ..RequestListQuery::default()
                },
            )
            .await?;
        assert!(other_project.rows.is_empty());

        database.cleanup().await?;
        Ok(())
    }
}
