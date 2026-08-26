//! PostgreSQL-backed OIDC identity reads.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::repo::{OidcRepo, RepoError, RepoResult, RequestContext};
use crate::row::OidcIdentityRow;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, issuer, subject, email, idp_name, \
last_login_at, CAST(user_id AS TEXT) AS user_id, created_at, updated_at, \
NULL::timestamptz AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgOidcRepo {
    pool: PgPool,
}

impl PgOidcRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn id(value: &str, label: &'static str) -> RepoResult<i64> {
    value.parse().map_err(|_| RepoError::NotFound(label))
}

fn database_error(operation: &str, error: sqlx::Error) -> RepoError {
    RepoError::Database(format!("postgres oidc repo {operation} failed: {error}"))
}

#[async_trait]
impl OidcRepo for PgOidcRepo {
    async fn find_oidc_identity_unchecked(
        &self,
        _ctx: &RequestContext,
        identity_id: &str,
    ) -> RepoResult<Option<OidcIdentityRow>> {
        sqlx::query_as::<_, OidcIdentityRow>(&format!(
            "SELECT {COLUMNS} FROM oidc_identities WHERE id = $1 AND deleted_at = 0"
        ))
        .bind(id(identity_id, "oidc identity id not a valid integer")?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error("find", error))
    }

    async fn list_oidc_identities_by_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Vec<OidcIdentityRow>> {
        sqlx::query_as::<_, OidcIdentityRow>(&format!(
            "SELECT {COLUMNS} FROM oidc_identities \
             WHERE user_id = $1 AND deleted_at = 0 ORDER BY created_at DESC, id DESC"
        ))
        .bind(id(user_id, "oidc user id not a valid integer")?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error("list by user", error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use chrono::Utc;

    #[tokio::test]
    async fn postgres_oidc_repo_filters_soft_deleted_rows_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let repo = PgOidcRepo::new(pool.clone());
        let context = RequestContext::new(PolicyContext::new(Principal::system()));
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let issuer = format!("https://oidc-{suffix}.example.test");
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO oidc_identities \
             (issuer, subject, email, idp_name, user_id) \
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(&issuer)
        .bind("subject")
        .bind("oidc@example.test")
        .bind("test")
        .bind(92000001_i64)
        .fetch_one(&pool)
        .await?;

        let found = repo
            .find_oidc_identity(&context, &id.to_string())
            .await?
            .ok_or("identity not found")?;
        assert_eq!(found.issuer, issuer);
        assert_eq!(
            repo.list_oidc_identities_by_user(&context, "92000001")
                .await?
                .len(),
            1
        );
        sqlx::query("UPDATE oidc_identities SET deleted_at = 1 WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await?;
        assert!(
            repo.find_oidc_identity(&context, &id.to_string())
                .await?
                .is_none()
        );

        sqlx::query("DELETE FROM oidc_identities WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
