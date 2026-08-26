//! PostgreSQL-backed upstream execution-attempt repository.
use crate::repo::request_execution_repo::{RequestExecutionRepo, UpdateRequestExecutionInput};
use crate::repo::{RepoError, RepoResult, RequestContext};
use crate::row::RequestExecutionRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};
const C: &str = "CAST(id AS TEXT) AS id,CAST(project_id AS TEXT) AS project_id,CAST(request_id AS TEXT) AS request_id,CAST(channel_id AS TEXT) AS channel_id,credential_identity,CAST(data_storage_id AS TEXT) AS data_storage_id,external_id,model_id,format,request_body,response_body,response_chunks,error_message,response_status_code,\"status\",stream,metrics_latency_ms,metrics_first_token_latency_ms,metrics_reasoning_duration_ms,request_headers,request_url,pass_through_applied,created_at,updated_at";
#[derive(Debug, Clone)]
pub struct PgRequestExecutionRepo {
    pool: PgPool,
}
impl PgRequestExecutionRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}
fn id(v: &str) -> RepoResult<i64> {
    v.parse()
        .map_err(|_| RepoError::NotFound("execution edge id not a valid integer"))
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
    RepoError::Database(format!("postgres request execution repo {c} failed: {e}"))
}
#[async_trait]
impl RequestExecutionRepo for PgRequestExecutionRepo {
    async fn create_request_execution_unchecked(
        &self,
        ctx: &RequestContext,
        r: RequestExecutionRow,
    ) -> RepoResult<RequestExecutionRow> {
        let n=sqlx::query_scalar::<_,i64>("INSERT INTO request_executions(project_id,request_id,channel_id,credential_identity,data_storage_id,external_id,model_id,format,request_body,response_body,response_chunks,error_message,response_status_code,\"status\",stream,metrics_latency_ms,metrics_first_token_latency_ms,metrics_reasoning_duration_ms,request_headers,request_url,pass_through_applied,created_at,updated_at)VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23)RETURNING id").bind(id(&r.project_id)?).bind(id(&r.request_id)?).bind(opt(&r.channel_id)?).bind(r.credential_identity).bind(opt(&r.data_storage_id)?).bind(r.external_id).bind(r.model_id).bind(r.format).bind(sqlx::types::Json(r.request_body)).bind(r.response_body.map(sqlx::types::Json)).bind(r.response_chunks.map(sqlx::types::Json)).bind(r.error_message).bind(r.response_status_code).bind(r.status).bind(r.stream).bind(r.metrics_latency_ms).bind(r.metrics_first_token_latency_ms).bind(r.metrics_reasoning_duration_ms).bind(r.request_headers.map(sqlx::types::Json)).bind(r.request_url).bind(r.pass_through_applied).bind(r.created_at).bind(r.updated_at).fetch_one(&self.pool).await.map_err(|e|err("create",e))?;
        self.find_request_execution_by_id_unchecked(ctx, &n.to_string())
            .await?
            .ok_or(RepoError::NotFound("execution"))
    }
    async fn find_request_execution_by_id_unchecked(
        &self,
        _: &RequestContext,
        v: &str,
    ) -> RepoResult<Option<RequestExecutionRow>> {
        sqlx::query_as::<_, RequestExecutionRow>(&format!(
            "SELECT {C} FROM request_executions WHERE id=$1"
        ))
        .bind(id(v)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| err("find", e))
    }
    async fn list_request_executions_unchecked(
        &self,
        _: &RequestContext,
        p: &str,
        r: &str,
    ) -> RepoResult<Vec<RequestExecutionRow>> {
        sqlx::query_as::<_,RequestExecutionRow>(&format!("SELECT {C} FROM request_executions WHERE project_id=$1 AND request_id=$2 ORDER BY created_at,id")).bind(id(p)?).bind(id(r)?).fetch_all(&self.pool).await.map_err(|e|err("list",e))
    }

    async fn list_all_request_executions_unchecked(
        &self,
        _ctx: &RequestContext,
        limit: u32,
    ) -> RepoResult<Vec<RequestExecutionRow>> {
        sqlx::query_as::<_, RequestExecutionRow>(&format!(
            "SELECT {C} FROM request_executions ORDER BY created_at,id LIMIT $1"
        ))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| err("list all", e))
    }
    async fn update_request_execution_unchecked(
        &self,
        ctx: &RequestContext,
        p: &str,
        e: &str,
        i: UpdateRequestExecutionInput,
    ) -> RepoResult<RequestExecutionRow> {
        let mut b = QueryBuilder::<Postgres>::new("UPDATE request_executions SET ");
        let mut s = b.separated(", ");
        macro_rules! set {
            ($f:expr,$v:expr) => {{
                s.push($f).push_bind_unseparated($v);
            }};
        }
        if let Some(v) = i.status {
            set!("\"status\"=", v)
        }
        if let Some(v) = i.external_id {
            set!("external_id=", v)
        }
        if let Some(v) = i.response_body {
            set!("response_body=", sqlx::types::Json(v))
        }
        if let Some(v) = i.response_chunks {
            set!("response_chunks=", sqlx::types::Json(v))
        }
        if let Some(v) = i.error_message {
            set!("error_message=", v)
        }
        if let Some(v) = i.response_status_code {
            set!("response_status_code=", v)
        }
        if let Some(v) = i.metrics_latency_ms {
            set!("metrics_latency_ms=", v)
        }
        if let Some(v) = i.metrics_first_token_latency_ms {
            set!("metrics_first_token_latency_ms=", v)
        }
        if let Some(v) = i.metrics_reasoning_duration_ms {
            set!("metrics_reasoning_duration_ms=", v)
        }
        if let Some(v) = i.request_url {
            set!("request_url=", v)
        }
        s.push("updated_at=now()");
        drop(s);
        b.push(" WHERE project_id=")
            .push_bind(id(p)?)
            .push(" AND id=")
            .push_bind(id(e)?);
        if b.build()
            .execute(&self.pool)
            .await
            .map_err(|v| err("update", v))?
            .rows_affected()
            == 0
        {
            return Err(RepoError::NotFound("execution"));
        }
        self.find_request_execution_by_id_unchecked(ctx, e)
            .await?
            .ok_or(RepoError::NotFound("execution"))
    }
    async fn reclaim_stale_processing_unchecked(
        &self,
        _: &RequestContext,
        cutoff: &str,
        now: &str,
    ) -> RepoResult<Vec<String>> {
        sqlx::query_scalar::<_,i64>("UPDATE request_executions SET \"status\"='canceled',updated_at=$2 WHERE \"status\"='processing' AND created_at<$1 RETURNING id").bind(ts(cutoff)).bind(ts(now)).fetch_all(&self.pool).await.map(|v|v.into_iter().map(|v|v.to_string()).collect()).map_err(|e|err("reclaim",e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use crate::repo::request_execution_repo::CreateRequestExecutionInput;
    #[tokio::test]
    async fn postgres_execution_streaming_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let repo = PgRequestExecutionRepo::new(database.pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let row = repo
            .create_request_execution(
                &ctx,
                CreateRequestExecutionInput {
                    id: "ignored".into(),
                    project_id: "1".into(),
                    request_id: "1".into(),
                    channel_id: Some("1".into()),
                    credential_identity: Some("sha256:test-credential".into()),
                    data_storage_id: None,
                    model_id: "mock-chat".into(),
                    format: "openai/chat_completions".into(),
                    request_headers: None,
                    request_body: serde_json::json!({"stream":true}),
                    stream: true,
                    request_url: Some("http://mock/v1/chat/completions".into()),
                    pass_through_applied: false,
                    created_at: "2026-08-15T00:00:00Z".into(),
                },
            )
            .await?;
        assert_eq!(row.status, "processing");
        assert_eq!(
            row.credential_identity.as_deref(),
            Some("sha256:test-credential")
        );
        let done = repo
            .update_request_execution(
                &ctx,
                "1",
                &row.id,
                UpdateRequestExecutionInput {
                    status: Some("completed".into()),
                    response_chunks: Some(vec![serde_json::json!({"data":"chunk"})]),
                    response_status_code: Some(200),
                    metrics_latency_ms: Some(25),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(done.status, "completed");
        assert_eq!(done.response_status_code, Some(200));
        assert_eq!(repo.list_request_executions(&ctx, "1", "1").await?.len(), 1);
        database.cleanup().await?;
        Ok(())
    }
}
