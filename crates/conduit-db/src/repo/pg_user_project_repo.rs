//! PostgreSQL-backed user/project membership repository.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::repo::user_project_repo::{CreateUserProjectInput, UserProjectRepo};
use crate::repo::{RepoError, RepoResult, RequestContext};
use crate::row::UserProjectRow;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, CAST(user_id AS TEXT) AS user_id, \
CAST(project_id AS TEXT) AS project_id, is_owner, scopes, created_at, updated_at";

#[derive(Debug, Clone)]
pub struct PgUserProjectRepo {
    pool: PgPool,
}

impl PgUserProjectRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn id(value: &str) -> RepoResult<i64> {
    value
        .parse()
        .map_err(|_| RepoError::NotFound("user/project id not a valid integer"))
}

fn error(context: &str, value: sqlx::Error) -> RepoError {
    if value
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|c| c == "23505")
    {
        RepoError::NameConflict
    } else {
        RepoError::Database(format!(
            "postgres user project repo {context} failed: {value}"
        ))
    }
}

#[async_trait]
impl UserProjectRepo for PgUserProjectRepo {
    async fn create_user_project_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateUserProjectInput,
    ) -> RepoResult<UserProjectRow> {
        sqlx::query_as::<_, UserProjectRow>(&format!(
            "INSERT INTO user_projects (user_id, project_id, is_owner, scopes) \
             VALUES ($1, $2, $3, $4) RETURNING {COLUMNS}"
        ))
        .bind(id(&input.user_id)?)
        .bind(id(&input.project_id)?)
        .bind(input.is_owner)
        .bind(sqlx::types::Json(input.scopes))
        .fetch_one(&self.pool)
        .await
        .map_err(|e| error("create", e))
    }

    async fn list_user_projects_by_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
    ) -> RepoResult<Vec<UserProjectRow>> {
        sqlx::query_as::<_, UserProjectRow>(&format!(
            "SELECT {COLUMNS} FROM user_projects WHERE user_id = $1 ORDER BY id"
        ))
        .bind(id(user_id)?)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| error("list", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};

    #[tokio::test]
    async fn postgres_create_list_and_reject_duplicate_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let repo = PgUserProjectRepo::new(database.pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let seed = chrono::Utc::now().timestamp_micros().to_string();
        let input = CreateUserProjectInput {
            id: "ignored".into(),
            user_id: seed.clone(),
            project_id: seed.clone(),
            is_owner: true,
            scopes: vec!["read".into()],
            created_at: String::new(),
        };
        let row = repo.create_user_project(&ctx, input.clone()).await?;
        assert!(row.is_owner);
        assert_eq!(repo.list_user_projects_by_user(&ctx, &seed).await?.len(), 1);
        assert!(matches!(
            repo.create_user_project(&ctx, input).await,
            Err(RepoError::NameConflict)
        ));
        sqlx::query("DELETE FROM user_projects WHERE id = $1")
            .bind(row.id.parse::<i64>()?)
            .execute(repo.pool())
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
