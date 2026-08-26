//! Runtime route-health adapters.
//!
//! The Operations ledger and request router share the classifier in
//! `conduit-services::route_health`; these adapters only load a short recent
//! execution window for the concrete credential selected for a target.

use std::collections::BTreeMap;

use async_trait::async_trait;
use conduit_core::ConduitError;
use conduit_orchestrator::orchestrator::{RouteHealthSource, RouteHealthTarget};
use conduit_services::{RouteHealthSample, RouteHealthStatus, classify_route_health};

use crate::wiring_operations::ERROR_CATEGORY_SQL;

fn sample(attempts: i64, successes: i64, errors: Vec<(String, i64)>) -> RouteHealthStatus {
    let category_count = |category: &str| {
        errors
            .iter()
            .filter(|(name, _)| name == category)
            .map(|(_, count)| *count)
            .sum()
    };
    classify_route_health(RouteHealthSample {
        attempts,
        successes,
        auth_failures: category_count("auth"),
        configuration_failures: category_count("configuration"),
        transient_failures: errors
            .iter()
            .filter(|(name, _)| {
                matches!(
                    name.as_str(),
                    "rate_limit" | "timeout" | "upstream_5xx" | "connection" | "canceled"
                )
            })
            .map(|(_, count)| *count)
            .sum(),
    })
}

pub(crate) struct PgRouteHealthSource {
    pool: sqlx::PgPool,
}

impl PgRouteHealthSource {
    pub(crate) fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RouteHealthSource for PgRouteHealthSource {
    async fn statuses(
        &self,
        targets: &[RouteHealthTarget],
    ) -> Result<BTreeMap<RouteHealthTarget, RouteHealthStatus>, ConduitError> {
        let mut result = BTreeMap::new();
        for target in targets {
            let Some(channel_id) = target.channel_id.parse::<i64>().ok() else {
                continue;
            };
            let attempts: (i64, i64) = sqlx::query_as(
                "SELECT COUNT(*)::bigint, COALESCE(SUM(CASE WHEN status='completed' THEN 1 ELSE 0 END),0)::bigint \
                 FROM request_executions WHERE channel_id=$1 AND model_id=$2 AND credential_identity IS NOT DISTINCT FROM $3 \
                   AND created_at >= now() - interval '15 minutes'",
            )
            .bind(channel_id)
            .bind(&target.actual_model)
            .bind(&target.credential_identity)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| ConduitError::internal(format!("route health query failed: {error}")))?;
            if attempts.0 == 0 {
                continue;
            }
            let query = format!(
                "SELECT {ERROR_CATEGORY_SQL} AS category, COUNT(*)::bigint \
                 FROM request_executions WHERE channel_id=$1 AND model_id=$2 AND credential_identity IS NOT DISTINCT FROM $3 \
                   AND status IN ('failed','canceled') AND created_at >= now() - interval '15 minutes' \
                 GROUP BY category"
            );
            let errors: Vec<(String, i64)> = sqlx::query_as(&query)
                .bind(channel_id)
                .bind(&target.actual_model)
                .bind(&target.credential_identity)
                .fetch_all(&self.pool)
                .await
                .map_err(|error| {
                    ConduitError::internal(format!("route health error query failed: {error}"))
                })?;
            result.insert(target.clone(), sample(attempts.0, attempts.1, errors));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod postgres_tests {
    use super::*;

    #[tokio::test]
    async fn postgres_credential_health_isolated_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model) \
             VALUES('openai','route-health-test','enabled','{}'::jsonb,'[]'::jsonb,'') RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        let request_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO requests(project_id,model_id,request_body,status) \
             VALUES(1,'same-model','{}'::jsonb,'completed') RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        sqlx::query(
            "INSERT INTO request_executions \
                (project_id,request_id,channel_id,model_id,credential_identity,request_body,status,response_status_code,error_message) \
             VALUES \
                (1,$1,$2,'same-model','sha256:bad','{}'::jsonb,'failed',401,'unauthorized'), \
                (1,$1,$2,'same-model','sha256:good','{}'::jsonb,'completed',200,NULL)",
        )
        .bind(request_id)
        .bind(channel_id)
        .execute(&database.pool)
        .await?;

        let bad = RouteHealthTarget {
            channel_id: channel_id.to_string(),
            actual_model: "same-model".into(),
            credential_identity: Some("sha256:bad".into()),
        };
        let good = RouteHealthTarget {
            credential_identity: Some("sha256:good".into()),
            ..bad.clone()
        };
        let statuses = PgRouteHealthSource::new(database.pool.clone())
            .statuses(&[bad.clone(), good.clone()])
            .await?;

        assert_eq!(statuses.get(&bad), Some(&RouteHealthStatus::Unhealthy));
        assert_eq!(statuses.get(&good), Some(&RouteHealthStatus::Healthy));
        database.cleanup().await?;
        Ok(())
    }
}
