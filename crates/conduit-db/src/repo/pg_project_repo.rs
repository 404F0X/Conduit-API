//! PostgreSQL-backed project repository.

use async_trait::async_trait;
use chrono::Utc;
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::repo::project_repo::{
    CreateProjectInput, ListProjectsQuery, ListProjectsResult, UpdateProjectInput,
};
use crate::repo::{ProjectRepo, RepoError, RepoResult, RequestContext};
use crate::row::ProjectRow;

const COLUMNS: &str = "\
CAST(id AS TEXT) AS id, name, status, description, COALESCE(profiles, '{}'::jsonb) AS profiles, \
created_at, updated_at, \
CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgProjectRepo {
    pool: PgPool,
}

impl PgProjectRepo {
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
        .map_err(|_| RepoError::NotFound("project id not a valid integer"))
}
fn error(context: &str, value: sqlx::Error) -> RepoError {
    RepoError::Database(format!("postgres project repo {context} failed: {value}"))
}

#[async_trait]
impl ProjectRepo for PgProjectRepo {
    async fn create_project_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateProjectInput,
    ) -> RepoResult<ProjectRow> {
        let result = sqlx::query("INSERT INTO projects (name, status, description, profiles) VALUES ($1, 'active', $2, '{}'::jsonb)")
            .bind(&input.name).bind(input.description.unwrap_or_default()).execute(&self.pool).await;
        if let Err(value) = result {
            if value
                .as_database_error()
                .and_then(|v| v.code())
                .is_some_and(|v| v == "23505")
            {
                return Err(RepoError::NameConflict);
            }
            return Err(error("create", value));
        }
        self.find_project_by_name_unchecked(ctx, &input.name)
            .await?
            .ok_or(RepoError::NotFound("project"))
    }

    async fn find_project_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Option<ProjectRow>> {
        sqlx::query_as::<_, ProjectRow>(&format!(
            "SELECT {COLUMNS} FROM projects WHERE id = $1 AND deleted_at = 0"
        ))
        .bind(id(project_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|v| error("find", v))
    }
    async fn find_project_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Option<ProjectRow>> {
        sqlx::query_as::<_, ProjectRow>(&format!("SELECT {COLUMNS} FROM projects WHERE id = $1"))
            .bind(id(project_id)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|v| error("find with deleted", v))
    }
    async fn find_project_by_name_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ProjectRow>> {
        sqlx::query_as::<_, ProjectRow>(&format!(
            "SELECT {COLUMNS} FROM projects WHERE name = $1 AND deleted_at = 0"
        ))
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|v| error("find name", v))
    }

    async fn list_projects_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListProjectsQuery,
    ) -> RepoResult<ListProjectsResult> {
        let fetch_n = i64::from(query.limit).saturating_add(1);
        let mut rows = if let (Some(at), Some(cursor_id)) =
            (query.after_created_at.as_deref(), query.after_id.as_deref())
        {
            let at = chrono::DateTime::parse_from_rfc3339(at)
                .map(|v| v.with_timezone(&Utc))
                .unwrap_or_default();
            sqlx::query_as::<_, ProjectRow>(&format!("SELECT {COLUMNS} FROM projects WHERE deleted_at = 0 AND (created_at > $1 OR (created_at = $1 AND id > $2)) ORDER BY created_at, id LIMIT $3"))
                .bind(at).bind(id(cursor_id)?).bind(fetch_n).fetch_all(&self.pool).await.map_err(|v| error("list keyset", v))?
        } else {
            sqlx::query_as::<_, ProjectRow>(&format!("SELECT {COLUMNS} FROM projects WHERE deleted_at = 0 ORDER BY created_at, id LIMIT $1 OFFSET $2"))
                .bind(fetch_n).bind(i64::from(query.offset)).fetch_all(&self.pool).await.map_err(|v| error("list offset", v))?
        };
        let has_more = rows.len() > query.limit as usize;
        rows.truncate(query.limit as usize);
        Ok(ListProjectsResult { rows, has_more })
    }

    async fn update_project_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        input: UpdateProjectInput,
    ) -> RepoResult<ProjectRow> {
        let project_id = id(project_id)?;
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE projects SET ");
        let mut set = builder.separated(", ");
        if let Some(v) = input.name {
            set.push("name = ").push_bind_unseparated(v);
        }
        if let Some(v) = input.description {
            set.push("description = ")
                .push_bind_unseparated(v.unwrap_or_default());
        }
        if let Some(v) = input.status {
            set.push("status = ").push_bind_unseparated(v);
        }
        if let Some(v) = input.profiles {
            set.push("profiles = ")
                .push_bind_unseparated(sqlx::types::Json(v));
        }
        set.push("updated_at = now()");
        drop(set);
        builder
            .push(" WHERE id = ")
            .push_bind(project_id)
            .push(" AND deleted_at = 0");
        match builder.build().execute(&self.pool).await {
            Ok(v) if v.rows_affected() == 0 => return Err(RepoError::NotFound("project")),
            Ok(_) => {}
            Err(v)
                if v.as_database_error()
                    .and_then(|e| e.code())
                    .is_some_and(|v| v == "23505") =>
            {
                return Err(RepoError::NameConflict);
            }
            Err(v) => return Err(error("update", v)),
        }
        self.find_project_unchecked(ctx, &project_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("project"))
    }

    async fn soft_delete_project_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        _deleted_at: &str,
    ) -> RepoResult<ProjectRow> {
        let project_id = id(project_id)?;
        let affected = sqlx::query("UPDATE projects SET deleted_at = $1, status = 'archived', updated_at = now() WHERE id = $2 AND deleted_at = 0")
            .bind(Utc::now().timestamp()).bind(project_id).execute(&self.pool).await.map_err(|v| error("delete", v))?.rows_affected();
        if affected == 0 {
            return Err(RepoError::NotFound("project"));
        }
        self.find_project_with_deleted_unchecked(ctx, &project_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("project"))
    }

    async fn restore_project_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<ProjectRow> {
        let project_id = id(project_id)?;
        let row = self
            .find_project_with_deleted_unchecked(ctx, &project_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("project"))?;
        let conflict: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE name = $1 AND deleted_at = 0 AND id <> $2)",
        )
        .bind(&row.name)
        .bind(project_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|v| error("restore pre-check", v))?;
        if conflict {
            return Err(RepoError::NameConflict);
        }
        sqlx::query("UPDATE projects SET deleted_at = 0, status = 'active', updated_at = now() WHERE id = $1")
            .bind(project_id).execute(&self.pool).await.map_err(|v| error("restore", v))?;
        self.find_project_unchecked(ctx, &project_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("project"))
    }

    async fn project_exists_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE name = $1 AND deleted_at = 0)",
        )
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .map_err(|v| error("exists", v))
    }
    async fn project_exists_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<bool> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE name = $1)")
            .bind(name)
            .fetch_one(&self.pool)
            .await
            .map_err(|v| error("exists with deleted", v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyContext, Principal};

    #[tokio::test]
    async fn live_postgres_project_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let repo = PgProjectRepo::new(pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let name = format!("pg-project-{}", std::process::id());
        sqlx::query("DELETE FROM projects WHERE name = $1")
            .bind(&name)
            .execute(&pool)
            .await?;
        let created = repo
            .create_project(
                &ctx,
                CreateProjectInput {
                    id: "ignored".into(),
                    name: name.clone(),
                    description: Some("test".into()),
                    created_at: Utc::now().to_rfc3339(),
                },
            )
            .await?;
        assert_eq!(created.profiles, serde_json::json!({}));
        let updated = repo
            .update_project(
                &ctx,
                &created.id,
                UpdateProjectInput {
                    profiles: Some(serde_json::json!({"mode":"simple"})),
                    updated_at: Utc::now().to_rfc3339(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.profiles["mode"], "simple");
        assert!(
            repo.soft_delete_project(&ctx, &created.id, "ignored")
                .await?
                .deleted_at
                .is_some()
        );
        assert!(repo.find_project_by_name(&ctx, &name).await?.is_none());
        assert!(
            repo.restore_project(&ctx, &created.id)
                .await?
                .deleted_at
                .is_none()
        );
        sqlx::query("DELETE FROM projects WHERE id = $1")
            .bind(created.id.parse::<i64>()?)
            .execute(&pool)
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
