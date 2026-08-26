//! PostgreSQL-backed role repository.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::repo::role_repo::{
    CreateRoleInput, ListRolesQuery, ListRolesResult, RoleRepo, UpdateRoleInput,
};
use crate::repo::{RepoError, RepoResult, RequestContext};
use crate::row::RoleRow;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, name, level, \
COALESCE(CAST(project_id AS TEXT), '') AS project_id, scopes, \
CASE WHEN deleted_at = 0 THEN 'active' ELSE 'deactivated' END AS status, \
created_at, updated_at, CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgRoleRepo {
    pool: PgPool,
}

impl PgRoleRepo {
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
        .map_err(|_| RepoError::NotFound("role/project id not a valid integer"))
}
fn timestamp(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or_default()
}
fn error(context: &str, value: sqlx::Error) -> RepoError {
    if value
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|c| c == "23505")
    {
        RepoError::NameConflict
    } else {
        RepoError::Database(format!("postgres role repo {context} failed: {value}"))
    }
}

#[async_trait]
impl RoleRepo for PgRoleRepo {
    async fn create_role_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateRoleInput,
    ) -> RepoResult<RoleRow> {
        // PostgreSQL UNIQUE treats NULL project ids as distinct, so the index
        // alone cannot protect system-role names.
        if self
            .role_name_exists_unchecked(ctx, &input.project_id, &input.name)
            .await?
        {
            return Err(RepoError::NameConflict);
        }
        let project_id = if input.project_id.is_empty() {
            None
        } else {
            Some(id(&input.project_id)?)
        };
        let inserted = sqlx::query_scalar::<_, i64>(
            "INSERT INTO roles (name, level, project_id, scopes) VALUES ($1, $2, $3, $4) RETURNING id")
            .bind(&input.name).bind(&input.level).bind(project_id)
            .bind(sqlx::types::Json(input.scopes)).fetch_one(&self.pool).await
            .map_err(|e| error("create", e))?;
        self.find_role_unchecked(ctx, &inserted.to_string())
            .await?
            .ok_or(RepoError::NotFound("role"))
    }

    async fn find_role_unchecked(
        &self,
        _ctx: &RequestContext,
        role_id: &str,
    ) -> RepoResult<Option<RoleRow>> {
        sqlx::query_as::<_, RoleRow>(&format!(
            "SELECT {COLUMNS} FROM roles WHERE id = $1 AND deleted_at = 0"
        ))
        .bind(id(role_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| error("find", e))
    }
    async fn find_role_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        role_id: &str,
    ) -> RepoResult<Option<RoleRow>> {
        sqlx::query_as::<_, RoleRow>(&format!("SELECT {COLUMNS} FROM roles WHERE id = $1"))
            .bind(id(role_id)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| error("find with deleted", e))
    }
    async fn list_system_roles_unchecked(&self, _ctx: &RequestContext) -> RepoResult<Vec<RoleRow>> {
        sqlx::query_as::<_, RoleRow>(&format!("SELECT {COLUMNS} FROM roles WHERE project_id IS NULL AND deleted_at = 0 ORDER BY created_at, id"))
            .fetch_all(&self.pool).await.map_err(|e| error("list system", e))
    }
    async fn list_roles_by_project_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<RoleRow>> {
        sqlx::query_as::<_, RoleRow>(&format!("SELECT {COLUMNS} FROM roles WHERE project_id = $1 AND deleted_at = 0 ORDER BY created_at, id"))
            .bind(id(project_id)?).fetch_all(&self.pool).await.map_err(|e| error("list project", e))
    }
    async fn list_roles_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListRolesQuery,
    ) -> RepoResult<ListRolesResult> {
        let mut builder = QueryBuilder::<Postgres>::new(format!(
            "SELECT {COLUMNS} FROM roles WHERE deleted_at = 0"
        ));
        if let Some(project_id) = &query.project_id {
            if project_id.is_empty() {
                builder.push(" AND project_id IS NULL");
            } else {
                builder
                    .push(" AND project_id = ")
                    .push_bind(id(project_id)?);
            }
        }
        if let (Some(at), Some(cursor_id)) = (&query.after_created_at, &query.after_id) {
            builder
                .push(" AND (created_at > ")
                .push_bind(timestamp(at))
                .push(" OR (created_at = ")
                .push_bind(timestamp(at))
                .push(" AND id > ")
                .push_bind(id(cursor_id)?)
                .push("))");
        }
        builder
            .push(" ORDER BY created_at, id LIMIT ")
            .push_bind(i64::from(query.limit) + 1)
            .push(" OFFSET ")
            .push_bind(i64::from(query.offset));
        let mut rows = builder
            .build_query_as::<RoleRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| error("list", e))?;
        let has_more = rows.len() > query.limit as usize;
        rows.truncate(query.limit as usize);
        Ok(ListRolesResult { rows, has_more })
    }
    async fn update_role_unchecked(
        &self,
        ctx: &RequestContext,
        role_id: &str,
        input: UpdateRoleInput,
    ) -> RepoResult<RoleRow> {
        let role_id = id(role_id)?;
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE roles SET ");
        let mut set = builder.separated(", ");
        if let Some(name) = input.name {
            set.push("name = ").push_bind_unseparated(name);
        }
        if let Some(scopes) = input.scopes {
            set.push("scopes = ")
                .push_bind_unseparated(sqlx::types::Json(scopes));
        }
        set.push("updated_at = now()");
        drop(set);
        builder
            .push(" WHERE id = ")
            .push_bind(role_id)
            .push(" AND deleted_at = 0");
        let changed = builder
            .build()
            .execute(&self.pool)
            .await
            .map_err(|e| error("update", e))?
            .rows_affected();
        if changed == 0 {
            return Err(RepoError::NotFound("role"));
        }
        self.find_role_unchecked(ctx, &role_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("role"))
    }
    async fn soft_delete_role_unchecked(
        &self,
        ctx: &RequestContext,
        role_id: &str,
        deleted_at: &str,
    ) -> RepoResult<RoleRow> {
        let role_id = id(role_id)?;
        let changed = sqlx::query("UPDATE roles SET deleted_at = CAST(EXTRACT(EPOCH FROM $2::timestamptz) AS BIGINT), updated_at = $2 WHERE id = $1 AND deleted_at = 0")
            .bind(role_id).bind(timestamp(deleted_at)).execute(&self.pool).await.map_err(|e| error("soft delete", e))?.rows_affected();
        if changed == 0 {
            return Err(RepoError::NotFound("role"));
        }
        self.find_role_with_deleted_unchecked(ctx, &role_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("role"))
    }
    async fn restore_role_unchecked(
        &self,
        ctx: &RequestContext,
        role_id: &str,
    ) -> RepoResult<RoleRow> {
        let role_id = id(role_id)?;
        let changed =
            sqlx::query("UPDATE roles SET deleted_at = 0, updated_at = now() WHERE id = $1")
                .bind(role_id)
                .execute(&self.pool)
                .await
                .map_err(|e| error("restore", e))?
                .rows_affected();
        if changed == 0 {
            return Err(RepoError::NotFound("role"));
        }
        self.find_role_unchecked(ctx, &role_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("role"))
    }
    async fn role_name_exists_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        name: &str,
    ) -> RepoResult<bool> {
        let count = if project_id.is_empty() {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM roles WHERE project_id IS NULL AND name = $1 AND deleted_at = 0").bind(name).fetch_one(&self.pool).await
        } else {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM roles WHERE project_id = $1 AND name = $2 AND deleted_at = 0").bind(id(project_id)?).bind(name).fetch_one(&self.pool).await
        }.map_err(|e| error("exists", e))?;
        Ok(count > 0)
    }
    async fn role_name_exists_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        name: &str,
    ) -> RepoResult<bool> {
        let count = if project_id.is_empty() {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM roles WHERE project_id IS NULL AND name = $1",
            )
            .bind(name)
            .fetch_one(&self.pool)
            .await
        } else {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM roles WHERE project_id = $1 AND name = $2",
            )
            .bind(id(project_id)?)
            .bind(name)
            .fetch_one(&self.pool)
            .await
        }
        .map_err(|e| error("exists with deleted", e))?;
        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};

    fn input(name: &str, project_id: &str) -> CreateRoleInput {
        CreateRoleInput {
            id: "ignored".into(),
            name: name.into(),
            level: if project_id.is_empty() {
                "system"
            } else {
                "project"
            }
            .into(),
            project_id: project_id.into(),
            scopes: vec!["read_models".into()],
            created_at: String::new(),
        }
    }

    #[tokio::test]
    async fn postgres_role_lifecycle_when_dsn_is_provided() -> Result<(), Box<dyn std::error::Error>>
    {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let repo = PgRoleRepo::new(database.pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let suffix = chrono::Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
            .to_string();
        let administrator = format!("Administrator {suffix}");
        let developer = format!("Developer {suffix}");
        let operator = format!("Operator {suffix}");
        let project_id = chrono::Utc::now().timestamp_micros().to_string();

        let system = repo.create_role(&ctx, input(&administrator, "")).await?;
        assert!(
            repo.list_system_roles(&ctx)
                .await?
                .iter()
                .any(|candidate| candidate.id == system.id)
        );
        assert!(matches!(
            repo.create_role(&ctx, input(&administrator, "")).await,
            Err(RepoError::NameConflict)
        ));

        let project = repo
            .create_role(&ctx, input(&developer, &project_id))
            .await?;
        let updated = repo
            .update_role(
                &ctx,
                &project.id,
                UpdateRoleInput {
                    name: Some(operator.clone()),
                    scopes: Some(vec!["read_channels".into()]),
                    updated_at: String::new(),
                },
            )
            .await?;
        assert_eq!(updated.name, operator);
        let deleted = repo
            .soft_delete_role(&ctx, &system.id, "2026-08-15T00:00:00Z")
            .await?;
        assert_eq!(deleted.status, "deactivated");
        assert_eq!(repo.restore_role(&ctx, &system.id).await?.status, "active");
        repo.soft_delete_role(&ctx, &system.id, "2026-08-15T00:00:01Z")
            .await?;
        repo.soft_delete_role(&ctx, &project.id, "2026-08-15T00:00:01Z")
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
