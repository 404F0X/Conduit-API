//! PostgreSQL-backed thread repository.

use crate::repo::{RepoError, RepoResult, RequestContext, ThreadRepo};
use crate::row::ThreadRow;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, CAST(project_id AS TEXT) AS project_id, \
thread_id, created_at, updated_at";

#[derive(Debug, Clone)]
pub struct PgThreadRepo {
    pool: PgPool,
}

impl PgThreadRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Global admin listing used by `Query.threads`.
    pub async fn list_all(&self) -> RepoResult<Vec<ThreadRow>> {
        sqlx::query_as::<_, ThreadRow>(&format!("SELECT {COLUMNS} FROM threads ORDER BY id ASC"))
            .fetch_all(&self.pool)
            .await
            .map_err(|error| database_error("list all", error))
    }

    /// Primary-key lookup used by the Relay `node(id:)` resolver.
    pub async fn find_by_row_id(&self, id: i64) -> RepoResult<Option<ThreadRow>> {
        sqlx::query_as::<_, ThreadRow>(&format!("SELECT {COLUMNS} FROM threads WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| database_error("find by row id", error))
    }
}

fn parse_id(value: &str, label: &'static str) -> RepoResult<i64> {
    value.parse().map_err(|_| RepoError::NotFound(label))
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
    RepoError::Database(format!("postgres thread repo {operation} failed: {error}"))
}

async fn fetch_by_pair(
    pool: &PgPool,
    project_id: i64,
    thread_id: &str,
) -> RepoResult<Option<ThreadRow>> {
    sqlx::query_as::<_, ThreadRow>(&format!(
        "SELECT {COLUMNS} FROM threads WHERE project_id = $1 AND thread_id = $2"
    ))
    .bind(project_id)
    .bind(thread_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| database_error("find by project and thread id", error))
}

#[async_trait]
impl ThreadRepo for PgThreadRepo {
    async fn get_or_create_thread_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        thread_id: &str,
        now: String,
    ) -> RepoResult<ThreadRow> {
        let project_id = parse_id(project_id, "thread project id not a valid integer")?;
        let now = parse_timestamp(&now);

        // The schema deliberately keeps the Go global uniqueness rule on
        // `thread_id`. `ON CONFLICT DO NOTHING` makes same-project races
        // first-write-wins; the scoped read below prevents leaking a row that
        // belongs to another project when an external id collides globally.
        sqlx::query(
            "INSERT INTO threads (project_id, thread_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $3) ON CONFLICT (thread_id) DO NOTHING",
        )
        .bind(project_id)
        .bind(thread_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|error| database_error("get or create insert", error))?;

        fetch_by_pair(&self.pool, project_id, thread_id)
            .await?
            .ok_or(RepoError::NotFound("thread"))
    }

    async fn find_thread_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        thread_id: &str,
    ) -> RepoResult<Option<ThreadRow>> {
        fetch_by_pair(
            &self.pool,
            parse_id(project_id, "thread project id not a valid integer")?,
            thread_id,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use std::sync::Arc;

    fn context() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn suffix() -> String {
        format!(
            "{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        )
    }

    #[tokio::test]
    async fn postgres_thread_get_or_create_is_atomic_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let repo = Arc::new(PgThreadRepo::new(pool.clone()));
        let external_id = format!("pg-thread-{}", suffix());
        let project_id = "91000001";

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let repo = repo.clone();
            let external_id = external_id.clone();
            tasks.push(tokio::spawn(async move {
                repo.get_or_create_thread(
                    &context(),
                    project_id,
                    &external_id,
                    "2026-08-15T00:00:00Z".into(),
                )
                .await
            }));
        }
        let mut ids = Vec::new();
        for task in tasks {
            ids.push(task.await??.id);
        }
        assert!(ids.windows(2).all(|pair| pair[0] == pair[1]));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM threads WHERE thread_id = $1")
            .bind(&external_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 1);

        sqlx::query("DELETE FROM threads WHERE thread_id = $1")
            .bind(&external_id)
            .execute(&pool)
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
