//! PostgreSQL-backed explicit route-affinity repository.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::repo::route_affinity_repo::{
    RouteAffinityKey, RouteAffinityRepo, UpsertRouteAffinityInput,
};
use crate::repo::{RepoError, RepoResult, RequestContext};
use crate::row::RouteAffinityRow;

const COLUMNS: &str = "CAST(id AS TEXT) AS id, CAST(project_id AS TEXT) AS project_id, \
key_class, key_hash, public_model_id, api_format, CAST(channel_id AS TEXT) AS channel_id, \
upstream_model_id, upstream_api_format, credential_identity, expires_at, created_at, updated_at";

#[derive(Debug, Clone)]
pub struct PgRouteAffinityRepo {
    pool: PgPool,
}

impl PgRouteAffinityRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

fn parse_id(value: &str, label: &'static str) -> RepoResult<i64> {
    value.parse().map_err(|_| RepoError::NotFound(label))
}

fn database_error(operation: &str, error: sqlx::Error) -> RepoError {
    RepoError::Database(format!(
        "postgres route affinity repo {operation} failed: {error}"
    ))
}

#[async_trait]
impl RouteAffinityRepo for PgRouteAffinityRepo {
    async fn find_valid_route_affinity_unchecked(
        &self,
        _ctx: &RequestContext,
        key: &RouteAffinityKey,
        now: DateTime<Utc>,
    ) -> RepoResult<Option<RouteAffinityRow>> {
        sqlx::query_as::<_, RouteAffinityRow>(&format!(
            "SELECT {COLUMNS} FROM route_affinities \
             WHERE project_id=$1 AND key_class=$2 AND key_hash=$3 \
               AND public_model_id=$4 AND api_format=$5 AND expires_at>$6"
        ))
        .bind(parse_id(
            &key.project_id,
            "route affinity project id not a valid integer",
        )?)
        .bind(&key.key_class)
        .bind(&key.key_hash)
        .bind(&key.public_model_id)
        .bind(&key.api_format)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| database_error("find valid", error))
    }

    async fn upsert_route_affinity_unchecked(
        &self,
        _ctx: &RequestContext,
        input: UpsertRouteAffinityInput,
        now: DateTime<Utc>,
    ) -> RepoResult<RouteAffinityRow> {
        sqlx::query_as::<_, RouteAffinityRow>(&format!(
            "INSERT INTO route_affinities(\
                project_id,key_class,key_hash,public_model_id,api_format,channel_id,\
                upstream_model_id,upstream_api_format,credential_identity,expires_at,created_at,updated_at\
             ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$11) \
             ON CONFLICT(project_id,key_class,key_hash,public_model_id,api_format) \
             DO UPDATE SET channel_id=EXCLUDED.channel_id, \
                upstream_model_id=EXCLUDED.upstream_model_id, \
                upstream_api_format=EXCLUDED.upstream_api_format, \
                credential_identity=EXCLUDED.credential_identity, \
                expires_at=EXCLUDED.expires_at, updated_at=EXCLUDED.updated_at \
             RETURNING {COLUMNS}"
        ))
        .bind(parse_id(
            &input.key.project_id,
            "route affinity project id not a valid integer",
        )?)
        .bind(input.key.key_class)
        .bind(input.key.key_hash)
        .bind(input.key.public_model_id)
        .bind(input.key.api_format)
        .bind(parse_id(
            &input.channel_id,
            "route affinity channel id not a valid integer",
        )?)
        .bind(input.upstream_model_id)
        .bind(input.upstream_api_format)
        .bind(input.credential_identity)
        .bind(input.expires_at)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| database_error("upsert", error))
    }

    async fn delete_expired_route_affinities_unchecked(
        &self,
        _ctx: &RequestContext,
        now: DateTime<Utc>,
        limit: u32,
    ) -> RepoResult<u64> {
        let result = sqlx::query(
            "DELETE FROM route_affinities WHERE id IN (\
                SELECT id FROM route_affinities WHERE expires_at <= $1 \
                ORDER BY expires_at, id LIMIT $2\
             )",
        )
        .bind(now)
        .bind(i64::from(limit))
        .execute(&self.pool)
        .await
        .map_err(|error| database_error("delete expired", error))?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::route_affinity_repo::{KEY_CLASS_PREVIOUS_RESPONSE_ID, RouteAffinityRepo};
    use crate::{PolicyContext, Principal};
    use chrono::Duration;

    fn context() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    #[tokio::test]
    async fn postgres_upsert_lookup_and_expiry_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let suffix = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let project_id: i64 = sqlx::query_scalar(
            "INSERT INTO projects(name,status) VALUES($1,'active') RETURNING id",
        )
        .bind(format!("route-affinity-project-{suffix}"))
        .fetch_one(&database.pool)
        .await?;
        let channel_id: i64 = sqlx::query_scalar(
            "INSERT INTO channels(\"type\",name,status,credentials,default_test_model) \
             VALUES('openai',$1,'enabled','{}'::jsonb,'gpt-test') RETURNING id",
        )
        .bind(format!("route-affinity-channel-{suffix}"))
        .fetch_one(&database.pool)
        .await?;
        let repo = PgRouteAffinityRepo::new(database.pool.clone());
        let now = Utc::now();
        let key = RouteAffinityKey {
            project_id: project_id.to_string(),
            key_class: KEY_CLASS_PREVIOUS_RESPONSE_ID.into(),
            key_hash: "b".repeat(64),
            public_model_id: "gpt-public".into(),
            api_format: "openai/responses".into(),
        };

        let first = repo
            .upsert_route_affinity(
                &context(),
                UpsertRouteAffinityInput {
                    key: key.clone(),
                    channel_id: channel_id.to_string(),
                    upstream_model_id: "gpt-upstream-a".into(),
                    upstream_api_format: "openai/responses".into(),
                    credential_identity: Some("sha256:first".into()),
                    expires_at: now + Duration::hours(1),
                },
                now,
            )
            .await?;
        let second = repo
            .upsert_route_affinity(
                &context(),
                UpsertRouteAffinityInput {
                    key: key.clone(),
                    channel_id: channel_id.to_string(),
                    upstream_model_id: "gpt-upstream-b".into(),
                    upstream_api_format: "openai/responses".into(),
                    credential_identity: Some("sha256:second".into()),
                    expires_at: now + Duration::hours(2),
                },
                now + Duration::seconds(1),
            )
            .await?;

        assert_eq!(first.id, second.id);
        assert_eq!(second.upstream_model_id, "gpt-upstream-b");
        assert_eq!(second.credential_identity.as_deref(), Some("sha256:second"));
        assert!(
            repo.find_valid_route_affinity(&context(), &key, now)
                .await?
                .is_some()
        );
        assert!(
            repo.find_valid_route_affinity(&context(), &key, now + Duration::hours(3))
                .await?
                .is_none()
        );
        assert_eq!(
            repo.delete_expired_route_affinities_unchecked(
                &context(),
                now + Duration::hours(3),
                100,
            )
            .await?,
            1
        );

        database.cleanup().await?;
        Ok(())
    }
}
