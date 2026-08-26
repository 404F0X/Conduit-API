//! PostgreSQL-backed API-key repository.

use async_trait::async_trait;
use chrono::Utc;
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::repo::api_key_repo::{
    CreateApiKeyInput, ListApiKeysQuery, ListApiKeysResult, UpdateApiKeyInput,
};
use crate::repo::{ApiKeyRepo, RepoError, RepoResult, RequestContext};
use crate::row::ApiKeyRow;

const COLUMNS: &str = "\
CAST(id AS TEXT) AS id, name, status, CAST(project_id AS TEXT) AS project_id, \
CAST(user_id AS TEXT) AS user_id, key, \"type\", \
COALESCE(scopes, '[]'::jsonb) AS scopes, \
COALESCE(profiles, '{}'::jsonb) AS profiles, created_at, updated_at, \
CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgApiKeyRepo {
    pool: PgPool,
}

impl PgApiKeyRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    async fn list_internal(
        &self,
        query: &ListApiKeysQuery,
        forced_user_id: Option<i64>,
        forced_project_id: Option<i64>,
    ) -> RepoResult<ListApiKeysResult> {
        let mut builder = QueryBuilder::<Postgres>::new(format!(
            "SELECT {COLUMNS} FROM api_keys WHERE deleted_at = 0"
        ));
        let user_id =
            forced_user_id.or_else(|| query.user_id.as_deref().and_then(|v| v.parse().ok()));
        let project_id =
            forced_project_id.or_else(|| query.project_id.as_deref().and_then(|v| v.parse().ok()));
        if let Some(value) = user_id {
            builder.push(" AND user_id = ").push_bind(value);
        }
        if let Some(value) = project_id {
            builder.push(" AND project_id = ").push_bind(value);
        }
        if let (Some(at), Some(id)) = (query.after_created_at.as_deref(), query.after_id.as_deref())
        {
            let at = chrono::DateTime::parse_from_rfc3339(at)
                .map(|v| v.with_timezone(&Utc))
                .unwrap_or_default();
            builder
                .push(" AND (created_at > ")
                .push_bind(at)
                .push(" OR (created_at = ")
                .push_bind(at)
                .push(" AND id > ")
                .push_bind(parse_id(id)?)
                .push("))");
        }
        let fetch_n = i64::from(query.limit).saturating_add(1);
        builder
            .push(" ORDER BY created_at, id LIMIT ")
            .push_bind(fetch_n);
        if query.after_created_at.is_none() || query.after_id.is_none() {
            builder.push(" OFFSET ").push_bind(i64::from(query.offset));
        }
        let mut rows = builder
            .build_query_as::<ApiKeyRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| db_error("list", e))?;
        let has_more = rows.len() > query.limit as usize;
        rows.truncate(query.limit as usize);
        Ok(ListApiKeysResult { rows, has_more })
    }
}

fn parse_id(value: &str) -> RepoResult<i64> {
    value
        .parse()
        .map_err(|_| RepoError::NotFound("api_key id not a valid integer"))
}

fn db_error(context: &str, error: sqlx::Error) -> RepoError {
    RepoError::Database(format!("postgres api_key repo {context} failed: {error}"))
}

fn unique(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|v| v == "23505")
}

#[async_trait]
impl ApiKeyRepo for PgApiKeyRepo {
    async fn create_api_key_unchecked(
        &self,
        _ctx: &RequestContext,
        input: CreateApiKeyInput,
    ) -> RepoResult<ApiKeyRow> {
        let project_id = input
            .project_id
            .parse::<i64>()
            .map_err(|_| RepoError::NotFound("project"))?;
        let user_id = input
            .user_id
            .as_deref()
            .map(str::parse::<i64>)
            .transpose()
            .map_err(|_| RepoError::NotFound("user"))?;
        let mut tx = self.pool.begin().await.map_err(|e| db_error("begin", e))?;
        let key_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_keys WHERE key = $1)")
                .bind(&input.key)
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| db_error("key pre-check", e))?;
        let name_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_keys WHERE project_id = $1 AND name = $2 AND deleted_at = 0)")
            .bind(project_id).bind(&input.name).fetch_one(&mut *tx).await.map_err(|e| db_error("name pre-check", e))?;
        if key_exists || name_exists {
            return Err(RepoError::NameConflict);
        }
        let profiles = input.profiles.unwrap_or_else(|| serde_json::json!({}));
        let result = sqlx::query("INSERT INTO api_keys (user_id, project_id, key, name, \"type\", status, scopes, profiles) VALUES ($1, $2, $3, $4, $5, 'enabled', $6, $7)")
            .bind(user_id).bind(project_id).bind(&input.key).bind(&input.name).bind(&input.key_type)
            .bind(sqlx::types::Json(&input.scopes)).bind(sqlx::types::Json(&profiles)).execute(&mut *tx).await;
        if let Err(error) = result {
            if unique(&error) {
                return Err(RepoError::NameConflict);
            }
            return Err(db_error("create", error));
        }
        let row = sqlx::query_as::<_, ApiKeyRow>(&format!(
            "SELECT {COLUMNS} FROM api_keys WHERE key = $1"
        ))
        .bind(&input.key)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_error("create readback", e))?;
        tx.commit().await.map_err(|e| db_error("commit", e))?;
        Ok(row)
    }

    async fn find_api_key_by_id_unchecked(
        &self,
        _ctx: &RequestContext,
        id: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        sqlx::query_as::<_, ApiKeyRow>(&format!(
            "SELECT {COLUMNS} FROM api_keys WHERE id = $1 AND deleted_at = 0"
        ))
        .bind(parse_id(id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("find id", e))
    }
    async fn find_api_key_by_id_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        id: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        sqlx::query_as::<_, ApiKeyRow>(&format!("SELECT {COLUMNS} FROM api_keys WHERE id = $1"))
            .bind(parse_id(id)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| db_error("find id with deleted", e))
    }
    async fn find_api_key_by_key_unchecked(
        &self,
        _ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        sqlx::query_as::<_, ApiKeyRow>(&format!(
            "SELECT {COLUMNS} FROM api_keys WHERE key = $1 AND deleted_at = 0"
        ))
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| db_error("find key", e))
    }
    async fn find_api_key_by_key_with_deleted_unchecked(
        &self,
        _ctx: &RequestContext,
        key: &str,
    ) -> RepoResult<Option<ApiKeyRow>> {
        sqlx::query_as::<_, ApiKeyRow>(&format!("SELECT {COLUMNS} FROM api_keys WHERE key = $1"))
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| db_error("find key with deleted", e))
    }
    async fn list_api_keys_by_user_unchecked(
        &self,
        _ctx: &RequestContext,
        user_id: &str,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult> {
        self.list_internal(query, Some(parse_id(user_id)?), None)
            .await
    }
    async fn list_api_keys_by_project_unchecked(
        &self,
        _ctx: &RequestContext,
        project_id: &str,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult> {
        self.list_internal(query, None, Some(parse_id(project_id)?))
            .await
    }
    async fn list_api_keys_unchecked(
        &self,
        _ctx: &RequestContext,
        query: &ListApiKeysQuery,
    ) -> RepoResult<ListApiKeysResult> {
        self.list_internal(query, None, None).await
    }

    async fn update_api_key_unchecked(
        &self,
        _ctx: &RequestContext,
        id: &str,
        input: UpdateApiKeyInput,
    ) -> RepoResult<ApiKeyRow> {
        let id = parse_id(id)?;
        let mut builder = QueryBuilder::<Postgres>::new("UPDATE api_keys SET ");
        let mut set = builder.separated(", ");
        if let Some(v) = input.name {
            set.push("name = ").push_bind_unseparated(v);
        }
        if let Some(v) = input.key_type {
            set.push("\"type\" = ").push_bind_unseparated(v);
        }
        if let Some(v) = input.status {
            set.push("status = ").push_bind_unseparated(v);
        }
        if let Some(v) = input.scopes {
            set.push("scopes = ")
                .push_bind_unseparated(sqlx::types::Json(v));
        }
        if let Some(v) = input.profiles {
            set.push("profiles = ")
                .push_bind_unseparated(sqlx::types::Json(v));
        }
        set.push("updated_at = now()");
        drop(set);
        builder
            .push(" WHERE id = ")
            .push_bind(id)
            .push(" AND deleted_at = 0");
        let affected = builder
            .build()
            .execute(&self.pool)
            .await
            .map_err(|e| db_error("update", e))?
            .rows_affected();
        if affected == 0 {
            return Err(RepoError::NotFound("api_key"));
        }
        self.find_api_key_by_id_unchecked(_ctx, &id.to_string())
            .await?
            .ok_or(RepoError::NotFound("api_key"))
    }

    async fn rotate_api_key_unchecked(
        &self,
        _ctx: &RequestContext,
        api_key_id: &str,
        new_key: &str,
    ) -> RepoResult<ApiKeyRow> {
        let row=sqlx::query_as::<_,ApiKeyRow>(&format!("UPDATE api_keys SET key=$1,updated_at=now() WHERE id=$2 AND deleted_at=0 RETURNING {COLUMNS}"))
            .bind(new_key).bind(parse_id(api_key_id)?).fetch_optional(&self.pool).await.map_err(|e|db_error("rotate",e))?;
        row.ok_or(RepoError::NotFound("api_key"))
    }

    async fn soft_delete_api_key_unchecked(
        &self,
        _ctx: &RequestContext,
        id: &str,
        _deleted_at: &str,
    ) -> RepoResult<ApiKeyRow> {
        let id = parse_id(id)?;
        let affected = sqlx::query("UPDATE api_keys SET deleted_at = $1, updated_at = now() WHERE id = $2 AND deleted_at = 0")
            .bind(Utc::now().timestamp()).bind(id).execute(&self.pool).await.map_err(|e| db_error("delete", e))?.rows_affected();
        if affected == 0 {
            return Err(RepoError::NotFound("api_key"));
        }
        self.find_api_key_by_id_with_deleted_unchecked(_ctx, &id.to_string())
            .await?
            .ok_or(RepoError::NotFound("api_key"))
    }

    async fn restore_api_key_unchecked(
        &self,
        _ctx: &RequestContext,
        id: &str,
    ) -> RepoResult<ApiKeyRow> {
        let id = parse_id(id)?;
        let row = self
            .find_api_key_by_id_with_deleted_unchecked(_ctx, &id.to_string())
            .await?
            .ok_or(RepoError::NotFound("api_key"))?;
        let conflict: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_keys WHERE project_id = $1 AND name = $2 AND deleted_at = 0 AND id <> $3)")
            .bind(row.project_id.parse::<i64>().map_err(|_| RepoError::NotFound("project"))?).bind(&row.name).bind(id)
            .fetch_one(&self.pool).await.map_err(|e| db_error("restore pre-check", e))?;
        if conflict {
            return Err(RepoError::NameConflict);
        }
        sqlx::query("UPDATE api_keys SET deleted_at = 0, updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| db_error("restore", e))?;
        self.find_api_key_by_id_unchecked(_ctx, &id.to_string())
            .await?
            .ok_or(RepoError::NotFound("api_key"))
    }

    async fn api_key_exists_unchecked(&self, _ctx: &RequestContext, key: &str) -> RepoResult<bool> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_keys WHERE key = $1)")
            .bind(key)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| db_error("exists", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PolicyContext, Principal};

    #[tokio::test]
    async fn live_postgres_api_key_isolated_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let repo = PgApiKeyRepo::new(pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let key = format!("conduit-pg-test-{}", std::process::id());
        sqlx::query("DELETE FROM api_keys WHERE key = $1")
            .bind(&key)
            .execute(&pool)
            .await?;
        let created = repo
            .create_api_key(
                &ctx,
                CreateApiKeyInput {
                    id: "ignored".into(),
                    user_id: Some("19301".into()),
                    project_id: "1".into(),
                    name: format!("pg-key-{}", std::process::id()),
                    key: key.clone(),
                    key_type: "user".into(),
                    scopes: vec!["api".into()],
                    profiles: Some(serde_json::json!({"activeProfile":"default"})),
                    created_at: Utc::now().to_rfc3339(),
                },
            )
            .await?;
        assert_eq!(created.user_id.as_deref(), Some("19301"));
        assert_eq!(created.profiles["activeProfile"], "default");
        let project_rows = repo
            .list_api_keys_by_project(
                &ctx,
                "1",
                &ListApiKeysQuery {
                    limit: 20,
                    ..Default::default()
                },
            )
            .await?;
        assert!(project_rows.rows.iter().any(|row| row.id == created.id));
        repo.soft_delete_api_key(&ctx, &created.id, "ignored")
            .await?;
        assert!(repo.find_api_key_by_key(&ctx, &key).await?.is_none());
        assert!(repo.api_key_exists(&ctx, &key).await?);
        repo.restore_api_key(&ctx, &created.id).await?;
        assert!(repo.find_api_key_by_key(&ctx, &key).await?.is_some());
        sqlx::query("DELETE FROM api_keys WHERE id = $1")
            .bind(created.id.parse::<i64>()?)
            .execute(&pool)
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
