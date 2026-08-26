//! PostgreSQL-backed public-model repository.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::repo::model_repo::{
    CreateModelInput, ListModelsQuery, ListModelsResult, ModelRepo, UpdateModelInput,
};
use crate::repo::{RepoError, RepoResult, RequestContext};
use crate::row::ModelRow;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, name, status, developer, model_id, \
\"type\", icon, \"group\", model_card, settings, remark, created_at, updated_at, \
CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgModelRepo {
    pool: PgPool,
}
impl PgModelRepo {
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
        .map_err(|_| RepoError::NotFound("model id not a valid integer"))
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
        RepoError::Database(format!("postgres model repo {context} failed: {value}"))
    }
}

#[async_trait]
impl ModelRepo for PgModelRepo {
    async fn create_model_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateModelInput,
    ) -> RepoResult<ModelRow> {
        let inserted = sqlx::query_scalar::<_, i64>("INSERT INTO models (developer, model_id, \"type\", name, icon, \"group\", model_card, settings, status, remark) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'disabled',$9) RETURNING id")
            .bind(input.developer).bind(input.model_id).bind(input.model_type.unwrap_or_else(|| "chat".into()))
            .bind(input.name).bind(input.icon.unwrap_or_default()).bind(input.group)
            .bind(sqlx::types::Json(input.model_card.unwrap_or_default()))
            .bind(sqlx::types::Json(input.settings.unwrap_or_default())).bind(input.remark)
            .fetch_one(&self.pool).await.map_err(|e| error("create", e))?;
        self.find_model_unchecked(ctx, &inserted.to_string())
            .await?
            .ok_or(RepoError::NotFound("model"))
    }
    async fn find_model_unchecked(
        &self,
        _ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>> {
        sqlx::query_as::<_, ModelRow>(&format!(
            "SELECT {COLUMNS} FROM models WHERE id=$1 AND deleted_at=0"
        ))
        .bind(id(model_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| error("find", e))
    }
    async fn find_model_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>> {
        sqlx::query_as::<_, ModelRow>(&format!("SELECT {COLUMNS} FROM models WHERE id=$1"))
            .bind(id(model_id)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| error("find with deleted", e))
    }
    async fn find_model_by_name_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<ModelRow>> {
        sqlx::query_as::<_, ModelRow>(&format!(
            "SELECT {COLUMNS} FROM models WHERE name=$1 AND deleted_at=0"
        ))
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| error("find name", e))
    }
    async fn find_model_by_model_id_unchecked(
        &self,
        _ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<Option<ModelRow>> {
        sqlx::query_as::<_, ModelRow>(&format!(
            "SELECT {COLUMNS} FROM models WHERE model_id=$1 AND deleted_at=0"
        ))
        .bind(model_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| error("find provider id", e))
    }
    async fn list_models_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListModelsQuery,
    ) -> RepoResult<ListModelsResult> {
        let mut builder = QueryBuilder::<Postgres>::new(format!(
            "SELECT {COLUMNS} FROM models WHERE deleted_at=0"
        ));
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
            .push(" ORDER BY created_at,id LIMIT ")
            .push_bind(i64::from(query.limit) + 1)
            .push(" OFFSET ")
            .push_bind(i64::from(query.offset));
        let mut rows = builder
            .build_query_as::<ModelRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| error("list", e))?;
        let has_more = rows.len() > query.limit as usize;
        rows.truncate(query.limit as usize);
        Ok(ListModelsResult { rows, has_more })
    }
    async fn update_model_unchecked(
        &self,
        ctx: &RequestContext,
        model_id: &str,
        input: UpdateModelInput,
    ) -> RepoResult<ModelRow> {
        let model_id = id(model_id)?;
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE models SET ");
        let mut set = builder.separated(", ");
        if let Some(v) = input.developer {
            set.push("developer=").push_bind_unseparated(v);
        }
        if let Some(v) = input.model_id {
            set.push("model_id=").push_bind_unseparated(v);
        }
        if let Some(v) = input.name {
            set.push("name=").push_bind_unseparated(v);
        }
        if let Some(v) = input.model_type {
            set.push("\"type\"=").push_bind_unseparated(v);
        }
        if let Some(v) = input.icon {
            set.push("icon=")
                .push_bind_unseparated(v.unwrap_or_default());
        }
        if let Some(v) = input.group {
            set.push("\"group\"=").push_bind_unseparated(v);
        }
        if let Some(v) = input.model_card {
            set.push("model_card=")
                .push_bind_unseparated(sqlx::types::Json(v));
        }
        if let Some(v) = input.settings {
            set.push("settings=")
                .push_bind_unseparated(sqlx::types::Json(v));
        }
        if let Some(v) = input.remark {
            set.push("remark=").push_bind_unseparated(v);
        }
        if let Some(v) = input.status {
            set.push("status=").push_bind_unseparated(v);
        }
        set.push("updated_at=now()");
        drop(set);
        builder
            .push(" WHERE id=")
            .push_bind(model_id)
            .push(" AND deleted_at=0");
        if builder
            .build()
            .execute(&self.pool)
            .await
            .map_err(|e| error("update", e))?
            .rows_affected()
            == 0
        {
            return Err(RepoError::NotFound("model"));
        }
        self.find_model_unchecked(ctx, &model_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("model"))
    }
    async fn soft_delete_model_unchecked(
        &self,
        ctx: &RequestContext,
        model_id: &str,
        deleted_at: &str,
    ) -> RepoResult<ModelRow> {
        let model_id = id(model_id)?;
        let changed=sqlx::query("UPDATE models SET deleted_at=CAST(EXTRACT(EPOCH FROM $2::timestamptz) AS BIGINT),updated_at=$2,status='archived' WHERE id=$1 AND deleted_at=0")
            .bind(model_id).bind(timestamp(deleted_at)).execute(&self.pool).await.map_err(|e| error("soft delete",e))?.rows_affected();
        if changed == 0 {
            return Err(RepoError::NotFound("model"));
        }
        self.find_model_with_deleted_unchecked(ctx, &model_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("model"))
    }
    async fn restore_model_unchecked(
        &self,
        ctx: &RequestContext,
        model_id: &str,
    ) -> RepoResult<ModelRow> {
        let model_id = id(model_id)?;
        let changed = sqlx::query(
            "UPDATE models SET deleted_at=0,updated_at=now(),status='disabled' WHERE id=$1",
        )
        .bind(model_id)
        .execute(&self.pool)
        .await
        .map_err(|e| error("restore", e))?
        .rows_affected();
        if changed == 0 {
            return Err(RepoError::NotFound("model"));
        }
        self.find_model_unchecked(ctx, &model_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("model"))
    }
    async fn model_exists_unchecked(&self, _ctx: &RequestContext, name: &str) -> RepoResult<bool> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM models WHERE name=$1 AND deleted_at=0)",
        )
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| error("exists", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{PolicyContext, Principal};

    #[tokio::test]
    async fn postgres_model_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let repo = PgModelRepo::new(database.pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let row = repo
            .create_model(
                &ctx,
                CreateModelInput {
                    id: "ignored".into(),
                    developer: "mock".into(),
                    model_id: "mock-chat".into(),
                    name: "Mock Chat".into(),
                    model_type: Some("chat".into()),
                    icon: None,
                    group: "mock".into(),
                    model_card: Some(serde_json::json!({})),
                    settings: Some(serde_json::json!({})),
                    remark: None,
                    created_at: String::new(),
                },
            )
            .await?;
        let found = repo
            .find_model_by_model_id(&ctx, "mock-chat")
            .await?
            .ok_or("created model was not found")?;
        assert_eq!(found.id, row.id);
        let updated = repo
            .update_model(
                &ctx,
                &row.id,
                UpdateModelInput {
                    name: Some("Mock Chat 2".into()),
                    status: Some("enabled".into()),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.status, "enabled");
        assert_eq!(
            repo.soft_delete_model(&ctx, &row.id, "2026-08-15T00:00:00Z")
                .await?
                .status,
            "archived"
        );
        assert_eq!(repo.restore_model(&ctx, &row.id).await?.status, "disabled");
        database.cleanup().await?;
        Ok(())
    }
}
