//! Keeps channel-discovered provider models separate from the public SKU catalog.
//!
//! `channels.supported_models` describes upstream inventory. It must never create
//! rows in `models`, because `models` is the stable, customer-facing SKU catalog.
//! Discovery is materialized as `(channel, upstream model, variant)` deployments;
//! administrators explicitly connect deployments to public SKUs through routes.

use sqlx::PgPool;
pub async fn ensure_upstream_model_deployments_postgres(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO upstream_model_deployments(channel_id,upstream_model_id,internal_name,variant,status,source,created_at,updated_at) \
         SELECT c.id,m.model_id,c.name||' / '||m.model_id,'','enabled','discovered',now(),now() \
         FROM channels c CROSS JOIN LATERAL jsonb_array_elements_text( \
           CASE WHEN jsonb_typeof(c.supported_models)='array' THEN c.supported_models ELSE '[]'::jsonb END \
         ) AS m(model_id) \
         WHERE c.deleted_at=0 AND btrim(m.model_id)<>'' \
         ON CONFLICT(channel_id,upstream_model_id,variant) DO UPDATE SET status='enabled',internal_name=EXCLUDED.internal_name,updated_at=now()",
    ).execute(pool).await?;
    sqlx::query(
        "UPDATE upstream_model_deployments d SET status='disabled',updated_at=now() \
         FROM channels c WHERE c.id=d.channel_id AND d.source='discovered' AND ( \
           c.deleted_at<>0 OR NOT EXISTS ( \
             SELECT 1 FROM jsonb_array_elements_text( \
               CASE WHEN jsonb_typeof(c.supported_models)='array' THEN c.supported_models ELSE '[]'::jsonb END \
             ) AS m(model_id) WHERE m.model_id=d.upstream_model_id \
           ) \
         )",
    )
    .execute(pool)
    .await?;
    sqlx::raw_sql(
        "CREATE OR REPLACE FUNCTION conduit_sync_channel_deployments() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           INSERT INTO upstream_model_deployments(channel_id,upstream_model_id,internal_name,variant,status,source,created_at,updated_at)
           SELECT NEW.id,m.model_id,NEW.name||' / '||m.model_id,'','enabled','discovered',now(),now()
           FROM jsonb_array_elements_text(
             CASE WHEN jsonb_typeof(NEW.supported_models)='array' THEN NEW.supported_models ELSE '[]'::jsonb END
           ) AS m(model_id)
           WHERE NEW.deleted_at=0 AND btrim(m.model_id)<>''
           ON CONFLICT(channel_id,upstream_model_id,variant) DO UPDATE
             SET status='enabled',internal_name=EXCLUDED.internal_name,updated_at=now();
           UPDATE upstream_model_deployments d SET status='disabled',updated_at=now()
           WHERE d.channel_id=NEW.id AND d.source='discovered'
             AND (NEW.deleted_at<>0 OR NOT EXISTS(SELECT 1 FROM jsonb_array_elements_text(
                              CASE WHEN jsonb_typeof(NEW.supported_models)='array' THEN NEW.supported_models ELSE '[]'::jsonb END
                            ) AS m(model_id)
                            WHERE m.model_id=d.upstream_model_id));
           RETURN NEW;
         END $$;
         CREATE OR REPLACE TRIGGER deployments_from_channel_supported_models
         AFTER INSERT OR UPDATE OF supported_models,name,deleted_at ON channels
         FOR EACH ROW EXECUTE FUNCTION conduit_sync_channel_deployments();",
    ).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_db::repo::channel_repo::{
        CreateChannelInput as RepoCreateChannelInput, UpdateChannelInput as RepoUpdateChannelInput,
    };
    use conduit_db::{ChannelRepo, PgChannelRepo, PolicyContext, Principal, RequestContext};

    #[tokio::test]
    async fn postgres_channel_repo_writes_materialize_supported_model_inventory_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let pool = PgPool::connect(&dsn).await?;
        conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;
        ensure_upstream_model_deployments_postgres(&pool).await?;
        let name = format!("pg-catalog-{}", uuid::Uuid::new_v4().simple());
        let repo = PgChannelRepo::new(pool.clone());
        let ctx = RequestContext::new(PolicyContext::new(Principal::test()));
        let channel = repo
            .create_channel(
                &ctx,
                RepoCreateChannelInput {
                    id: String::new(),
                    channel_type: "openai".into(),
                    name,
                    base_url: Some("http://127.0.0.1:18099/v1".into()),
                    website_url: None,
                    quota_currency: Some("USD".into()),
                    actual_quota_used: None,
                    quota_remaining: None,
                    credentials: serde_json::json!({}),
                    supported_models: vec!["same-model".into()],
                    manual_models: Vec::new(),
                    default_test_model: "same-model".into(),
                    auto_sync_supported_models: false,
                    auto_sync_model_pattern: String::new(),
                    tags: Vec::new(),
                    policies: None,
                    settings: None,
                    endpoints: Vec::new(),
                    remark: None,
                    ordering_weight: 0,
                    created_at: chrono::Utc::now().to_rfc3339(),
                },
            )
            .await?;
        let channel_id = channel.id.parse::<i64>()?;
        repo.update_channel(
            &ctx,
            &channel.id,
            RepoUpdateChannelInput {
                supported_models: Some(vec!["other-model".into()]),
                updated_at: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            },
        )
        .await?;
        let rows=sqlx::query_as::<_,(String,String)>("SELECT upstream_model_id,status FROM upstream_model_deployments WHERE channel_id=$1 ORDER BY upstream_model_id")
            .bind(channel_id).fetch_all(&pool).await?;
        assert_eq!(
            rows,
            vec![
                ("other-model".into(), "enabled".into()),
                ("same-model".into(), "disabled".into())
            ]
        );
        sqlx::query("UPDATE channels SET supported_models='{}'::jsonb WHERE id=$1")
            .bind(channel_id)
            .execute(&pool)
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM upstream_model_deployments \
                 WHERE channel_id=$1 AND upstream_model_id='other-model'"
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await?,
            "disabled"
        );
        repo.update_channel(
            &ctx,
            &channel.id,
            RepoUpdateChannelInput {
                supported_models: Some(vec!["other-model".into()]),
                updated_at: chrono::Utc::now().to_rfc3339(),
                ..Default::default()
            },
        )
        .await?;
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM upstream_model_deployments \
                 WHERE channel_id=$1 AND upstream_model_id='other-model'"
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await?,
            "enabled"
        );
        repo.soft_delete_channel(&ctx, &channel.id, &chrono::Utc::now().to_rfc3339())
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM upstream_model_deployments \
                 WHERE channel_id=$1 AND upstream_model_id='other-model'"
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await?,
            "disabled"
        );
        Ok(())
    }
}
