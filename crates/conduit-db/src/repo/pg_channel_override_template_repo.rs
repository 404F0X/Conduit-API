//! PostgreSQL implementation of the channel override-template repository.

use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;

use crate::repo::channel_override_template_repo::{
    ChannelOverrideTemplateRepo, CreateChannelOverrideTemplateInput,
    UpdateChannelOverrideTemplateInput,
};
use crate::repo::{RepoError, RepoResult};
use crate::row::ChannelOverrideTemplateRow;

const SELECT_COLS: &str = "id::text AS id, user_id::text AS user_id, name, description, \
    override_parameters, override_headers, \
    COALESCE(header_override_operations, '[]'::jsonb) AS header_override_operations, \
    COALESCE(body_override_operations, '[]'::jsonb) AS body_override_operations, \
    created_at, updated_at, \
    CASE WHEN deleted_at = 0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

pub struct PgChannelOverrideTemplateRepo {
    pool: PgPool,
}

impl PgChannelOverrideTemplateRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn fetch(&self, id: i64, user_id: i64) -> RepoResult<Option<ChannelOverrideTemplateRow>> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM channel_override_templates \
             WHERE id = $1 AND user_id = $2 AND deleted_at = 0"
        );
        sqlx::query_as::<_, ChannelOverrideTemplateRow>(&sql)
            .bind(id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| {
                RepoError::Database(format!("postgres override-template fetch failed: {error}"))
            })
    }

    async fn live_name_count(
        &self,
        user_id: i64,
        name: &str,
        exclude_id: Option<i64>,
    ) -> RepoResult<i64> {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM channel_override_templates \
             WHERE user_id = $1 AND name = $2 AND deleted_at = 0 \
             AND ($3::bigint IS NULL OR id != $3)",
        )
        .bind(user_id)
        .bind(name)
        .bind(exclude_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| {
            RepoError::Database(format!(
                "postgres override-template name count failed: {error}"
            ))
        })
    }
}

fn parse_json(raw: &str, field: &'static str) -> RepoResult<Value> {
    serde_json::from_str(raw).map_err(|error| {
        RepoError::Database(format!("invalid override-template {field} JSON: {error}"))
    })
}

#[async_trait]
impl ChannelOverrideTemplateRepo for PgChannelOverrideTemplateRepo {
    async fn list(&self, user_id: i64) -> RepoResult<Vec<ChannelOverrideTemplateRow>> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM channel_override_templates \
             WHERE user_id = $1 AND deleted_at = 0 ORDER BY id ASC"
        );
        sqlx::query_as::<_, ChannelOverrideTemplateRow>(&sql)
            .bind(user_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| {
                RepoError::Database(format!("postgres override-template list failed: {error}"))
            })
    }

    async fn find(&self, id: i64, user_id: i64) -> RepoResult<Option<ChannelOverrideTemplateRow>> {
        self.fetch(id, user_id).await
    }

    async fn create(
        &self,
        input: CreateChannelOverrideTemplateInput,
    ) -> RepoResult<ChannelOverrideTemplateRow> {
        if input.name.trim().is_empty() {
            return Err(RepoError::Database(
                "override-template name must not be empty".to_string(),
            ));
        }
        if self
            .live_name_count(input.user_id, &input.name, None)
            .await?
            > 0
        {
            return Err(RepoError::Database(format!(
                "override-template name already exists: {}",
                input.name
            )));
        }
        let override_headers = parse_json(&input.override_headers, "override_headers")?;
        let header_ops = parse_json(
            &input.header_override_operations,
            "header_override_operations",
        )?;
        let body_ops = parse_json(&input.body_override_operations, "body_override_operations")?;
        let new_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channel_override_templates \
             (user_id, name, description, override_parameters, override_headers, \
              header_override_operations, body_override_operations) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        )
        .bind(input.user_id)
        .bind(&input.name)
        .bind(&input.description)
        .bind(&input.override_parameters)
        .bind(override_headers)
        .bind(header_ops)
        .bind(body_ops)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| {
            RepoError::Database(format!("postgres override-template create failed: {error}"))
        })?;
        self.fetch(new_id, input.user_id)
            .await?
            .ok_or(RepoError::NotFound("channel override template"))
    }

    async fn update(
        &self,
        id: i64,
        user_id: i64,
        input: UpdateChannelOverrideTemplateInput,
    ) -> RepoResult<ChannelOverrideTemplateRow> {
        if self.fetch(id, user_id).await?.is_none() {
            return Err(RepoError::NotFound("channel override template"));
        }
        if let Some(name) = &input.name {
            if name.trim().is_empty() {
                return Err(RepoError::Database(
                    "override-template name must not be empty".to_string(),
                ));
            }
            if self.live_name_count(user_id, name, Some(id)).await? > 0 {
                return Err(RepoError::Database(format!(
                    "override-template name already exists: {name}"
                )));
            }
        }

        let override_headers = input
            .override_headers
            .as_deref()
            .map(|raw| parse_json(raw, "override_headers"))
            .transpose()?;
        let header_ops = input
            .header_override_operations
            .as_deref()
            .map(|raw| parse_json(raw, "header_override_operations"))
            .transpose()?;
        let body_ops = input
            .body_override_operations
            .as_deref()
            .map(|raw| parse_json(raw, "body_override_operations"))
            .transpose()?;
        let description_present = input.description.is_some();

        sqlx::query(
            "UPDATE channel_override_templates SET \
             name = COALESCE($1, name), \
             description = CASE WHEN $2 THEN NULL WHEN $3 THEN $4 ELSE description END, \
             override_parameters = COALESCE($5, override_parameters), \
             override_headers = COALESCE($6, override_headers), \
             header_override_operations = COALESCE($7, header_override_operations), \
             body_override_operations = COALESCE($8, body_override_operations), \
             updated_at = now() \
             WHERE id = $9 AND user_id = $10 AND deleted_at = 0",
        )
        .bind(&input.name)
        .bind(input.clear_description)
        .bind(description_present)
        .bind(&input.description)
        .bind(&input.override_parameters)
        .bind(override_headers)
        .bind(header_ops)
        .bind(body_ops)
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|error| {
            RepoError::Database(format!("postgres override-template update failed: {error}"))
        })?;
        self.fetch(id, user_id)
            .await?
            .ok_or(RepoError::NotFound("channel override template"))
    }

    async fn soft_delete(&self, id: i64, user_id: i64) -> RepoResult<()> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            RepoError::Database(format!(
                "postgres override-template delete begin failed: {error}"
            ))
        })?;
        let name = sqlx::query_scalar::<_, String>(
            "SELECT name FROM channel_override_templates \
             WHERE id = $1 AND user_id = $2 AND deleted_at = 0 FOR UPDATE",
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| {
            RepoError::Database(format!(
                "postgres override-template delete probe failed: {error}"
            ))
        })?
        .ok_or(RepoError::NotFound("channel override template"))?;
        let max_deleted = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(deleted_at), 0) FROM channel_override_templates \
             WHERE user_id = $1 AND name = $2",
        )
        .bind(user_id)
        .bind(name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| {
            RepoError::Database(format!(
                "postgres override-template delete max failed: {error}"
            ))
        })?;
        let deleted_at = chrono::Utc::now().timestamp().max(max_deleted + 1);
        sqlx::query(
            "UPDATE channel_override_templates SET deleted_at = $1, updated_at = now() \
             WHERE id = $2 AND user_id = $3 AND deleted_at = 0",
        )
        .bind(deleted_at)
        .bind(id)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            RepoError::Database(format!("postgres override-template delete failed: {error}"))
        })?;
        tx.commit().await.map_err(|error| {
            RepoError::Database(format!(
                "postgres override-template delete commit failed: {error}"
            ))
        })
    }

    async fn channel_settings(&self, channel_id: i64) -> RepoResult<Option<String>> {
        sqlx::query_scalar::<_, String>(
            "SELECT COALESCE(settings, '{}'::jsonb)::text FROM channels \
             WHERE id = $1 AND deleted_at = 0",
        )
        .bind(channel_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| {
            RepoError::Database(format!("postgres channel settings read failed: {error}"))
        })
    }

    async fn set_channel_settings_batch(&self, updates: &[(i64, String)]) -> RepoResult<()> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            RepoError::Database(format!("postgres channel settings begin failed: {error}"))
        })?;
        for (channel_id, raw_settings) in updates {
            let settings = parse_json(raw_settings, "channel settings")?;
            let affected = sqlx::query(
                "UPDATE channels SET settings = $1, updated_at = now() \
                 WHERE id = $2 AND deleted_at = 0",
            )
            .bind(settings)
            .bind(channel_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                RepoError::Database(format!("postgres channel settings write failed: {error}"))
            })?
            .rows_affected();
            if affected == 0 {
                return Err(RepoError::NotFound("channel"));
            }
        }
        tx.commit().await.map_err(|error| {
            RepoError::Database(format!("postgres channel settings commit failed: {error}"))
        })
    }
}
