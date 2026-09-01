//! PostgreSQL-backed trace repository.

use crate::repo::{RepoError, RepoResult, RequestContext, TraceRepo};
use crate::row::TraceRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, CAST(project_id AS TEXT) AS project_id, \
trace_id, CAST(thread_id AS TEXT) AS thread_id, created_at, updated_at";

#[derive(Debug, Clone)]
pub struct PgTraceRepo {
    pool: PgPool,
}

impl PgTraceRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Global admin listing used by `Query.traces`.
    pub async fn list_all(&self) -> RepoResult<Vec<TraceRow>> {
        sqlx::query_as::<_, TraceRow>(&format!("SELECT {COLUMNS} FROM traces ORDER BY id ASC"))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| database_error("list all", error))
    }

    pub async fn list_by_project(&self, project_id: &str) -> RepoResult<Vec<TraceRow>> {
        sqlx::query_as::<_, TraceRow>(&format!(
            "SELECT {COLUMNS} FROM traces WHERE project_id = $1 ORDER BY id ASC"
        ))
        .bind(parse_id(
            project_id,
            "trace project id not a valid integer",
        )?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error("list by project", error))
    }

    /// Primary-key lookup used by the Relay `node(id:)` resolver.
    pub async fn find_by_row_id(&self, id: i64) -> RepoResult<Option<TraceRow>> {
        sqlx::query_as::<_, TraceRow>(&format!("SELECT {COLUMNS} FROM traces WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| database_error("find by row id", error))
    }
}

fn parse_id(value: &str, label: &'static str) -> RepoResult<i64> {
    value.parse().map_err(|_| RepoError::NotFound(label))
}

fn parse_optional_id(value: Option<String>) -> RepoResult<Option<i64>> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| parse_id(value, "trace thread id not a valid integer"))
        .transpose()
}

fn parse_timestamp(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
                .map(|value| DateTime::<Utc>::from_naive_utc_and_offset(value, Utc))
        })
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").map(|value| {
                DateTime::<Utc>::from_naive_utc_and_offset(
                    value.and_hms_opt(0, 0, 0).unwrap_or_default(),
                    Utc,
                )
            })
        })
        .unwrap_or_else(|_| DateTime::from_timestamp(0, 0).unwrap_or_default())
}

fn database_error(operation: &str, error: sqlx::Error) -> RepoError {
    RepoError::Database(format!("postgres trace repo {operation} failed: {error}"))
}

async fn fetch_by_pair(
    pool: &PgPool,
    project_id: i64,
    trace_id: &str,
) -> RepoResult<Option<TraceRow>> {
    sqlx::query_as::<_, TraceRow>(&format!(
        "SELECT {COLUMNS} FROM traces WHERE project_id = $1 AND trace_id = $2"
    ))
    .bind(project_id)
    .bind(trace_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| database_error("find by project and trace id", error))
}

#[async_trait]
impl TraceRepo for PgTraceRepo {
    async fn get_or_create_trace_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        trace_id: &str,
        thread_id: Option<String>,
        now: String,
    ) -> RepoResult<TraceRow> {
        let project_id = parse_id(project_id, "trace project id not a valid integer")?;
        let thread_id = parse_optional_id(thread_id)?;
        let now = parse_timestamp(&now);

        sqlx::query(
            "INSERT INTO traces (project_id, trace_id, thread_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $4) ON CONFLICT (trace_id) DO NOTHING",
        )
        .bind(project_id)
        .bind(trace_id)
        .bind(thread_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|error| database_error("get or create insert", error))?;

        fetch_by_pair(&self.pool, project_id, trace_id)
            .await?
            .ok_or(RepoError::NotFound("trace"))
    }

    async fn find_trace_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        trace_id: &str,
    ) -> RepoResult<Option<TraceRow>> {
        fetch_by_pair(
            &self.pool,
            parse_id(project_id, "trace project id not a valid integer")?,
            trace_id,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use crate::repo::ThreadRepo;

    fn context() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    #[tokio::test]
    async fn postgres_trace_preserves_immutable_thread_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let thread_repo = crate::PgThreadRepo::new(pool.clone());
        let repo = PgTraceRepo::new(pool.clone());
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let thread_external_id = format!("pg-trace-thread-{suffix}");
        let trace_external_id = format!("pg-trace-{suffix}");
        let project_id = "91000002";
        let thread = thread_repo
            .get_or_create_thread(
                &context(),
                project_id,
                &thread_external_id,
                "2026-08-15T00:00:00Z".into(),
            )
            .await?;

        let first = repo
            .get_or_create_trace(
                &context(),
                project_id,
                &trace_external_id,
                Some(thread.id.clone()),
                "2026-08-15T00:00:00Z".into(),
            )
            .await?;
        let second = repo
            .get_or_create_trace(
                &context(),
                project_id,
                &trace_external_id,
                None,
                "2026-08-16T00:00:00Z".into(),
            )
            .await?;
        assert_eq!(first.id, second.id);
        assert_eq!(second.thread_id.as_deref(), Some(thread.id.as_str()));
        assert_eq!(second.created_at, first.created_at);
        assert!(repo.find_by_row_id(first.id.parse()?).await?.is_some());

        sqlx::query("DELETE FROM traces WHERE trace_id = $1")
            .bind(&trace_external_id)
            .execute(&pool)
            .await?;
        sqlx::query("DELETE FROM threads WHERE thread_id = $1")
            .bind(&thread_external_id)
            .execute(&pool)
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
