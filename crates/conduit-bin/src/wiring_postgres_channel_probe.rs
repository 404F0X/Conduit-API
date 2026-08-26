//! PostgreSQL channel-probe worker executor and live-stream sweeper.

use chrono::{DateTime, Utc};
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_scheduler::{AlignInterval, ProbeFrequency, align_to_interval};
use conduit_services::SystemService;
use sqlx::{PgPool, Row};
use std::sync::{Arc, Mutex};

pub struct PgChannelProbeAdapter {
    pool: PgPool,
    system: Option<Arc<SystemService>>,
    last_dynamic_run: Mutex<Option<DateTime<Utc>>>,
}

impl PgChannelProbeAdapter {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            system: None,
            last_dynamic_run: Mutex::new(None),
        }
    }

    pub fn with_dynamic_settings(mut self, system: Arc<SystemService>) -> Self {
        self.system = Some(system);
        self
    }

    async fn compute_and_store(
        &self,
        aligned: DateTime<Utc>,
        interval_minutes: i64,
    ) -> Result<u64, sqlx::Error> {
        let channel_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM channels WHERE status = 'enabled' AND deleted_at = 0",
        )
        .fetch_all(&self.pool)
        .await?;
        if channel_ids.is_empty() {
            return Ok(0);
        }
        let start = aligned - chrono::Duration::minutes(interval_minutes);
        let rows = sqlx::query(
            "SELECT se.channel_id, \
                COUNT(*)::bigint AS total_count, \
                SUM(CASE WHEN se.status = 'completed' THEN 1 ELSE 0 END)::bigint AS success_count, \
                SUM(CASE WHEN se.status = 'completed' THEN COALESCE(ul.total_completion_tokens, 0) ELSE 0 END)::bigint AS total_tokens, \
                SUM(CASE WHEN se.status = 'completed' THEN \
                    CASE WHEN se.stream AND se.metrics_first_token_latency_ms IS NOT NULL \
                         THEN GREATEST(COALESCE(se.metrics_latency_ms, 0) \
                              - se.metrics_first_token_latency_ms, 0) \
                         ELSE COALESCE(se.metrics_latency_ms, 0) END \
                    ELSE 0 END)::bigint AS effective_latency_ms, \
                SUM(CASE WHEN se.status = 'completed' AND se.stream \
                         AND se.metrics_first_token_latency_ms IS NOT NULL \
                         THEN se.metrics_first_token_latency_ms ELSE 0 END)::bigint \
                    AS total_first_token_latency, \
                COUNT(DISTINCT se.request_id)::bigint AS request_count, \
                SUM(CASE WHEN se.status = 'completed' AND se.stream \
                         AND se.metrics_first_token_latency_ms IS NOT NULL \
                         THEN 1 ELSE 0 END)::bigint AS streaming_request_count \
             FROM request_executions se \
             LEFT JOIN ( \
                SELECT request_id, channel_id, \
                    SUM(COALESCE(completion_tokens, 0) \
                      + COALESCE(completion_reasoning_tokens, 0) \
                      + COALESCE(completion_audio_tokens, 0))::bigint \
                        AS total_completion_tokens \
                FROM usage_logs WHERE created_at >= $1 \
                GROUP BY request_id, channel_id \
             ) ul ON se.status = 'completed' \
                AND se.request_id = ul.request_id AND se.channel_id = ul.channel_id \
             WHERE se.created_at >= $2 AND se.created_at < $3 \
                AND se.status IN ('completed', 'failed') \
                AND se.channel_id = ANY($4) \
             GROUP BY se.channel_id ORDER BY se.channel_id",
        )
        .bind(start)
        .bind(start)
        .bind(aligned)
        .bind(&channel_ids)
        .fetch_all(&self.pool)
        .await?;

        let mut tx = self.pool.begin().await?;
        let mut inserted = 0;
        for row in rows {
            let total: i64 = row.get("total_count");
            if total == 0 {
                continue;
            }
            let success: i64 = row.get("success_count");
            let total_tokens: i64 = row.get("total_tokens");
            let effective_latency_ms: i64 = row.get("effective_latency_ms");
            let total_first_token_latency: i64 = row.get("total_first_token_latency");
            let streaming_request_count: i64 = row.get("streaming_request_count");
            let avg_tps = (total_tokens > 0 && effective_latency_ms > 0)
                .then(|| total_tokens as f64 / (effective_latency_ms as f64 / 1000.0));
            let avg_ttft = (total_first_token_latency > 0 && streaming_request_count > 0)
                .then(|| total_first_token_latency as f64 / streaming_request_count as f64);
            sqlx::query(
                "INSERT INTO channel_probes \
                 (channel_id, total_request_count, success_request_count, \
                  avg_tokens_per_second, avg_time_to_first_token_ms, \"timestamp\") \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(row.get::<i64, _>("channel_id"))
            .bind(total)
            .bind(success)
            .bind(avg_tps)
            .bind(avg_ttft)
            .bind(aligned.timestamp())
            .execute(&mut *tx)
            .await?;
            inserted += 1;
        }
        tx.commit().await?;
        Ok(inserted)
    }

    async fn current_probe_plan(
        &self,
        now: DateTime<Utc>,
        fallback_interval_minutes: i64,
    ) -> Option<(DateTime<Utc>, i64)> {
        let Some(system) = &self.system else {
            return Some((now, fallback_interval_minutes));
        };
        let context = RequestContext::new(PolicyContext::new(Principal::system()));
        let settings = system.channel_setting_or_default(&context).await;
        dynamic_probe_plan(
            settings.probe.enabled,
            &settings.probe.frequency.0,
            now,
            &self.last_dynamic_run,
        )
    }
}

impl conduit_scheduler::ChannelProbeExecutor for PgChannelProbeAdapter {
    fn run_probe(&self, now: DateTime<Utc>, interval_minutes: i64) -> Result<(), String> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let Some((aligned, interval_minutes)) =
                    self.current_probe_plan(now, interval_minutes).await
                else {
                    return Ok(());
                };
                self.compute_and_store(aligned, interval_minutes)
                    .await
                    .map(|_| ())
                    .map_err(|error| format!("postgres channel probe failed: {error}"))
            })
        })
    }
}

fn dynamic_probe_plan(
    enabled: bool,
    frequency: &str,
    now: DateTime<Utc>,
    last_run: &Mutex<Option<DateTime<Utc>>>,
) -> Option<(DateTime<Utc>, i64)> {
    if !enabled {
        return None;
    }
    let frequency = match frequency {
        "1m" => ProbeFrequency::OneMinute,
        "30m" => ProbeFrequency::ThirtyMinutes,
        "1h" => ProbeFrequency::OneHour,
        _ => ProbeFrequency::FiveMinutes,
    };
    let interval_minutes = frequency.interval_minutes();
    let aligned = align_to_interval(AlignInterval::from_probe_frequency(frequency), now);
    let Ok(mut last_run) = last_run.lock() else {
        return None;
    };
    if last_run.as_ref() == Some(&aligned) {
        return None;
    }
    *last_run = Some(aligned);
    Some((aligned, interval_minutes))
}

pub struct PgLiveStreamSweepAdapter {
    registry: std::sync::Arc<conduit_orchestrator::live_streaming::LiveStreamRegistry>,
}

impl PgLiveStreamSweepAdapter {
    pub fn new(
        registry: std::sync::Arc<conduit_orchestrator::live_streaming::LiveStreamRegistry>,
    ) -> Self {
        Self { registry }
    }
}

impl conduit_scheduler::LiveStreamSweepExecutor for PgLiveStreamSweepAdapter {
    fn sweep(&self, idle_threshold_minutes: i64) -> Result<usize, String> {
        Ok(self
            .registry
            .sweep_stale_entries(std::time::Duration::from_secs(
                idle_threshold_minutes.max(1) as u64 * 60,
            )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;
    use conduit_scheduler::ChannelProbeExecutor;
    use sqlx::types::Json;

    #[test]
    fn dynamic_probe_plan_applies_disable_and_frequency_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let last_run = Mutex::new(None);
        let first = DateTime::parse_from_rfc3339("2024-01-01T10:02:30Z")?.with_timezone(&Utc);
        let same_bucket = DateTime::parse_from_rfc3339("2024-01-01T10:04:59Z")?.with_timezone(&Utc);

        assert!(dynamic_probe_plan(false, "1m", first, &last_run).is_none());
        let first_plan = dynamic_probe_plan(true, "5m", first, &last_run).ok_or("first plan")?;
        assert_eq!(first_plan.1, 5);
        assert_eq!(first_plan.0.minute(), 0);
        assert!(dynamic_probe_plan(true, "5m", same_bucket, &last_run).is_none());

        let one_minute_plan =
            dynamic_probe_plan(true, "1m", same_bucket, &last_run).ok_or("one-minute plan")?;
        assert_eq!(one_minute_plan.1, 1);
        assert_eq!(one_minute_plan.0.minute(), 4);
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn postgres_probe_aggregates_and_persists_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels \
             (\"type\", name, status, credentials, default_test_model) \
             VALUES ('openai', 'probe-channel', 'enabled', '{}'::jsonb, 'gpt-test') \
             RETURNING id",
        )
        .fetch_one(&database.pool)
        .await?;
        let aligned = Utc::now()
            .with_second(0)
            .and_then(|value| value.with_nanosecond(0))
            .ok_or("failed to align test timestamp")?;
        let created_at = aligned - chrono::Duration::seconds(30);
        sqlx::query(
            "INSERT INTO request_executions \
             (project_id, request_id, channel_id, model_id, format, request_body, \
              status, stream, metrics_latency_ms, metrics_first_token_latency_ms, created_at) \
             VALUES (1, 901, $1, 'gpt-test', 'openai/chat_completions', $2, \
                     'completed', TRUE, 2000, 500, $3)",
        )
        .bind(channel_id)
        .bind(Json(serde_json::json!({})))
        .bind(created_at)
        .execute(&database.pool)
        .await?;
        sqlx::query(
            "INSERT INTO usage_logs \
             (request_id, channel_id, project_id, model_id, completion_tokens, \
              total_tokens, created_at) VALUES (901, $1, 1, 'gpt-test', 150, 150, $2)",
        )
        .bind(channel_id)
        .bind(created_at)
        .execute(&database.pool)
        .await?;

        PgChannelProbeAdapter::new(database.pool.clone()).run_probe(aligned, 5)?;
        let row = sqlx::query(
            "SELECT total_request_count, success_request_count, \
             avg_tokens_per_second, avg_time_to_first_token_ms \
             FROM channel_probes WHERE channel_id = $1 AND \"timestamp\" = $2",
        )
        .bind(channel_id)
        .bind(aligned.timestamp())
        .fetch_one(&database.pool)
        .await?;
        assert_eq!(row.get::<i64, _>("total_request_count"), 1);
        assert_eq!(row.get::<i64, _>("success_request_count"), 1);
        assert!((row.get::<f64, _>("avg_tokens_per_second") - 100.0).abs() < 0.001);
        assert!((row.get::<f64, _>("avg_time_to_first_token_ms") - 500.0).abs() < 0.001);
        database.cleanup().await?;
        Ok(())
    }
}
