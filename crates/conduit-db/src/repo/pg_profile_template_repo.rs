//! PostgreSQL-backed API-key profile-template repository.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::repo::profile_template_repo::{
    ApiKeyProfileTemplateRow, CreateProfileTemplateInput, ProfileTemplateRepo,
    UpdateProfileTemplateInput,
};
use crate::repo::{RepoError, RepoResult, RequestContext};

const COLUMNS: &str = "\
CAST(id AS TEXT) AS id, CAST(project_id AS TEXT) AS project_id, name, description, profile, \
created_at, updated_at, \
CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgProfileTemplateRepo {
    pool: PgPool,
}

impl PgProfileTemplateRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn parse_id(value: &str) -> RepoResult<i64> {
    value
        .parse()
        .map_err(|_| RepoError::NotFound("profile template id not a valid integer"))
}

fn parse_project_id(value: &str) -> RepoResult<i64> {
    value
        .parse()
        .map_err(|_| RepoError::NotFound("profile template project id not a valid integer"))
}

fn parse_timestamp(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| DateTime::from_timestamp(0, 0).unwrap_or_default())
}

fn default_profile() -> serde_json::Value {
    serde_json::json!({"name": "", "modelMappings": null})
}

fn db_error(context: &str, error: sqlx::Error) -> RepoError {
    RepoError::Database(format!(
        "postgres profile template repo {context} failed: {error}"
    ))
}

fn is_unique(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code == "23505")
}

#[async_trait]
impl ProfileTemplateRepo for PgProfileTemplateRepo {
    async fn create_profile_template_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateProfileTemplateInput,
    ) -> RepoResult<ApiKeyProfileTemplateRow> {
        let project_id = parse_project_id(&input.project_id)?;
        let created_at = parse_timestamp(&input.created_at);
        let profile = input.profile.unwrap_or_else(default_profile);
        let row = sqlx::query_as::<_, ApiKeyProfileTemplateRow>(&format!(
            "INSERT INTO api_key_profile_templates \
             (project_id, name, description, profile, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $5) RETURNING {COLUMNS}"
        ))
        .bind(project_id)
        .bind(input.name)
        .bind(input.description.unwrap_or_default())
        .bind(sqlx::types::Json(profile))
        .bind(created_at)
        .fetch_one(&self.pool)
        .await;

        match row {
            Ok(row) => Ok(row),
            Err(error) if is_unique(&error) => Err(RepoError::NameConflict),
            Err(error) => Err(db_error("create", error)),
        }
    }

    async fn find_profile_template_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
    ) -> RepoResult<Option<ApiKeyProfileTemplateRow>> {
        let Ok(template_id) = template_id.parse::<i64>() else {
            return Ok(None);
        };
        sqlx::query_as::<_, ApiKeyProfileTemplateRow>(&format!(
            "SELECT {COLUMNS} FROM api_key_profile_templates \
             WHERE project_id = $1 AND id = $2 AND deleted_at = 0"
        ))
        .bind(parse_project_id(project_id)?)
        .bind(template_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| db_error("find", error))
    }

    async fn find_profile_template_by_id_unchecked(
        &self,
        _ctx: &RequestContext,
        template_id: &str,
    ) -> RepoResult<Option<ApiKeyProfileTemplateRow>> {
        let Ok(template_id) = template_id.parse::<i64>() else {
            return Ok(None);
        };
        sqlx::query_as::<_, ApiKeyProfileTemplateRow>(&format!(
            "SELECT {COLUMNS} FROM api_key_profile_templates \
             WHERE id = $1 AND deleted_at = 0"
        ))
        .bind(template_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| db_error("global find", error))
    }

    async fn find_profile_template_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
    ) -> RepoResult<Option<ApiKeyProfileTemplateRow>> {
        let Ok(template_id) = template_id.parse::<i64>() else {
            return Ok(None);
        };
        sqlx::query_as::<_, ApiKeyProfileTemplateRow>(&format!(
            "SELECT {COLUMNS} FROM api_key_profile_templates \
             WHERE project_id = $1 AND id = $2"
        ))
        .bind(parse_project_id(project_id)?)
        .bind(template_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| db_error("find with deleted", error))
    }

    async fn list_profile_templates_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<ApiKeyProfileTemplateRow>> {
        sqlx::query_as::<_, ApiKeyProfileTemplateRow>(&format!(
            "SELECT {COLUMNS} FROM api_key_profile_templates \
             WHERE project_id = $1 AND deleted_at = 0 ORDER BY id"
        ))
        .bind(parse_project_id(project_id)?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| db_error("list", error))
    }

    async fn list_all_profile_templates_unchecked(
        &self,
        _ctx: &RequestContext,
    ) -> RepoResult<Vec<ApiKeyProfileTemplateRow>> {
        sqlx::query_as::<_, ApiKeyProfileTemplateRow>(&format!(
            "SELECT {COLUMNS} FROM api_key_profile_templates \
             WHERE deleted_at = 0 ORDER BY id"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(|error| db_error("global list", error))
    }

    async fn update_profile_template_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
        input: UpdateProfileTemplateInput,
    ) -> RepoResult<ApiKeyProfileTemplateRow> {
        let project_id = parse_project_id(project_id)?;
        let template_id = parse_id(template_id)?;
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE api_key_profile_templates SET ");
        let mut set = builder.separated(", ");
        if let Some(value) = input.name {
            set.push("name = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.description {
            set.push("description = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.profile {
            set.push("profile = ")
                .push_bind_unseparated(sqlx::types::Json(value));
        }
        set.push("updated_at = ")
            .push_bind_unseparated(parse_timestamp(&input.updated_at));
        drop(set);
        builder
            .push(" WHERE project_id = ")
            .push_bind(project_id)
            .push(" AND id = ")
            .push_bind(template_id)
            .push(" AND deleted_at = 0");

        match builder.build().execute(&self.pool).await {
            Ok(result) if result.rows_affected() == 0 => {
                return Err(RepoError::NotFound("profile template"));
            }
            Ok(_) => {}
            Err(error) if is_unique(&error) => return Err(RepoError::NameConflict),
            Err(error) => return Err(db_error("update", error)),
        }

        self.find_profile_template_unchecked(ctx, &project_id.to_string(), &template_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("profile template"))
    }

    async fn soft_delete_profile_template_unchecked(
        &self,
        ctx: &RequestContext,
        project_id: &str,
        template_id: &str,
        _deleted_at: String,
    ) -> RepoResult<ApiKeyProfileTemplateRow> {
        let project_id = parse_project_id(project_id)?;
        let template_id = parse_id(template_id)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| db_error("delete begin", error))?;

        let name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM api_key_profile_templates \
             WHERE project_id = $1 AND id = $2 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(project_id)
        .bind(template_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| db_error("delete lookup", error))?
        .ok_or(RepoError::NotFound("profile template"))?;

        let max_deleted_at = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(deleted_at), 0) FROM api_key_profile_templates \
             WHERE project_id = $1 AND name = $2",
        )
        .bind(project_id)
        .bind(&name)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| db_error("delete max timestamp", error))?;
        let deleted_at = Utc::now().timestamp().max(max_deleted_at.saturating_add(1));

        let affected = sqlx::query(
            "UPDATE api_key_profile_templates SET deleted_at = $1, updated_at = now() \
             WHERE project_id = $2 AND id = $3 AND deleted_at = 0",
        )
        .bind(deleted_at)
        .bind(project_id)
        .bind(template_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| db_error("delete", error))?
        .rows_affected();
        if affected == 0 {
            return Err(RepoError::NotFound("profile template"));
        }
        transaction
            .commit()
            .await
            .map_err(|error| db_error("delete commit", error))?;

        self.find_profile_template_with_deleted_unchecked(
            ctx,
            &project_id.to_string(),
            &template_id.to_string(),
        )
        .await?
        .ok_or(RepoError::NotFound("profile template"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyContext, Principal};

    #[tokio::test]
    async fn postgres_profile_template_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let repo = PgProfileTemplateRepo::new(pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let suffix = std::process::id();
        let name = format!("pg-profile-template-{suffix}");
        sqlx::query("DELETE FROM api_key_profile_templates WHERE name = $1")
            .bind(&name)
            .execute(&pool)
            .await?;

        let created = repo
            .create_profile_template(
                &ctx,
                CreateProfileTemplateInput {
                    project_id: "1".to_string(),
                    name: name.clone(),
                    description: Some("postgres template".to_string()),
                    profile: Some(serde_json::json!({"name": "Default"})),
                    created_at: Utc::now().to_rfc3339(),
                },
            )
            .await?;
        assert_eq!(
            created.profile,
            Some(serde_json::json!({"name": "Default"}))
        );
        let listed = repo.list_profile_templates(&ctx, "1").await?;
        assert!(listed.iter().any(|row| row.id == created.id));

        let deleted = repo
            .soft_delete_profile_template(&ctx, "1", &created.id, Utc::now().to_rfc3339())
            .await?;
        assert!(deleted.deleted_at.is_some());
        assert!(
            repo.find_profile_template(&ctx, "1", &created.id)
                .await?
                .is_none()
        );
        sqlx::query("DELETE FROM api_key_profile_templates WHERE id = $1")
            .bind(created.id.parse::<i64>()?)
            .execute(&pool)
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
