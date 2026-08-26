//! PostgreSQL-backed system settings repository.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::repo::{RepoError, RepoResult, RequestContext, SystemRepo, SystemRow};

const SYSTEM_SELECT_COLUMNS: &str = "\
CAST(id AS TEXT) AS id, key, value, created_at, updated_at, \
CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgSystemRepo {
    pool: PgPool,
}

impl PgSystemRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl SystemRepo for PgSystemRepo {
    async fn get_system_value_unchecked(
        &self,
        _ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<SystemRow>> {
        sqlx::query_as::<_, SystemRow>(&format!(
            "SELECT {SYSTEM_SELECT_COLUMNS} FROM systems WHERE key = $1 AND deleted_at = 0"
        ))
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| RepoError::Database(format!("postgres system repo get failed: {error}")))
    }

    async fn set_system_value_unchecked(
        &self,
        _ctx: &RequestContext,
        row: SystemRow,
    ) -> RepoResult<SystemRow> {
        sqlx::query(
            "INSERT INTO systems (key, value, created_at, updated_at) VALUES ($1, $2, $3, $4) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, \
             updated_at = excluded.updated_at",
        )
        .bind(&row.key)
        .bind(&row.value)
        .bind(row.created_at)
        .bind(row.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            RepoError::Database(format!("postgres system repo set failed: {error}"))
        })?;

        sqlx::query_as::<_, SystemRow>(&format!(
            "SELECT {SYSTEM_SELECT_COLUMNS} FROM systems WHERE key = $1 AND deleted_at = 0"
        ))
        .bind(&row.key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| {
            RepoError::Database(format!("postgres system repo readback failed: {error}"))
        })?
        .ok_or(RepoError::NotFound("system"))
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::{PolicyContext, Principal};

    #[tokio::test]
    async fn live_postgres_system_repo_round_trip_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let repo = PgSystemRepo::new(pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let timestamp = Utc
            .timestamp_opt(1_700_000_000, 0)
            .single()
            .ok_or("test timestamp is invalid")?;
        let key = format!("pg-system-test-{}", std::process::id());
        let saved = repo
            .set_system_value(
                &ctx,
                SystemRow {
                    id: "0".into(),
                    key: key.clone(),
                    value: "first".into(),
                    created_at: timestamp,
                    updated_at: timestamp,
                    deleted_at: None,
                },
            )
            .await?;
        assert_eq!(saved.value, "first");
        let found = repo
            .get_system_value(&ctx, &key)
            .await?
            .ok_or("saved system value was not found")?;
        assert_eq!(found.id, saved.id);
        sqlx::query("DELETE FROM systems WHERE key = $1")
            .bind(key)
            .execute(&pool)
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
