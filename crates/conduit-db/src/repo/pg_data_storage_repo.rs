//! PostgreSQL-backed data-storage repository.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::repo::data_storage_repo::{
    CreateDataStorageInput, DataStorageRepo, ListDataStoragesQuery, ListDataStoragesResult,
    UpdateDataStorageInput,
};
use crate::repo::{RepoError, RepoResult, RequestContext};
use crate::row::DataStorageRow;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, name, status, description, \"primary\", \
\"type\", settings, created_at, updated_at, \
CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgDataStorageRepo {
    pool: PgPool,
}
impl PgDataStorageRepo {
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
        .map_err(|_| RepoError::NotFound("data storage id not a valid integer"))
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
        RepoError::Database(format!(
            "postgres data storage repo {context} failed: {value}"
        ))
    }
}

#[async_trait]
impl DataStorageRepo for PgDataStorageRepo {
    async fn create_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
        input: CreateDataStorageInput,
    ) -> RepoResult<DataStorageRow> {
        let inserted = sqlx::query_scalar::<_, i64>("INSERT INTO data_storages (name, description, \"primary\", \"type\", settings, status) VALUES ($1, $2, $3, $4, $5, 'active') RETURNING id")
            .bind(input.name).bind(input.description).bind(input.primary)
            .bind(input.storage_type.unwrap_or_else(|| "database".into()))
            .bind(sqlx::types::Json(input.settings.unwrap_or_default()))
            .fetch_one(&self.pool).await.map_err(|e| error("create", e))?;
        self.find_data_storage_unchecked(ctx, &inserted.to_string())
            .await?
            .ok_or(RepoError::NotFound("data storage"))
    }
    async fn find_data_storage_unchecked(
        &self,
        _ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<Option<DataStorageRow>> {
        sqlx::query_as::<_, DataStorageRow>(&format!(
            "SELECT {COLUMNS} FROM data_storages WHERE id = $1 AND deleted_at = 0"
        ))
        .bind(id(storage_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| error("find", e))
    }
    async fn find_data_storage_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<Option<DataStorageRow>> {
        sqlx::query_as::<_, DataStorageRow>(&format!(
            "SELECT {COLUMNS} FROM data_storages WHERE id = $1"
        ))
        .bind(id(storage_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| error("find with deleted", e))
    }
    async fn find_data_storage_by_name_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<Option<DataStorageRow>> {
        sqlx::query_as::<_, DataStorageRow>(&format!(
            "SELECT {COLUMNS} FROM data_storages WHERE name = $1 AND deleted_at = 0"
        ))
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| error("find name", e))
    }
    async fn find_primary_data_storage_unchecked(
        &self,
        _ctx: &RequestContext,
    ) -> RepoResult<Option<DataStorageRow>> {
        sqlx::query_as::<_, DataStorageRow>(&format!("SELECT {COLUMNS} FROM data_storages WHERE \"primary\" = TRUE AND deleted_at = 0 ORDER BY id LIMIT 1"))
            .fetch_optional(&self.pool).await.map_err(|e| error("find primary", e))
    }
    async fn list_data_storages_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListDataStoragesQuery,
    ) -> RepoResult<ListDataStoragesResult> {
        let mut builder = QueryBuilder::<Postgres>::new(format!(
            "SELECT {COLUMNS} FROM data_storages WHERE deleted_at = 0"
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
            .push(" ORDER BY created_at, id LIMIT ")
            .push_bind(i64::from(query.limit) + 1)
            .push(" OFFSET ")
            .push_bind(i64::from(query.offset));
        let mut rows = builder
            .build_query_as::<DataStorageRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| error("list", e))?;
        let has_more = rows.len() > query.limit as usize;
        rows.truncate(query.limit as usize);
        Ok(ListDataStoragesResult { rows, has_more })
    }
    async fn update_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
        input: UpdateDataStorageInput,
    ) -> RepoResult<DataStorageRow> {
        let storage_id = id(storage_id)?;
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE data_storages SET ");
        let mut set = builder.separated(", ");
        if let Some(v) = input.name {
            set.push("name = ").push_bind_unseparated(v);
        }
        if let Some(v) = input.description {
            set.push("description = ").push_bind_unseparated(v);
        }
        if let Some(v) = input.storage_type {
            set.push("\"type\" = ").push_bind_unseparated(v);
        }
        if let Some(v) = input.settings {
            set.push("settings = ")
                .push_bind_unseparated(sqlx::types::Json(v));
        }
        if let Some(v) = input.status {
            set.push("status = ").push_bind_unseparated(v);
        }
        set.push("updated_at = now()");
        drop(set);
        builder
            .push(" WHERE id = ")
            .push_bind(storage_id)
            .push(" AND deleted_at = 0");
        if builder
            .build()
            .execute(&self.pool)
            .await
            .map_err(|e| error("update", e))?
            .rows_affected()
            == 0
        {
            return Err(RepoError::NotFound("data storage"));
        }
        self.find_data_storage_unchecked(ctx, &storage_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("data storage"))
    }
    async fn soft_delete_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
        deleted_at: &str,
    ) -> RepoResult<DataStorageRow> {
        let storage_id = id(storage_id)?;
        let changed = sqlx::query("UPDATE data_storages SET deleted_at = CAST(EXTRACT(EPOCH FROM $2::timestamptz) AS BIGINT), updated_at = $2, status = 'archived' WHERE id = $1 AND deleted_at = 0")
            .bind(storage_id).bind(timestamp(deleted_at)).execute(&self.pool).await.map_err(|e| error("soft delete", e))?.rows_affected();
        if changed == 0 {
            return Err(RepoError::NotFound("data storage"));
        }
        self.find_data_storage_with_deleted_unchecked(ctx, &storage_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("data storage"))
    }
    async fn restore_data_storage_unchecked(
        &self,
        ctx: &RequestContext,
        storage_id: &str,
    ) -> RepoResult<DataStorageRow> {
        let storage_id = id(storage_id)?;
        let changed = sqlx::query("UPDATE data_storages SET deleted_at = 0, updated_at = now(), status = 'active' WHERE id = $1")
            .bind(storage_id).execute(&self.pool).await.map_err(|e| error("restore", e))?.rows_affected();
        if changed == 0 {
            return Err(RepoError::NotFound("data storage"));
        }
        self.find_data_storage_unchecked(ctx, &storage_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("data storage"))
    }
    async fn data_storage_exists_unchecked(
        &self,
        _ctx: &RequestContext,
        name: &str,
    ) -> RepoResult<bool> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM data_storages WHERE name = $1 AND deleted_at = 0)",
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
    async fn postgres_data_storage_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let repo = PgDataStorageRepo::new(database.pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let row = repo
            .create_data_storage(
                &ctx,
                CreateDataStorageInput {
                    id: "ignored".into(),
                    name: "Primary".into(),
                    description: "db".into(),
                    primary: true,
                    storage_type: Some("database".into()),
                    settings: Some(serde_json::json!({"dsn":"hidden"})),
                    created_at: String::new(),
                },
            )
            .await?;
        assert!(repo.find_primary_data_storage(&ctx).await?.is_some());
        assert_eq!(
            repo.soft_delete_data_storage(&ctx, &row.id, "2026-08-15T00:00:00Z")
                .await?
                .status,
            "archived"
        );
        assert_eq!(
            repo.restore_data_storage(&ctx, &row.id).await?.status,
            "active"
        );
        database.cleanup().await?;
        Ok(())
    }
}
