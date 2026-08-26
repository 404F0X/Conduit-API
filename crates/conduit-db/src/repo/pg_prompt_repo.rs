//! PostgreSQL-backed prompt repository.

use async_trait::async_trait;
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::repo::prompt_repo::{CreatePromptInput, UpdatePromptInput};
use crate::repo::{PromptRepo, RepoError, RepoResult, RequestContext};
use crate::row::PromptRow;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, CAST(project_id AS TEXT) AS project_id, \
name, status, description, role, content, \"order\", settings, created_at, updated_at, \
CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgPromptRepo {
    pool: PgPool,
}

impl PgPromptRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Load a live prompt by its table-local Relay row id.
    ///
    /// Normal prompt operations are project-scoped. A Relay GUID already
    /// identifies one globally unique row in the `prompts` table, so the host
    /// node dispatcher uses this narrow lookup to recover that row without
    /// guessing an owning project id.
    pub async fn find_by_row_id(&self, prompt_id: i64) -> RepoResult<Option<PromptRow>> {
        sqlx::query_as::<_, PromptRow>(&format!(
            "SELECT {COLUMNS} FROM prompts WHERE id = $1 AND deleted_at = 0"
        ))
        .bind(prompt_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error("find by row id", error))
    }
}

fn parse_id(value: &str) -> RepoResult<i64> {
    value
        .parse()
        .map_err(|_| RepoError::NotFound("prompt id not a valid integer"))
}

fn parse_project_id(value: &str) -> RepoResult<i64> {
    value
        .parse()
        .map_err(|_| RepoError::NotFound("prompt project id not a valid integer"))
}

fn database_error(context: &str, error: sqlx::Error) -> RepoError {
    if error
        .as_database_error()
        .and_then(|error| error.code())
        .is_some_and(|code| code == "23505")
    {
        RepoError::NameConflict
    } else {
        RepoError::Database(format!("postgres prompt repo {context} failed: {error}"))
    }
}

#[async_trait]
impl PromptRepo for PgPromptRepo {
    async fn create_prompt_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreatePromptInput,
    ) -> RepoResult<PromptRow> {
        let settings = input
            .settings
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
        sqlx::query_as::<_, PromptRow>(&format!(
            "INSERT INTO prompts \
             (project_id, name, description, role, content, status, \"order\", settings) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             RETURNING {COLUMNS}"
        ))
        .bind(parse_project_id(&input.project_id)?)
        .bind(input.name)
        .bind(input.description.unwrap_or_default())
        .bind(input.role)
        .bind(input.content)
        .bind(input.status.unwrap_or_else(|| "disabled".into()))
        .bind(input.order.unwrap_or(0))
        .bind(sqlx::types::Json(settings))
        .fetch_one(&self.pool)
        .await
        .map_err(|error| database_error("create", error))
    }

    async fn find_prompt_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<Option<PromptRow>> {
        sqlx::query_as::<_, PromptRow>(&format!(
            "SELECT {COLUMNS} FROM prompts \
             WHERE project_id = $1 AND id = $2 AND deleted_at = 0"
        ))
        .bind(parse_project_id(project_id)?)
        .bind(parse_id(prompt_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error("find", error))
    }

    async fn find_prompt_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<Option<PromptRow>> {
        sqlx::query_as::<_, PromptRow>(&format!(
            "SELECT {COLUMNS} FROM prompts WHERE project_id = $1 AND id = $2"
        ))
        .bind(parse_project_id(project_id)?)
        .bind(parse_id(prompt_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error("find with deleted", error))
    }

    async fn list_prompts_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
    ) -> RepoResult<Vec<PromptRow>> {
        sqlx::query_as::<_, PromptRow>(&format!(
            "SELECT {COLUMNS} FROM prompts \
             WHERE project_id = $1 AND deleted_at = 0 \
             ORDER BY \"order\" ASC, id ASC"
        ))
        .bind(parse_project_id(project_id)?)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error("list", error))
    }

    async fn list_live_prompt_project_ids_unchecked(
        &self,
        _ctx: &RequestContext,
    ) -> RepoResult<Vec<String>> {
        let ids = sqlx::query_scalar::<_, i64>(
            "SELECT DISTINCT project_id FROM prompts \
             WHERE deleted_at = 0 ORDER BY project_id ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| database_error("list live project ids", error))?;
        Ok(ids.into_iter().map(|id| id.to_string()).collect())
    }

    async fn update_prompt_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        input: UpdatePromptInput,
    ) -> RepoResult<PromptRow> {
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE prompts SET ");
        let mut fields = builder.separated(", ");
        if let Some(value) = input.name {
            fields.push("name = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.description {
            fields.push("description = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.role {
            fields.push("role = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.content {
            fields.push("content = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.order {
            fields.push("\"order\" = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.status {
            fields.push("status = ").push_bind_unseparated(value);
        }
        if let Some(value) = input.settings {
            fields
                .push("settings = ")
                .push_bind_unseparated(sqlx::types::Json(value));
        }
        fields.push("updated_at = now()");
        drop(fields);
        builder
            .push(" WHERE project_id = ")
            .push_bind(parse_project_id(project_id)?)
            .push(" AND id = ")
            .push_bind(parse_id(prompt_id)?)
            .push(" AND deleted_at = 0 RETURNING ")
            .push(COLUMNS);

        builder
            .build_query_as::<PromptRow>()
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| database_error("update", error))?
            .ok_or(RepoError::NotFound("prompt"))
    }

    async fn set_prompt_status_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        status: String,
        _updated_at: String,
    ) -> RepoResult<PromptRow> {
        sqlx::query_as::<_, PromptRow>(&format!(
            "UPDATE prompts SET status = $3, updated_at = now() \
             WHERE project_id = $1 AND id = $2 AND deleted_at = 0 \
             RETURNING {COLUMNS}"
        ))
        .bind(parse_project_id(project_id)?)
        .bind(parse_id(prompt_id)?)
        .bind(status)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error("set status", error))?
        .ok_or(RepoError::NotFound("prompt"))
    }

    async fn soft_delete_prompt_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
        _deleted_at: String,
    ) -> RepoResult<PromptRow> {
        let project_id = parse_project_id(project_id)?;
        let prompt_id = parse_id(prompt_id)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| database_error("soft delete begin", error))?;

        let name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM prompts \
             WHERE project_id = $1 AND id = $2 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(project_id)
        .bind(prompt_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error("soft delete lock", error))?
        .ok_or(RepoError::NotFound("prompt"))?;

        let maximum = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(deleted_at), 0) FROM prompts \
             WHERE project_id = $1 AND name = $2",
        )
        .bind(project_id)
        .bind(name)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|error| database_error("soft delete timestamp lookup", error))?;
        let deleted_at = chrono::Utc::now()
            .timestamp()
            .max(1)
            .max(maximum.saturating_add(1));

        let row = sqlx::query_as::<_, PromptRow>(&format!(
            "UPDATE prompts SET deleted_at = $3, updated_at = now() \
             WHERE project_id = $1 AND id = $2 AND deleted_at = 0 \
             RETURNING {COLUMNS}"
        ))
        .bind(project_id)
        .bind(prompt_id)
        .bind(deleted_at)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error("soft delete", error))?
        .ok_or(RepoError::NotFound("prompt"))?;

        transaction
            .commit()
            .await
            .map_err(|error| database_error("soft delete commit", error))?;
        Ok(row)
    }

    async fn restore_prompt_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        prompt_id: &str,
    ) -> RepoResult<PromptRow> {
        let project_id = parse_project_id(project_id)?;
        let prompt_id = parse_id(prompt_id)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| database_error("restore begin", error))?;

        let current = sqlx::query_as::<_, PromptRow>(&format!(
            "SELECT {COLUMNS} FROM prompts \
             WHERE project_id = $1 AND id = $2 FOR UPDATE"
        ))
        .bind(project_id)
        .bind(prompt_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|error| database_error("restore lock", error))?
        .ok_or(RepoError::NotFound("prompt"))?;

        let row = if current.deleted_at.is_some() {
            sqlx::query_as::<_, PromptRow>(&format!(
                "UPDATE prompts SET deleted_at = 0, updated_at = now() \
                 WHERE project_id = $1 AND id = $2 AND deleted_at <> 0 \
                 RETURNING {COLUMNS}"
            ))
            .bind(project_id)
            .bind(prompt_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| database_error("restore", error))?
            .ok_or(RepoError::NotFound("prompt"))?
        } else {
            current
        };

        transaction
            .commit()
            .await
            .map_err(|error| database_error("restore commit", error))?;
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};
    use sqlx::postgres::PgPoolOptions;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct IsolatedPostgres {
        pool: PgPool,
        admin_pool: PgPool,
        schema: String,
    }

    impl IsolatedPostgres {
        async fn new(dsn: &str) -> Result<Self, Box<dyn std::error::Error>> {
            let admin_pool = PgPool::connect(dsn).await?;
            let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros();
            let schema = format!("conduit_prompt_{}_{}", std::process::id(), nonce);
            sqlx::query(&format!("CREATE SCHEMA \"{schema}\""))
                .execute(&admin_pool)
                .await?;
            let search_path = format!("SET search_path TO \"{schema}\"");
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .after_connect(move |connection, _| {
                    let search_path = search_path.clone();
                    Box::pin(async move {
                        sqlx::query(&search_path).execute(connection).await?;
                        Ok(())
                    })
                })
                .connect(dsn)
                .await?;
            crate::connection::migrate_postgres_with_flag(&pool, false).await?;
            Ok(Self {
                pool,
                admin_pool,
                schema,
            })
        }

        async fn cleanup(self) -> Result<(), sqlx::Error> {
            self.pool.close().await;
            sqlx::query(&format!("DROP SCHEMA \"{}\" CASCADE", self.schema))
                .execute(&self.admin_pool)
                .await?;
            self.admin_pool.close().await;
            Ok(())
        }
    }

    fn context() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn input(project_id: &str, name: &str, order: i64) -> CreatePromptInput {
        CreatePromptInput {
            id: "ignored".into(),
            project_id: project_id.into(),
            name: name.into(),
            description: Some("description".into()),
            role: "system".into(),
            content: "be helpful".into(),
            status: None,
            order: Some(order),
            settings: Some(serde_json::json!({"conditions": []})),
            created_at: "2024-01-01T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn postgres_prompt_repo_isolated_crud_and_project_semantics_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = IsolatedPostgres::new(&dsn).await?;
        let repo = PgPromptRepo::new(database.pool.clone());
        let ctx = context();

        let first = repo.create_prompt(&ctx, input("1", "shared", 20)).await?;
        assert_eq!(first.status, "disabled");
        assert_eq!(first.project_id, "1");

        let earlier = repo.create_prompt(&ctx, input("1", "earlier", 10)).await?;
        let other_project = repo.create_prompt(&ctx, input("2", "shared", 10)).await?;
        assert_eq!(other_project.project_id, "2");
        assert!(matches!(
            repo.create_prompt(&ctx, input("1", "shared", 30)).await,
            Err(RepoError::NameConflict)
        ));

        let listed = repo.list_prompts(&ctx, "1").await?;
        assert_eq!(
            listed.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            vec![earlier.id.as_str(), first.id.as_str()]
        );
        assert!(repo.find_prompt(&ctx, "2", &first.id).await?.is_none());

        let updated = repo
            .update_prompt(
                &ctx,
                "1",
                &first.id,
                UpdatePromptInput {
                    content: Some("updated".into()),
                    order: Some(5),
                    settings: Some(serde_json::json!({"conditions": ["x"]})),
                    updated_at: "2024-02-01T00:00:00Z".into(),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.content, "updated");
        assert_eq!(updated.order_val, 5);
        assert_eq!(updated.settings, serde_json::json!({"conditions": ["x"]}));

        let enabled = repo
            .set_prompt_status(
                &ctx,
                "1",
                &first.id,
                "enabled".into(),
                "2024-02-01T00:00:00Z".into(),
            )
            .await?;
        assert_eq!(enabled.status, "enabled");

        let deleted = repo
            .soft_delete_prompt(&ctx, "1", &first.id, "2024-03-01T00:00:00Z".into())
            .await?;
        assert!(deleted.deleted_at.is_some());
        assert!(repo.find_prompt(&ctx, "1", &first.id).await?.is_none());

        let replacement = repo.create_prompt(&ctx, input("1", "shared", 30)).await?;
        assert!(matches!(
            repo.restore_prompt(&ctx, "1", &first.id).await,
            Err(RepoError::NameConflict)
        ));
        repo.soft_delete_prompt(&ctx, "1", &replacement.id, "2024-03-02T00:00:00Z".into())
            .await?;
        let restored = repo.restore_prompt(&ctx, "1", &first.id).await?;
        assert!(restored.deleted_at.is_none());

        assert_eq!(
            repo.list_live_prompt_project_ids_unchecked(&ctx).await?,
            vec!["1", "2"]
        );
        database.cleanup().await?;
        Ok(())
    }
}
