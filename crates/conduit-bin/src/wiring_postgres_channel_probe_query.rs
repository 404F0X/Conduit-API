//! PostgreSQL-backed channel probe and public-health GraphQL queries.

use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::ID;
use async_trait::async_trait;
use chrono::{DateTime, Timelike, Utc};
use conduit_admin_graphql::channel_probe_ext::{
    ChannelProbeData, ChannelProbeError, ChannelProbePoint, ChannelProbeServices,
    GetChannelProbeDataInput, PublicChannelHealth, PublicChannelHealthSettings,
};
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_services::SystemService;
use sqlx::{PgPool, Row};

pub struct PgChannelProbeQueryAdapter {
    pool: PgPool,
    system: Arc<SystemService>,
}

impl PgChannelProbeQueryAdapter {
    pub fn new(pool: PgPool, system: Arc<SystemService>) -> Self {
        Self { pool, system }
    }

    async fn query_at(
        &self,
        input: GetChannelProbeDataInput,
        now: DateTime<Utc>,
    ) -> Result<Vec<ChannelProbeData>, ChannelProbeError> {
        if input.channel_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = input
            .channel_ids
            .iter()
            .map(|id| decode_channel_id(id.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let setting = self.system.channel_setting_or_default(&ctx).await;
        let (interval_minutes, range_minutes) = probe_window(&setting.probe.frequency.0);
        let end = align_time(now, interval_minutes);
        let start = end - chrono::Duration::minutes(range_minutes);
        let timestamps = (start.timestamp()..=end.timestamp())
            .step_by((interval_minutes * 60) as usize)
            .map(|timestamp| {
                checked_i32(timestamp, "timestamp")
                    .map(|graphql_timestamp| (timestamp, graphql_timestamp))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let rows = sqlx::query(
            "SELECT channel_id, total_request_count, success_request_count, \
             avg_tokens_per_second, avg_time_to_first_token_ms, \"timestamp\" \
             FROM channel_probes WHERE channel_id = ANY($1) \
             AND \"timestamp\" >= $2 AND \"timestamp\" <= $3 \
             ORDER BY \"timestamp\" ASC",
        )
        .bind(&ids)
        .bind(start.timestamp())
        .bind(end.timestamp())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| ChannelProbeError::Query(error.to_string()))?;

        let mut probes = HashMap::new();
        for row in rows {
            let timestamp: i64 = row.get("timestamp");
            probes.insert(
                (row.get::<i64, _>("channel_id"), timestamp),
                ChannelProbePoint {
                    timestamp: checked_i32(timestamp, "timestamp")?,
                    total_request_count: checked_i32(
                        row.get("total_request_count"),
                        "total_request_count",
                    )?,
                    success_request_count: checked_i32(
                        row.get("success_request_count"),
                        "success_request_count",
                    )?,
                    avg_tokens_per_second: row.get("avg_tokens_per_second"),
                    avg_time_to_first_token_ms: row.get("avg_time_to_first_token_ms"),
                },
            );
        }

        ids.into_iter()
            .map(|channel_id| {
                let points = timestamps
                    .iter()
                    .map(|(timestamp, graphql_timestamp)| {
                        probes.get(&(channel_id, *timestamp)).cloned().unwrap_or(
                            ChannelProbePoint {
                                timestamp: *graphql_timestamp,
                                total_request_count: 0,
                                success_request_count: 0,
                                avg_tokens_per_second: None,
                                avg_time_to_first_token_ms: None,
                            },
                        )
                    })
                    .collect();
                Ok(ChannelProbeData {
                    channel_id: ID::from(format!("gid://conduit/Channel/{channel_id}")),
                    points,
                })
            })
            .collect()
    }

    async fn public_health_enabled(&self) -> bool {
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        self.system
            .channel_setting_or_default(&ctx)
            .await
            .extra
            .get("expose_public_channel_health")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    async fn aggregate_public_health(
        &self,
        now: DateTime<Utc>,
    ) -> Result<PublicChannelHealth, ChannelProbeError> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(total_request_count), 0)::bigint AS total_requests, \
             COALESCE(SUM(success_request_count), 0)::bigint AS successful_requests, \
             AVG(avg_time_to_first_token_ms) AS avg_ttft, \
             AVG(avg_tokens_per_second) AS avg_tps, MAX(\"timestamp\") AS last_updated \
             FROM channel_probes WHERE \"timestamp\" >= $1",
        )
        .bind((now - chrono::Duration::hours(24)).timestamp())
        .fetch_one(&self.pool)
        .await
        .map_err(|error| ChannelProbeError::Query(error.to_string()))?;
        let total: i64 = row.get("total_requests");
        let successful: i64 = row.get("successful_requests");
        let success_rate = (total > 0).then(|| successful as f64 * 100.0 / total as f64);
        let last_updated: Option<i64> = row.get("last_updated");
        Ok(PublicChannelHealth {
            status: public_health_status(success_rate).to_string(),
            success_rate,
            avg_time_to_first_token_ms: row.get("avg_ttft"),
            avg_tokens_per_second: row.get("avg_tps"),
            last_updated_at: last_updated
                .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0))
                .map(|timestamp| timestamp.to_rfc3339()),
        })
    }
}

#[async_trait]
impl ChannelProbeServices for PgChannelProbeQueryAdapter {
    async fn channel_probe_data(
        &self,
        input: GetChannelProbeDataInput,
    ) -> Result<Vec<ChannelProbeData>, ChannelProbeError> {
        self.query_at(input, Utc::now()).await
    }

    async fn public_channel_health(
        &self,
    ) -> Result<Option<PublicChannelHealth>, ChannelProbeError> {
        if !self.public_health_enabled().await {
            return Ok(None);
        }
        self.aggregate_public_health(Utc::now()).await.map(Some)
    }

    async fn public_channel_health_settings(
        &self,
    ) -> Result<PublicChannelHealthSettings, ChannelProbeError> {
        Ok(PublicChannelHealthSettings {
            enabled: self.public_health_enabled().await,
        })
    }

    async fn set_public_channel_health_settings(
        &self,
        enabled: bool,
    ) -> Result<(), ChannelProbeError> {
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let mut settings = self.system.channel_setting_or_default(&ctx).await;
        settings.extra.insert(
            "expose_public_channel_health".to_string(),
            serde_json::Value::Bool(enabled),
        );
        self.system
            .set_channel_setting(&ctx, settings)
            .await
            .map(|_| ())
            .map_err(|error| ChannelProbeError::Query(error.to_string()))
    }
}

fn public_health_status(success_rate: Option<f64>) -> &'static str {
    match success_rate {
        None => "UNKNOWN",
        Some(rate) if rate >= 99.0 => "OPERATIONAL",
        Some(rate) if rate >= 90.0 => "DEGRADED",
        Some(_) => "DISRUPTED",
    }
}

fn decode_channel_id(raw: &str) -> Result<i64, ChannelProbeError> {
    raw.strip_prefix("gid://conduit/Channel/")
        .unwrap_or(raw)
        .parse()
        .map_err(|_| ChannelProbeError::InvalidChannelId(raw.to_string()))
}

fn probe_window(frequency: &str) -> (i64, i64) {
    match frequency {
        "5m" => (5, 60),
        "30m" => (30, 720),
        "1h" => (60, 1440),
        _ => (1, 10),
    }
}

fn align_time(now: DateTime<Utc>, interval_minutes: i64) -> DateTime<Utc> {
    let minute = i64::from(now.minute());
    let aligned_minute = minute - minute.rem_euclid(interval_minutes);
    now.with_minute(aligned_minute as u32)
        .and_then(|time| time.with_second(0))
        .and_then(|time| time.with_nanosecond(0))
        .unwrap_or(now)
}

fn checked_i32(value: i64, field: &'static str) -> Result<i32, ChannelProbeError> {
    i32::try_from(value)
        .map_err(|_| ChannelProbeError::Query(format!("{field} exceeds GraphQL Int range")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_cache::NoopCache;

    #[tokio::test]
    async fn postgres_probe_query_and_health_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let system = Arc::new(SystemService::from_system_repo(
            Arc::new(conduit_db::PgSystemRepo::new(database.pool.clone())),
            Arc::new(NoopCache::new()),
        ));
        let adapter = PgChannelProbeQueryAdapter::new(database.pool.clone(), system);
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let setting = adapter.system.channel_setting_or_default(&ctx).await;
        let (interval_minutes, _) = probe_window(&setting.probe.frequency.0);
        let now = align_time(Utc::now(), interval_minutes);
        sqlx::query(
            "INSERT INTO channel_probes \
             (channel_id, total_request_count, success_request_count, \
              avg_tokens_per_second, avg_time_to_first_token_ms, \"timestamp\") \
             VALUES (33, 10, 9, 42.5, 120.0, $1)",
        )
        .bind(now.timestamp())
        .execute(&database.pool)
        .await?;
        let data = adapter
            .query_at(
                GetChannelProbeDataInput {
                    channel_ids: vec![ID::from("gid://conduit/Channel/33")],
                },
                now,
            )
            .await?;
        assert_eq!(data.len(), 1);
        let point = data[0]
            .points
            .iter()
            .find(|point| i64::from(point.timestamp) == now.timestamp())
            .ok_or("probe point missing")?;
        assert_eq!(point.total_request_count, 10);
        assert_eq!(point.success_request_count, 9);

        adapter.set_public_channel_health_settings(true).await?;
        let health = adapter
            .public_channel_health()
            .await?
            .ok_or("public health should be enabled")?;
        assert_eq!(health.status, "DEGRADED");
        assert_eq!(health.success_rate, Some(90.0));
        database.cleanup().await?;
        Ok(())
    }
}
