//! PostgreSQL-backed upstream-channel repository.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder, Transaction};

use crate::repo::channel_repo::{
    ChannelRepo, CreateChannelInput, ListChannelsQuery, ListChannelsResult, UpdateChannelInput,
    cache_signature,
};
use crate::repo::{RepoError, RepoResult, RequestContext};
use crate::row::ChannelRow;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, \"type\", base_url, website_url, quota_currency, \
CAST(actual_quota_used AS TEXT) AS actual_quota_used, CAST(quota_remaining AS TEXT) AS quota_remaining, \
name,status,credentials,COALESCE(disabled_api_keys,'[]'::jsonb) AS disabled_api_keys, \
supported_models,COALESCE(manual_models,'[]'::jsonb) AS manual_models,auto_sync_supported_models, \
COALESCE(auto_sync_model_pattern,'') AS auto_sync_model_pattern,COALESCE(tags,'[]'::jsonb) AS tags, \
default_test_model,COALESCE(policies,'{\"stream\":\"unlimited\"}'::jsonb) AS policies, \
COALESCE(settings,'{\"model_mappings\":[]}'::jsonb) AS settings,ordering_weight,error_message,remark, \
COALESCE(endpoints,'[]'::jsonb) AS endpoints,created_at,updated_at, \
CASE WHEN deleted_at=0 THEN NULL ELSE to_timestamp(deleted_at) END AS deleted_at";

#[derive(Debug, Clone)]
pub struct PgChannelRepo {
    pool: PgPool,
}
impl PgChannelRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn find_channel_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        value: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        sqlx::query_as::<_, ChannelRow>(&format!(
            "SELECT {COLUMNS} FROM channels WHERE id=$1 AND deleted_at=0"
        ))
        .bind(id(value)?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| error("find in transaction", e))
    }

    pub async fn create_channel_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        input: CreateChannelInput,
    ) -> RepoResult<ChannelRow> {
        let inserted = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\",base_url,website_url,quota_currency,actual_quota_used,quota_remaining, \
              name,status,credentials,supported_models,manual_models,auto_sync_supported_models, \
              auto_sync_model_pattern,tags,default_test_model,policies,settings,endpoints,remark, \
              ordering_weight) \
             VALUES ($1,$2,$3,$4,NULLIF($5::text,'')::numeric,NULLIF($6::text,'')::numeric, \
                     $7,'disabled',$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19) \
             RETURNING id",
        )
        .bind(input.channel_type)
        .bind(input.base_url)
        .bind(input.website_url)
        .bind(input.quota_currency)
        .bind(input.actual_quota_used)
        .bind(input.quota_remaining)
        .bind(input.name)
        .bind(sqlx::types::Json(input.credentials))
        .bind(sqlx::types::Json(input.supported_models))
        .bind(sqlx::types::Json(input.manual_models))
        .bind(input.auto_sync_supported_models)
        .bind(input.auto_sync_model_pattern)
        .bind(sqlx::types::Json(input.tags))
        .bind(input.default_test_model)
        .bind(sqlx::types::Json(
            input
                .policies
                .unwrap_or_else(|| serde_json::json!({"stream":"unlimited"})),
        ))
        .bind(sqlx::types::Json(
            input
                .settings
                .unwrap_or_else(|| serde_json::json!({"model_mappings":[]})),
        ))
        .bind(sqlx::types::Json(input.endpoints))
        .bind(input.remark)
        .bind(input.ordering_weight)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| error("create in transaction", e))?;
        Self::find_channel_in_tx(tx, &inserted.to_string())
            .await?
            .ok_or(RepoError::NotFound("channel"))
    }
}
fn id(v: &str) -> RepoResult<i64> {
    v.parse()
        .map_err(|_| RepoError::NotFound("channel id not a valid integer"))
}
fn timestamp(v: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(v)
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or_default()
}
fn error(c: &str, e: sqlx::Error) -> RepoError {
    if e.as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|v| v == "23505")
    {
        RepoError::NameConflict
    } else {
        RepoError::Database(format!("postgres channel repo {c} failed: {e}"))
    }
}

#[async_trait]
impl ChannelRepo for PgChannelRepo {
    async fn create_channel_unchecked(
        &self,
        ctx: &RequestContext,
        i: CreateChannelInput,
    ) -> RepoResult<ChannelRow> {
        let inserted=sqlx::query_scalar::<_,i64>("INSERT INTO channels (\"type\",base_url,website_url,quota_currency,actual_quota_used,quota_remaining,name,status,credentials,supported_models,manual_models,auto_sync_supported_models,auto_sync_model_pattern,tags,default_test_model,policies,settings,endpoints,remark,ordering_weight) VALUES ($1,$2,$3,$4,NULLIF($5::text,'')::numeric,NULLIF($6::text,'')::numeric,$7,'disabled',$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19) RETURNING id")
   .bind(i.channel_type).bind(i.base_url).bind(i.website_url).bind(i.quota_currency).bind(i.actual_quota_used).bind(i.quota_remaining).bind(i.name)
   .bind(sqlx::types::Json(i.credentials)).bind(sqlx::types::Json(i.supported_models)).bind(sqlx::types::Json(i.manual_models))
   .bind(i.auto_sync_supported_models).bind(i.auto_sync_model_pattern).bind(sqlx::types::Json(i.tags)).bind(i.default_test_model)
   .bind(sqlx::types::Json(i.policies.unwrap_or_else(||serde_json::json!({"stream":"unlimited"}))))
   .bind(sqlx::types::Json(i.settings.unwrap_or_else(||serde_json::json!({"model_mappings":[]}))))
   .bind(sqlx::types::Json(i.endpoints)).bind(i.remark).bind(i.ordering_weight).fetch_one(&self.pool).await.map_err(|e|error("create",e))?;
        self.find_channel_unchecked(ctx, &inserted.to_string())
            .await?
            .ok_or(RepoError::NotFound("channel"))
    }
    async fn find_channel_unchecked(
        &self,
        _: &RequestContext,
        v: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        sqlx::query_as::<_, ChannelRow>(&format!(
            "SELECT {COLUMNS} FROM channels WHERE id=$1 AND deleted_at=0"
        ))
        .bind(id(v)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| error("find", e))
    }
    async fn find_channel_with_deleted_unchecked(
        &self,
        _: &RequestContext,
        v: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        sqlx::query_as::<_, ChannelRow>(&format!("SELECT {COLUMNS} FROM channels WHERE id=$1"))
            .bind(id(v)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| error("find deleted", e))
    }
    async fn find_channel_by_name_unchecked(
        &self,
        _: &RequestContext,
        v: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        sqlx::query_as::<_, ChannelRow>(&format!(
            "SELECT {COLUMNS} FROM channels WHERE name=$1 AND deleted_at=0"
        ))
        .bind(v)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| error("find name", e))
    }
    async fn find_channels_by_tags_unchecked(
        &self,
        _: &RequestContext,
        tags: &[String],
    ) -> RepoResult<Vec<ChannelRow>> {
        if tags.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_,ChannelRow>(&format!("SELECT {COLUMNS} FROM channels WHERE deleted_at=0 AND COALESCE(tags,'[]'::jsonb) ?| $1 ORDER BY ordering_weight DESC,name")).bind(tags).fetch_all(&self.pool).await.map_err(|e|error("find tags",e))
    }
    async fn list_enabled_channels_unchecked(
        &self,
        _: &RequestContext,
    ) -> RepoResult<Vec<ChannelRow>> {
        sqlx::query_as::<_,ChannelRow>(&format!("SELECT {COLUMNS} FROM channels WHERE status='enabled' AND deleted_at=0 ORDER BY ordering_weight DESC,name")).fetch_all(&self.pool).await.map_err(|e|error("list enabled",e))
    }
    async fn find_channel_by_cache_signature_unchecked(
        &self,
        ctx: &RequestContext,
        s: &str,
    ) -> RepoResult<Option<ChannelRow>> {
        let Some((channel_id, _)) = s.split_once(':') else {
            return Ok(None);
        };
        let row = self.find_channel_unchecked(ctx, channel_id).await?;
        Ok(row.filter(|r| cache_signature(r) == s))
    }
    async fn list_channels_unchecked(
        &self,
        _: &RequestContext,
        q: &ListChannelsQuery,
    ) -> RepoResult<ListChannelsResult> {
        let mut b = QueryBuilder::<Postgres>::new(format!(
            "SELECT {COLUMNS} FROM channels WHERE deleted_at=0"
        ));
        if !q.status_in.is_empty() {
            b.push(" AND status = ANY(")
                .push_bind(&q.status_in)
                .push(")");
        }
        if let (Some(at), Some(cursor)) = (&q.after_created_at, &q.after_id) {
            b.push(" AND (created_at>")
                .push_bind(timestamp(at))
                .push(" OR (created_at=")
                .push_bind(timestamp(at))
                .push(" AND id>")
                .push_bind(id(cursor)?)
                .push("))");
        }
        b.push(" ORDER BY created_at,id LIMIT ")
            .push_bind(i64::from(q.limit) + 1)
            .push(" OFFSET ")
            .push_bind(i64::from(q.offset));
        let mut rows = b
            .build_query_as::<ChannelRow>()
            .fetch_all(&self.pool)
            .await
            .map_err(|e| error("list", e))?;
        let has_more = rows.len() > q.limit as usize;
        rows.truncate(q.limit as usize);
        Ok(ListChannelsResult { rows, has_more })
    }
    async fn update_channel_unchecked(
        &self,
        ctx: &RequestContext,
        v: &str,
        i: UpdateChannelInput,
    ) -> RepoResult<ChannelRow> {
        let channel_id = id(v)?;
        let mut b = QueryBuilder::<Postgres>::new("UPDATE channels SET ");
        let mut s = b.separated(", ");
        macro_rules! set {
            ($field:expr,$value:expr) => {{
                s.push($field).push_bind_unseparated($value);
            }};
        }
        if let Some(v) = i.channel_type {
            set!("\"type\"=", v)
        }
        if let Some(v) = i.name {
            set!("name=", v)
        }
        if let Some(v) = i.base_url {
            set!("base_url=", v)
        }
        if let Some(v) = i.website_url {
            set!("website_url=", v)
        }
        if let Some(v) = i.quota_currency {
            set!("quota_currency=", v)
        }
        if let Some(v) = i.actual_quota_used {
            s.push("actual_quota_used=NULLIF(")
                .push_bind_unseparated(v)
                .push_unseparated("::text,'')::numeric");
        }
        if let Some(v) = i.quota_remaining {
            s.push("quota_remaining=NULLIF(")
                .push_bind_unseparated(v)
                .push_unseparated("::text,'')::numeric");
        }
        if let Some(v) = i.credentials {
            set!("credentials=", sqlx::types::Json(v))
        }
        if let Some(v) = i.disabled_api_keys {
            set!("disabled_api_keys=", sqlx::types::Json(v))
        }
        if let Some(v) = i.supported_models {
            set!("supported_models=", sqlx::types::Json(v))
        }
        if let Some(v) = i.manual_models {
            set!("manual_models=", sqlx::types::Json(v))
        }
        if let Some(v) = i.auto_sync_supported_models {
            set!("auto_sync_supported_models=", v)
        }
        if let Some(v) = i.auto_sync_model_pattern {
            set!("auto_sync_model_pattern=", v)
        }
        if let Some(v) = i.tags {
            set!("tags=", sqlx::types::Json(v))
        }
        if let Some(v) = i.default_test_model {
            set!("default_test_model=", v)
        }
        if let Some(v) = i.policies {
            set!("policies=", sqlx::types::Json(v))
        }
        if let Some(v) = i.settings {
            set!("settings=", sqlx::types::Json(v))
        }
        if let Some(v) = i.endpoints {
            set!("endpoints=", sqlx::types::Json(v))
        }
        if let Some(v) = i.remark {
            set!("remark=", v)
        }
        if let Some(v) = i.ordering_weight {
            set!("ordering_weight=", v)
        }
        if let Some(v) = i.error_message {
            set!("error_message=", v)
        }
        if let Some(v) = i.status {
            set!("status=", v)
        }
        s.push("updated_at=now()");
        drop(s);
        b.push(" WHERE id=")
            .push_bind(channel_id)
            .push(" AND deleted_at=0");
        if b.build()
            .execute(&self.pool)
            .await
            .map_err(|e| error("update", e))?
            .rows_affected()
            == 0
        {
            return Err(RepoError::NotFound("channel"));
        }
        self.find_channel_unchecked(ctx, &channel_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("channel"))
    }
    async fn soft_delete_channel_unchecked(
        &self,
        ctx: &RequestContext,
        v: &str,
        at: &str,
    ) -> RepoResult<ChannelRow> {
        let channel_id = id(v)?;
        if sqlx::query("UPDATE channels SET deleted_at=CAST(EXTRACT(EPOCH FROM $2::timestamptz) AS BIGINT),updated_at=$2,status='archived' WHERE id=$1 AND deleted_at=0").bind(channel_id).bind(timestamp(at)).execute(&self.pool).await.map_err(|e|error("delete",e))?.rows_affected()==0{return Err(RepoError::NotFound("channel"))}
        self.find_channel_with_deleted_unchecked(ctx, &channel_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("channel"))
    }
    async fn restore_channel_unchecked(
        &self,
        ctx: &RequestContext,
        v: &str,
    ) -> RepoResult<ChannelRow> {
        let channel_id = id(v)?;
        if sqlx::query(
            "UPDATE channels SET deleted_at=0,updated_at=now(),status='disabled' WHERE id=$1",
        )
        .bind(channel_id)
        .execute(&self.pool)
        .await
        .map_err(|e| error("restore", e))?
        .rows_affected()
            == 0
        {
            return Err(RepoError::NotFound("channel"));
        }
        self.find_channel_unchecked(ctx, &channel_id.to_string())
            .await?
            .ok_or(RepoError::NotFound("channel"))
    }
    async fn channel_exists_unchecked(&self, _: &RequestContext, name: &str) -> RepoResult<bool> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM channels WHERE name=$1 AND deleted_at=0)",
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
    async fn postgres_channel_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let repo = PgChannelRepo::new(database.pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let suffix = chrono::Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_default()
            .to_string();
        let tag = format!("mock-{suffix}");
        let row = repo
            .create_channel(
                &ctx,
                CreateChannelInput {
                    id: "ignored".into(),
                    channel_type: "openai".into(),
                    name: format!("Mock upstream {suffix}"),
                    base_url: Some("http://127.0.0.1:18080".into()),
                    website_url: None,
                    quota_currency: Some("USD".into()),
                    actual_quota_used: Some("5.25".into()),
                    quota_remaining: Some("44.75".into()),
                    credentials: serde_json::json!({"api_key":"secret"}),
                    supported_models: vec!["mock-chat".into()],
                    manual_models: vec![],
                    default_test_model: "mock-chat".into(),
                    auto_sync_supported_models: false,
                    auto_sync_model_pattern: String::new(),
                    tags: vec![tag.clone()],
                    policies: None,
                    settings: None,
                    endpoints: vec![],
                    remark: None,
                    ordering_weight: 10,
                    created_at: String::new(),
                },
            )
            .await?;
        assert_eq!(row.quota_remaining.as_deref(), Some("44.75"));
        assert_eq!(
            repo.find_channels_by_tags(&ctx, std::slice::from_ref(&tag))
                .await?
                .len(),
            1
        );
        let updated = repo
            .update_channel(
                &ctx,
                &row.id,
                UpdateChannelInput {
                    status: Some("enabled".into()),
                    quota_remaining: Some(Some("40.5".into())),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.status, "enabled");
        assert!(
            repo.list_enabled_channels(&ctx)
                .await?
                .iter()
                .any(|candidate| candidate.id == row.id)
        );
        assert_eq!(
            repo.soft_delete_channel(&ctx, &row.id, "2026-08-15T00:00:00Z")
                .await?
                .status,
            "archived"
        );
        assert_eq!(
            repo.restore_channel(&ctx, &row.id).await?.status,
            "disabled"
        );
        repo.soft_delete_channel(&ctx, &row.id, "2026-08-15T00:00:01Z")
            .await?;
        database.cleanup().await?;
        Ok(())
    }
}
