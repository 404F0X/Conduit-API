//! PostgreSQL-backed administrator operations ledger.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;

use conduit_admin_graphql::operations::{
    OperationsAccountingCoverage, OperationsChannelRow, OperationsError, OperationsFlow,
    OperationsFlowRow, OperationsLedger, OperationsLedgerSummary, OperationsModelSeries,
    OperationsModelSeriesPoint, OperationsRisk, OperationsRouteHealthRow, OperationsServices,
    OperationsTrendPoint, ProviderObservationHistory, ProviderPriceChangeEvent,
    ProviderQuotaObservation,
};
use conduit_services::classify_route_health;

use crate::wiring_operations::{
    AttemptAggregate, BillingAggregate, CASH_ACCOUNTING_NOTE, ERROR_CATEGORY_SQL, TpsAggregate,
    UsageAggregate, cost_metric, count, error_buckets, profit_metric, rate, revenue_metric,
    route_health_sample, timestamp_is_stale, weighted_average,
};

const OPERATIONS_ATTEMPT_AGGREGATE_SQL: &str = "SELECT channel_id,COUNT(DISTINCT request_id)::bigint,COUNT(*)::bigint, \
            SUM(CASE WHEN status='completed' THEN 1 ELSE 0 END)::bigint, \
            SUM(CASE WHEN status IN ('failed','canceled') THEN 1 ELSE 0 END)::bigint, \
            AVG(metrics_latency_ms)::double precision, \
            AVG(metrics_first_token_latency_ms)::double precision, \
            COUNT(metrics_first_token_latency_ms)::bigint, \
            SUM(CASE WHEN EXISTS (SELECT 1 FROM request_executions previous \
                WHERE previous.request_id = current.request_id AND \
                  (previous.created_at < current.created_at OR \
                   (previous.created_at = current.created_at AND previous.id < current.id))) \
                THEN 1 ELSE 0 END)::bigint, MAX(created_at) \
     FROM request_executions current WHERE channel_id IS NOT NULL \
       AND created_at >= $1 AND created_at < $2 \
     GROUP BY channel_id";

const OPERATIONS_ROUTE_HEALTH_SQL: &str = "SELECT execution.channel_id,channel.name,execution.model_id, \
            execution.credential_identity,COUNT(*)::bigint, \
            SUM(CASE WHEN execution.status='completed' THEN 1 ELSE 0 END)::bigint, \
            SUM(CASE WHEN execution.status IN ('failed','canceled') THEN 1 ELSE 0 END)::bigint, \
            MAX(execution.created_at) \
     FROM request_executions execution JOIN channels channel ON channel.id=execution.channel_id \
     WHERE execution.channel_id IS NOT NULL AND execution.created_at >= $1 AND execution.created_at < $2 \
     GROUP BY execution.channel_id,channel.name,execution.model_id,execution.credential_identity \
     ORDER BY channel.name,execution.model_id,execution.credential_identity";

const OPERATIONS_USAGE_AGGREGATE_SQL: &str = "SELECT channel_id,COUNT(*)::bigint,COUNT(total_cost)::bigint,SUM(total_cost), \
            COALESCE(SUM(prompt_tokens),0)::bigint, \
            COALESCE(SUM(completion_tokens),0)::bigint, \
            COALESCE(SUM(prompt_cached_tokens),0)::bigint, \
            COALESCE(SUM(total_tokens),0)::bigint,MAX(created_at) \
     FROM usage_logs WHERE channel_id IS NOT NULL \
       AND created_at >= $1 AND created_at < $2 \
     GROUP BY channel_id";

fn rolling_bucket_label(start: DateTime<Utc>, bucket: i64) -> String {
    (start + Duration::days(bucket + 1))
        .date_naive()
        .to_string()
}

#[derive(Clone)]
pub(crate) struct PgOperationsAdapter {
    pool: PgPool,
    read_pool: Option<PgPool>,
    fallback_on_replica_failure: bool,
    now: Option<DateTime<Utc>>,
}

impl PgOperationsAdapter {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self {
            pool,
            read_pool: None,
            fallback_on_replica_failure: false,
            now: None,
        }
    }

    pub(crate) fn with_read_pool(
        mut self,
        read_pool: Option<PgPool>,
        fallback_on_replica_failure: bool,
    ) -> Self {
        self.read_pool = read_pool;
        self.fallback_on_replica_failure = fallback_on_replica_failure;
        self
    }

    #[cfg(test)]
    fn with_now(mut self, now: DateTime<Utc>) -> Self {
        self.now = Some(now);
        self
    }

    fn now(&self) -> DateTime<Utc> {
        self.now.unwrap_or_else(Utc::now)
    }
}

#[derive(Debug)]
struct ChannelMeta {
    id: i64,
    name: String,
    channel_type: String,
    status: String,
    quota_currency: Option<String>,
    quota_remaining: Option<String>,
    actual_quota_used: Option<String>,
    updated_at: DateTime<Utc>,
    observed_snapshot_at: Option<DateTime<Utc>>,
    observed_pricing_source: Option<String>,
    observed_pricing_at: Option<DateTime<Utc>>,
    observed_price_change_count: i64,
}

#[async_trait]
impl OperationsServices for PgOperationsAdapter {
    async fn operations_ledger(
        &self,
        period_days: i32,
    ) -> Result<OperationsLedger, OperationsError> {
        if let Some(read_pool) = self.read_pool.as_ref() {
            match self.load_from(read_pool, period_days).await {
                Ok(ledger) => return Ok(ledger),
                Err(_) if self.fallback_on_replica_failure => {}
                Err(error) => return Err(OperationsError::Query(error.to_string())),
            }
        }
        self.load_from(&self.pool, period_days)
            .await
            .map_err(|error| OperationsError::Query(error.to_string()))
    }

    async fn operations_flow(
        &self,
        period_days: i32,
        limit: i32,
    ) -> Result<OperationsFlow, OperationsError> {
        if let Some(read_pool) = self.read_pool.as_ref() {
            match self.load_flow_from(read_pool, period_days, limit).await {
                Ok(flow) => return Ok(flow),
                Err(_) if self.fallback_on_replica_failure => {}
                Err(error) => return Err(OperationsError::Query(error.to_string())),
            }
        }
        self.load_flow_from(&self.pool, period_days, limit)
            .await
            .map_err(|error| OperationsError::Query(error.to_string()))
    }

    async fn operations_model_series(
        &self,
        period_days: i32,
    ) -> Result<OperationsModelSeries, OperationsError> {
        if let Some(read_pool) = self.read_pool.as_ref() {
            match self.load_model_series_from(read_pool, period_days).await {
                Ok(series) => return Ok(series),
                Err(_) if self.fallback_on_replica_failure => {}
                Err(error) => return Err(OperationsError::Query(error.to_string())),
            }
        }
        self.load_model_series_from(&self.pool, period_days)
            .await
            .map_err(|error| OperationsError::Query(error.to_string()))
    }

    async fn provider_observation_history(
        &self,
        channel_id: &str,
        requested_limit: i32,
    ) -> Result<ProviderObservationHistory, OperationsError> {
        let channel_id = channel_id
            .rsplit('/')
            .next()
            .unwrap_or(channel_id)
            .parse::<i64>()
            .map_err(|_| OperationsError::Query("invalid channel ID".to_string()))?;
        let limit = i64::from(requested_limit.clamp(1, 200));
        let quota_rows = sqlx::query_as::<
            _,
            (
                i64,
                String,
                bool,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<bool>,
                Option<String>,
                Option<String>,
                DateTime<Utc>,
            ),
        >(
            "SELECT id,status,success,currency,total,used,remaining,unlimited, \
                    balance_source,error_message,observed_at \
             FROM provider_quota_observations WHERE channel_id=$1 \
             ORDER BY observed_at DESC,id DESC LIMIT $2",
        )
        .bind(channel_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| OperationsError::Query(error.to_string()))?;
        let price_rows = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                DateTime<Utc>,
            ),
        >(
            "SELECT id,upstream_model_id,group_name,billing_kind,event_type,field_name, \
                    from_value,to_value,created_at \
             FROM provider_price_change_events WHERE channel_id=$1 \
             ORDER BY created_at DESC,id DESC LIMIT $2",
        )
        .bind(channel_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| OperationsError::Query(error.to_string()))?;
        Ok(ProviderObservationHistory {
            quota: quota_rows
                .into_iter()
                .map(|row| ProviderQuotaObservation {
                    id: count(row.0),
                    status: row.1,
                    success: row.2,
                    currency: row.3,
                    total: row.4,
                    used: row.5,
                    remaining: row.6,
                    unlimited: row.7,
                    balance_source: row.8,
                    error_message: row.9,
                    observed_at: row.10.to_rfc3339(),
                })
                .collect(),
            price_changes: price_rows
                .into_iter()
                .map(|row| ProviderPriceChangeEvent {
                    id: count(row.0),
                    upstream_model_id: row.1,
                    group_name: row.2,
                    billing_kind: row.3,
                    event_type: row.4,
                    field_name: row.5,
                    from_value: row.6,
                    to_value: row.7,
                    created_at: row.8.to_rfc3339(),
                })
                .collect(),
        })
    }
}

impl PgOperationsAdapter {
    async fn load_model_series_from(
        &self,
        pool: &PgPool,
        requested_days: i32,
    ) -> Result<OperationsModelSeries, sqlx::Error> {
        let days = match requested_days {
            1 | 7 | 14 | 29 => requested_days,
            _ => 1,
        };
        let granularity = if days == 1 {
            "hour"
        } else if days <= 14 {
            "day"
        } else {
            "week"
        };
        let end = self.now();
        let start = end - Duration::days(i64::from(days));
        type PointTuple = (DateTime<Utc>, String, i64, i64, Option<f64>, f64, i64);
        let rows = sqlx::query_as::<_,PointTuple>(
            "SELECT date_trunc($3::text,ul.created_at),COALESCE(NULLIF(route.requested_model,''),NULLIF(request.model_id,''),'unknown'), \
                    COUNT(*)::bigint,COALESCE(SUM(ul.total_tokens),0)::bigint,SUM(ul.total_cost), \
                    COALESCE(SUM(COALESCE(settlement.revenue,0)),0)::double precision,COALESCE(SUM(COALESCE(settlement.settled_count,0)),0)::bigint \
             FROM usage_logs ul LEFT JOIN requests request ON request.id=ul.request_id \
             LEFT JOIN request_route_explanations route ON route.request_id=ul.request_id \
             LEFT JOIN (SELECT event.usage_log_id,SUM(CASE WHEN charge.status='settled' THEN NULLIF(event.calculation_snapshot->>'accounting_amount','')::numeric ELSE 0::numeric END)::double precision revenue, \
                    SUM(CASE WHEN charge.status='settled' THEN 1 ELSE 0 END)::bigint settled_count FROM customer_charge_events event \
                    LEFT JOIN charge_settlements charge ON charge.charge_event_id=event.id GROUP BY event.usage_log_id) settlement ON settlement.usage_log_id=ul.id \
             WHERE ul.created_at >= $1 AND ul.created_at < $2 GROUP BY 1,2 ORDER BY 1,2"
        ).bind(start).bind(end).bind(granularity).fetch_all(pool).await?;
        Ok(OperationsModelSeries {
            generated_at: end.to_rfc3339(),
            period_start: start.to_rfc3339(),
            period_end: end.to_rfc3339(),
            period_days: days,
            granularity: granularity.into(),
            points: rows
                .into_iter()
                .map(|row| OperationsModelSeriesPoint {
                    bucket_start: row.0.to_rfc3339(),
                    requested_model: row.1,
                    metered_requests: count(row.2),
                    total_tokens: count(row.3),
                    recorded_upstream_cost: row.4,
                    recognized_usage_revenue: (row.6 > 0).then_some(row.5),
                })
                .collect(),
        })
    }

    async fn load_flow_from(
        &self,
        pool: &PgPool,
        requested_days: i32,
        requested_limit: i32,
    ) -> Result<OperationsFlow, sqlx::Error> {
        let days = match requested_days {
            1 | 7 | 14 | 29 | 30 => requested_days,
            _ => 7,
        };
        let limit = i64::from(requested_limit.clamp(1, 500));
        let end = self.now();
        let start = end - Duration::days(i64::from(days));

        type FlowTuple = (
            Option<i64>,
            Option<String>,
            i64,
            Option<String>,
            Option<i64>,
            Option<String>,
            String,
            String,
            Option<i64>,
            Option<String>,
            i64,
            i64,
            Option<f64>,
            f64,
            i64,
            Option<DateTime<Utc>>,
        );
        let rows = sqlx::query_as::<_, FlowTuple>(
            "SELECT usr.id,usr.email,ul.project_id,project.name,key.id,key.name, \
                    COALESCE(NULLIF(route.requested_model,''),NULLIF(request.model_id,''),'unknown'), \
                    COALESCE(NULLIF(route.final_model_id,''),NULLIF(ul.model_id,''),'unknown'), \
                    ul.channel_id,channel.name,COUNT(*)::bigint, \
                    COALESCE(SUM(ul.total_tokens),0)::bigint,SUM(ul.total_cost), \
                    COALESCE(SUM(COALESCE(settlement.revenue,0)),0)::double precision, \
                    COALESCE(SUM(COALESCE(settlement.settled_count,0)),0)::bigint,MAX(ul.created_at) \
             FROM usage_logs ul \
             LEFT JOIN requests request ON request.id=ul.request_id \
             LEFT JOIN request_route_explanations route ON route.request_id=ul.request_id \
             LEFT JOIN projects project ON project.id=ul.project_id \
             LEFT JOIN api_keys key ON key.id=ul.api_key_id \
             LEFT JOIN users usr ON usr.id=key.user_id \
             LEFT JOIN channels channel ON channel.id=ul.channel_id \
             LEFT JOIN ( \
               SELECT event.usage_log_id, \
                      SUM(CASE WHEN charge.status='settled' THEN NULLIF(event.calculation_snapshot->>'accounting_amount','')::numeric ELSE 0::numeric END)::double precision revenue, \
                      SUM(CASE WHEN charge.status='settled' THEN 1 ELSE 0 END)::bigint settled_count \
               FROM customer_charge_events event \
               LEFT JOIN charge_settlements charge ON charge.charge_event_id=event.id \
               GROUP BY event.usage_log_id \
             ) settlement ON settlement.usage_log_id=ul.id \
             WHERE ul.created_at >= $1 AND ul.created_at < $2 \
             GROUP BY usr.id,usr.email,ul.project_id,project.name,key.id,key.name, \
                      COALESCE(NULLIF(route.requested_model,''),NULLIF(request.model_id,''),'unknown'), \
                      COALESCE(NULLIF(route.final_model_id,''),NULLIF(ul.model_id,''),'unknown'), \
                      ul.channel_id,channel.name \
             ORDER BY COALESCE(SUM(ul.total_tokens),0) DESC,COUNT(*) DESC LIMIT $3",
        )
        .bind(start)
        .bind(end)
        .bind(limit)
        .fetch_all(pool)
        .await?;
        let (usage_rows, settled_usage_rows) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT COUNT(*)::bigint,COALESCE(SUM(CASE WHEN EXISTS( \
               SELECT 1 FROM customer_charge_events event \
               JOIN charge_settlements charge ON charge.charge_event_id=event.id \
               WHERE event.usage_log_id=ul.id AND charge.status='settled') THEN 1 ELSE 0 END),0)::bigint \
             FROM usage_logs ul WHERE ul.created_at >= $1 AND ul.created_at < $2",
        )
        .bind(start)
        .bind(end)
        .fetch_one(pool)
        .await?;

        Ok(OperationsFlow {
            generated_at: end.to_rfc3339(),
            period_start: start.to_rfc3339(),
            period_end: end.to_rfc3339(),
            period_days: days,
            usage_rows: count(usage_rows),
            settled_usage_rows: count(settled_usage_rows),
            rows: rows
                .into_iter()
                .map(|row| OperationsFlowRow {
                    user_id: row.0.map(count),
                    user_email: row.1.unwrap_or_default(),
                    project_id: count(row.2),
                    project_name: row.3.unwrap_or_default(),
                    api_key_id: row.4.map(count),
                    api_key_name: row.5.unwrap_or_default(),
                    requested_model: row.6,
                    actual_model: row.7,
                    channel_id: row.8.map(count),
                    channel_name: row.9.unwrap_or_default(),
                    metered_requests: count(row.10),
                    total_tokens: count(row.11),
                    recorded_upstream_cost: row.12,
                    recognized_usage_revenue: (row.14 > 0).then_some(row.13),
                    settled_requests: count(row.14),
                    last_activity_at: row.15.map(|value| value.to_rfc3339()).unwrap_or_default(),
                })
                .collect(),
            attribution_note: "METERED_SUCCESSFUL_USAGE_FLOW".into(),
        })
    }

    async fn load_from(
        &self,
        pool: &PgPool,
        requested_days: i32,
    ) -> Result<OperationsLedger, sqlx::Error> {
        let days = match requested_days {
            1 | 7 | 14 | 29 | 30 => requested_days,
            _ => 7,
        };
        let end = self.now();
        let start = end - Duration::days(i64::from(days));
        let health_start = end - Duration::minutes(15);

        let raw_channels = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                DateTime<Utc>,
                Option<DateTime<Utc>>,
                Option<String>,
                Option<DateTime<Utc>>,
                i64,
            ),
        >(
            "SELECT c.id,c.name,c.type,c.status, \
                    COALESCE(c.quota_currency,q.quota_data->>'currency'), \
                    COALESCE(c.quota_remaining::text,q.quota_data->>'remaining'), \
                    COALESCE(c.actual_quota_used::text,q.quota_data->>'used'), \
                    c.updated_at,qo.observed_at,ps.primary_endpoint,ps.observed_at, \
                    COALESCE((SELECT COUNT(*)::bigint FROM provider_price_change_events event \
                              WHERE event.to_snapshot_id=ps.id),0::bigint) \
             FROM channels c \
             LEFT JOIN provider_quota_status q ON q.channel_id=c.id AND q.deleted_at=0 \
             LEFT JOIN LATERAL ( \
               SELECT observed_at FROM provider_quota_observations candidate \
               WHERE candidate.channel_id=c.id AND candidate.success \
               ORDER BY candidate.observed_at DESC,candidate.id DESC LIMIT 1 \
             ) qo ON TRUE \
             LEFT JOIN LATERAL ( \
               SELECT id,primary_endpoint,observed_at FROM provider_price_snapshots candidate \
               WHERE candidate.channel_id=c.id AND candidate.status='success' \
               ORDER BY candidate.observed_at DESC,candidate.id DESC LIMIT 1 \
             ) ps ON TRUE \
             WHERE c.deleted_at=0 ORDER BY c.ordering_weight DESC,c.name ASC",
        )
        .fetch_all(pool)
        .await?;
        let channel_rows = raw_channels
            .into_iter()
            .map(|row| ChannelMeta {
                id: row.0,
                name: row.1,
                channel_type: row.2,
                status: row.3,
                quota_currency: row.4,
                quota_remaining: row.5,
                actual_quota_used: row.6,
                updated_at: row.7,
                observed_snapshot_at: row.8,
                observed_pricing_source: row.9,
                observed_pricing_at: row.10,
                observed_price_change_count: row.11,
            })
            .collect::<Vec<_>>();

        let attempts = sqlx::query_as::<
            _,
            (
                i64,
                i64,
                i64,
                i64,
                i64,
                Option<f64>,
                Option<f64>,
                i64,
                i64,
                Option<DateTime<Utc>>,
            ),
        >(OPERATIONS_ATTEMPT_AGGREGATE_SQL)
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;
        let attempt_map = attempts
            .into_iter()
            .map(|row| {
                (
                    row.0,
                    AttemptAggregate {
                        requests: row.1,
                        total: row.2,
                        success: row.3,
                        failed: row.4,
                        avg_latency: row.5,
                        avg_ttft: row.6,
                        ttft_samples: row.7,
                        retries: row.8,
                        last_activity: row.9.map(|value| value.to_rfc3339()),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        let tps_rows = sqlx::query_as::<_, (i64, Option<f64>, i64)>(
            "SELECT ul.channel_id, \
                    AVG(ul.completion_tokens::double precision * 1000.0 / \
                        (execution.metrics_latency_ms - execution.metrics_first_token_latency_ms))::double precision, \
                    COUNT(*)::bigint \
             FROM usage_logs ul JOIN request_executions execution \
               ON execution.id = (SELECT MAX(candidate.id) FROM request_executions candidate \
                 WHERE candidate.request_id = ul.request_id AND candidate.channel_id = ul.channel_id \
                   AND candidate.status = 'completed') \
             WHERE ul.channel_id IS NOT NULL AND ul.created_at >= $1 AND ul.created_at < $2 \
               AND ul.completion_tokens > 0 \
               AND execution.metrics_latency_ms > execution.metrics_first_token_latency_ms \
             GROUP BY ul.channel_id",
        )
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;
        let tps_map = tps_rows
            .into_iter()
            .map(|row| {
                (
                    row.0,
                    TpsAggregate {
                        average: row.1,
                        samples: row.2,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        let error_query = format!(
            "SELECT channel_id, {ERROR_CATEGORY_SQL} AS category, COUNT(*)::bigint \
             FROM request_executions WHERE channel_id IS NOT NULL \
               AND status IN ('failed','canceled') AND created_at >= $1 AND created_at < $2 \
             GROUP BY channel_id, category"
        );
        let error_rows = sqlx::query_as::<_, (i64, String, i64)>(&error_query)
            .bind(start)
            .bind(end)
            .fetch_all(pool)
            .await?;
        let mut error_map: BTreeMap<i64, Vec<(String, i64)>> = BTreeMap::new();
        for (channel_id, category, value) in error_rows {
            error_map
                .entry(channel_id)
                .or_default()
                .push((category, value));
        }

        let route_rows = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                Option<String>,
                i64,
                i64,
                i64,
                Option<DateTime<Utc>>,
            ),
        >(OPERATIONS_ROUTE_HEALTH_SQL)
        .bind(health_start)
        .bind(end)
        .fetch_all(pool)
        .await?;
        let route_error_query = format!(
            "SELECT channel_id,model_id,credential_identity,category,COUNT(*)::bigint \
             FROM (SELECT channel_id,model_id,credential_identity,{ERROR_CATEGORY_SQL} AS category \
                   FROM request_executions WHERE channel_id IS NOT NULL \
                     AND status IN ('failed','canceled') AND created_at >= $1 AND created_at < $2) failures \
             GROUP BY channel_id,model_id,credential_identity,category"
        );
        let route_error_rows =
            sqlx::query_as::<_, (i64, String, Option<String>, String, i64)>(&route_error_query)
                .bind(health_start)
                .bind(end)
                .fetch_all(pool)
                .await?;
        let mut route_errors = BTreeMap::new();
        for (channel_id, model, credential, category, value) in route_error_rows {
            route_errors
                .entry((channel_id, model, credential))
                .or_insert_with(Vec::new)
                .push((category, value));
        }
        let route_health = route_rows
            .into_iter()
            .map(|row| {
                let key = (row.0, row.2.clone(), row.3.clone());
                let errors = route_errors.remove(&key).unwrap_or_default();
                let health = classify_route_health(route_health_sample(row.4, row.5, &errors));
                OperationsRouteHealthRow {
                    channel_id: count(row.0),
                    channel_name: row.1,
                    actual_model: row.2,
                    credential_identity: row.3,
                    health_status: health.as_str().into(),
                    upstream_attempts: count(row.4),
                    successful_attempts: count(row.5),
                    failed_attempts: count(row.6),
                    success_rate: rate(row.5, row.4),
                    error_breakdown: error_buckets(errors),
                    last_activity_at: row.7.map(|value| value.to_rfc3339()),
                }
            })
            .collect();

        let usages = sqlx::query_as::<
            _,
            (
                i64,
                i64,
                i64,
                Option<f64>,
                i64,
                i64,
                i64,
                i64,
                Option<DateTime<Utc>>,
            ),
        >(OPERATIONS_USAGE_AGGREGATE_SQL)
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;
        let usage_map = usages
            .into_iter()
            .map(|row| {
                (
                    row.0,
                    UsageAggregate {
                        rows: row.1,
                        costed: row.2,
                        cost: row.3,
                        input: row.4,
                        output: row.5,
                        cached: row.6,
                        total: row.7,
                        last_activity: row.8.map(|value| value.to_rfc3339()),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        let billings = sqlx::query_as::<_, (i64, i64, i64, i64, Option<f64>)>(
            "SELECT ul.channel_id, \
                    SUM(CASE WHEN ul.api_key_id IS NOT NULL THEN 1 ELSE 0 END)::bigint, \
                    SUM(CASE WHEN cs.status='settled' THEN 1 ELSE 0 END)::bigint, \
                    SUM(CASE WHEN ul.api_key_id IS NOT NULL AND \
                      (cce.id IS NULL OR (cce.status<>'not_billable' AND \
                       (cs.id IS NULL OR cs.status<>'settled'))) THEN 1 ELSE 0 END)::bigint, \
                    SUM(CASE WHEN cs.status='settled' THEN NULLIF(cce.calculation_snapshot->>'accounting_amount','')::numeric ELSE 0::numeric END)::double precision \
             FROM usage_logs ul \
             LEFT JOIN customer_charge_events cce ON cce.usage_log_id=ul.id \
             LEFT JOIN charge_settlements cs ON cs.charge_event_id=cce.id \
             WHERE ul.channel_id IS NOT NULL \
               AND ul.created_at >= $1 AND ul.created_at < $2 GROUP BY ul.channel_id",
        )
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;
        let billing_map = billings
            .into_iter()
            .map(|row| {
                (
                    row.0,
                    BillingAggregate {
                        billable: row.1,
                        settled: row.2,
                        pending: row.3,
                        revenue: row.4,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let probe_map = sqlx::query_as::<_, (i64, i64)>(
            "SELECT channel_id,MAX(timestamp) FROM channel_probes GROUP BY channel_id",
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        let price_map = sqlx::query_as::<_, (i64, i64)>(
            "SELECT channel_id,COUNT(*)::bigint FROM channel_model_prices \
             WHERE deleted_at=0 GROUP BY channel_id",
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect::<BTreeMap<_, _>>();

        let mut channels = Vec::with_capacity(channel_rows.len());
        let mut risks = Vec::new();
        for meta in channel_rows {
            let attempt = attempt_map.get(&meta.id).cloned().unwrap_or_default();
            let usage = usage_map.get(&meta.id).cloned().unwrap_or_default();
            let billing = billing_map.get(&meta.id).cloned().unwrap_or_default();
            let tps = tps_map.get(&meta.id).cloned().unwrap_or_default();
            let cost = cost_metric(usage.cost, usage.costed, usage.rows);
            let revenue = revenue_metric(billing.revenue, billing.settled, billing.billable);
            let (profit, gross_margin) = profit_metric(&revenue, &cost);
            let success_rate = rate(attempt.success, attempt.total);
            let last_probe = probe_map
                .get(&meta.id)
                .and_then(|timestamp| DateTime::from_timestamp(*timestamp, 0))
                .map(|timestamp| timestamp.to_rfc3339());
            let last_activity = [attempt.last_activity.clone(), usage.last_activity.clone()]
                .into_iter()
                .flatten()
                .max();
            let quota_snapshot_at = meta
                .observed_snapshot_at
                .map(|value| value.to_rfc3339())
                .or_else(|| {
                    (meta.quota_remaining.is_some() || meta.actual_quota_used.is_some())
                        .then(|| meta.updated_at.to_rfc3339())
                });
            append_risks(
                &mut risks,
                &meta,
                &attempt,
                &usage,
                &billing,
                success_rate,
                quota_snapshot_at.as_deref(),
                probe_map.get(&meta.id).copied(),
                price_map.get(&meta.id).copied().unwrap_or(0),
                days,
                end,
            );
            channels.push(OperationsChannelRow {
                channel_id: count(meta.id),
                channel_name: meta.name,
                channel_type: meta.channel_type,
                channel_status: meta.status,
                customer_requests: count(attempt.requests),
                upstream_attempts: count(attempt.total),
                successful_attempts: count(attempt.success),
                failed_attempts: count(attempt.failed),
                success_rate,
                retry_count: count(attempt.retries),
                input_tokens: count(usage.input),
                output_tokens: count(usage.output),
                cached_tokens: count(usage.cached),
                total_tokens: count(usage.total),
                average_latency_ms: attempt.avg_latency,
                average_ttft_ms: attempt.avg_ttft,
                ttft_sample_count: count(attempt.ttft_samples),
                average_tps: tps.average,
                tps_sample_count: count(tps.samples),
                error_breakdown: error_buckets(error_map.remove(&meta.id).unwrap_or_default()),
                recorded_upstream_cost: cost.clone(),
                recognized_usage_revenue: revenue,
                gross_profit: profit,
                gross_margin,
                cost_per_attempt: if cost.quality == "EXACT" && attempt.total > 0 {
                    cost.amount.map(|amount| amount / attempt.total as f64)
                } else {
                    None
                },
                usage_rows: count(usage.rows),
                costed_usage_rows: count(usage.costed),
                settled_usage_rows: count(billing.settled),
                pending_charge_rows: count(billing.pending),
                cost_coverage_rate: rate(usage.costed, usage.rows),
                billing_coverage_rate: rate(billing.settled, billing.billable),
                quota_currency: meta.quota_currency,
                quota_remaining: meta.quota_remaining,
                actual_quota_used: meta.actual_quota_used,
                quota_snapshot_at,
                observed_pricing_source: meta.observed_pricing_source,
                observed_pricing_at: meta.observed_pricing_at.map(|value| value.to_rfc3339()),
                observed_price_change_count: count(meta.observed_price_change_count),
                last_probe_at: last_probe,
                last_activity_at: last_activity,
            });
        }
        channels.sort_by(|left, right| {
            right
                .upstream_attempts
                .cmp(&left.upstream_attempts)
                .then_with(|| left.channel_name.cmp(&right.channel_name))
        });
        risks.sort_by_key(|risk| match risk.severity.as_str() {
            "critical" => 0,
            "warning" => 1,
            _ => 2,
        });

        let customer_requests = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(DISTINCT request_id)::bigint FROM request_executions \
             WHERE created_at >= $1 AND created_at < $2",
        )
        .bind(start)
        .bind(end)
        .fetch_one(pool)
        .await?;
        let total_attempts = attempt_map.values().map(|row| row.total).sum::<i64>();
        let successful_attempts = attempt_map.values().map(|row| row.success).sum::<i64>();
        let failed_attempts = attempt_map.values().map(|row| row.failed).sum::<i64>();
        let retry_count = attempt_map.values().map(|row| row.retries).sum::<i64>();
        let ttft_sample_count = attempt_map
            .values()
            .map(|row| row.ttft_samples)
            .sum::<i64>();
        let average_ttft_ms = weighted_average(
            attempt_map
                .values()
                .map(|row| (row.avg_ttft, row.ttft_samples)),
        );
        let tps_sample_count = tps_map.values().map(|row| row.samples).sum::<i64>();
        let average_tps = weighted_average(tps_map.values().map(|row| (row.average, row.samples)));
        let summary_errors = error_buckets(
            channels
                .iter()
                .flat_map(|channel| {
                    channel
                        .error_breakdown
                        .iter()
                        .map(|bucket| (bucket.category.clone(), i64::from(bucket.count)))
                })
                .fold(
                    BTreeMap::<String, i64>::new(),
                    |mut totals, (category, value)| {
                        *totals.entry(category).or_default() += value;
                        totals
                    },
                ),
        );
        let usage_rows = usage_map.values().map(|row| row.rows).sum::<i64>();
        let costed_rows = usage_map.values().map(|row| row.costed).sum::<i64>();
        let settled_rows = billing_map.values().map(|row| row.settled).sum::<i64>();
        let billable_rows = billing_map.values().map(|row| row.billable).sum::<i64>();
        let pending_rows = billing_map.values().map(|row| row.pending).sum::<i64>();
        let total_cost = usage_map.values().filter_map(|row| row.cost).sum::<f64>();
        let cost = cost_metric(
            (costed_rows > 0).then_some(total_cost),
            costed_rows,
            usage_rows,
        );
        let total_revenue = billing_map
            .values()
            .filter_map(|row| row.revenue)
            .sum::<f64>();
        let revenue = revenue_metric(
            (settled_rows > 0).then_some(total_revenue),
            settled_rows,
            billable_rows,
        );
        let (gross_profit, gross_margin) = profit_metric(&revenue, &cost);
        let trend = self.load_trend(pool, start, end, days).await?;
        Ok(OperationsLedger {
            generated_at: end.to_rfc3339(),
            period_start: start.to_rfc3339(),
            period_end: end.to_rfc3339(),
            period_days: days,
            summary: OperationsLedgerSummary {
                customer_requests: count(customer_requests),
                upstream_attempts: count(total_attempts),
                successful_attempts: count(successful_attempts),
                failed_attempts: count(failed_attempts),
                success_rate: rate(successful_attempts, total_attempts),
                retry_count: count(retry_count),
                average_ttft_ms,
                ttft_sample_count: count(ttft_sample_count),
                average_tps,
                tps_sample_count: count(tps_sample_count),
                error_breakdown: summary_errors,
                input_tokens: count(usage_map.values().map(|row| row.input).sum()),
                output_tokens: count(usage_map.values().map(|row| row.output).sum()),
                cached_tokens: count(usage_map.values().map(|row| row.cached).sum()),
                total_tokens: count(usage_map.values().map(|row| row.total).sum()),
                recorded_upstream_cost: cost,
                recognized_usage_revenue: revenue,
                gross_profit,
                gross_margin,
            },
            coverage: OperationsAccountingCoverage {
                usage_rows: count(usage_rows),
                costed_usage_rows: count(costed_rows),
                settled_usage_rows: count(settled_rows),
                pending_charge_rows: count(pending_rows),
                cost_coverage_rate: rate(costed_rows, usage_rows),
                billing_coverage_rate: rate(settled_rows, billable_rows),
                cost_complete: usage_rows == costed_rows,
                billing_complete: billable_rows == settled_rows,
            },
            trend,
            channels,
            route_health,
            risks,
            accounting_scope_note: CASH_ACCOUNTING_NOTE.into(),
        })
    }

    async fn load_trend(
        &self,
        pool: &PgPool,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        days: i32,
    ) -> Result<Vec<OperationsTrendPoint>, sqlx::Error> {
        let attempts = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i64, Option<f64>, i64)>(
            "SELECT bucket,COUNT(*)::bigint, \
                    SUM(CASE WHEN successful_attempts=0 AND failed_attempts>0 THEN 1 ELSE 0 END)::bigint, \
                    SUM(upstream_attempts)::bigint,SUM(successful_attempts)::bigint,SUM(failed_attempts)::bigint, \
                    SUM(upstream_attempts - 1)::bigint,SUM(ttft_sum)::double precision,SUM(ttft_samples)::bigint \
             FROM (SELECT FLOOR(EXTRACT(EPOCH FROM (created_at - $1))/86400)::bigint AS bucket, \
                          request_id,COUNT(*)::bigint AS upstream_attempts, \
                          SUM(CASE WHEN status='completed' THEN 1 ELSE 0 END)::bigint AS successful_attempts, \
                          SUM(CASE WHEN status IN ('failed','canceled') THEN 1 ELSE 0 END)::bigint AS failed_attempts, \
                          SUM(metrics_first_token_latency_ms)::double precision AS ttft_sum, \
                          COUNT(metrics_first_token_latency_ms)::bigint AS ttft_samples \
                   FROM request_executions WHERE created_at >= $1 AND created_at < $2 \
                   GROUP BY bucket,request_id) requests \
             GROUP BY bucket",
        )
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;
        let usage = sqlx::query_as::<_, (i64, i64, Option<f64>)>(
            "SELECT FLOOR(EXTRACT(EPOCH FROM (created_at - $1))/86400)::bigint, \
                    COUNT(total_cost)::bigint,SUM(total_cost) \
             FROM usage_logs WHERE created_at >= $1 AND created_at < $2 GROUP BY 1",
        )
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;
        let revenue = sqlx::query_as::<_, (i64, f64)>(
            "SELECT FLOOR(EXTRACT(EPOCH FROM (ul.created_at - $1))/86400)::bigint, \
                    COALESCE(SUM(NULLIF(cce.calculation_snapshot->>'accounting_amount','')::numeric),0)::double precision \
             FROM usage_logs ul \
             JOIN customer_charge_events cce ON cce.usage_log_id=ul.id \
             JOIN charge_settlements cs ON cs.charge_event_id=cce.id AND cs.status='settled' \
             WHERE ul.created_at >= $1 AND ul.created_at < $2 GROUP BY 1",
        )
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;
        let attempt_map = attempts
            .into_iter()
            .map(|row| {
                (
                    row.0,
                    (row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let usage_map = usage
            .into_iter()
            .map(|row| (row.0, (row.1, row.2)))
            .collect::<BTreeMap<_, _>>();
        let revenue_map = revenue.into_iter().collect::<BTreeMap<_, _>>();
        let tps = sqlx::query_as::<_, (i64, Option<f64>)>(
            "SELECT FLOOR(EXTRACT(EPOCH FROM (ul.created_at - $1))/86400)::bigint, \
                    AVG(ul.completion_tokens::double precision * 1000.0 / \
                        (execution.metrics_latency_ms - execution.metrics_first_token_latency_ms))::double precision \
             FROM usage_logs ul JOIN request_executions execution \
               ON execution.id = (SELECT MAX(candidate.id) FROM request_executions candidate \
                 WHERE candidate.request_id = ul.request_id AND candidate.channel_id = ul.channel_id \
                   AND candidate.status = 'completed') \
             WHERE ul.created_at >= $1 AND ul.created_at < $2 AND ul.completion_tokens > 0 \
               AND execution.metrics_latency_ms > execution.metrics_first_token_latency_ms \
             GROUP BY 1",
        )
        .bind(start)
        .bind(end)
        .fetch_all(pool)
        .await?;
        let tps_map = tps.into_iter().collect::<BTreeMap<_, _>>();
        let mut points = Vec::with_capacity(days as usize);
        for bucket in 0..i64::from(days) {
            // The API exposes rolling 24h/7d/30d windows. Label each complete
            // 24-hour bucket by its end date so all data in [start, end) is
            // represented without creating an extra calendar-date point.
            let label = rolling_bucket_label(start, bucket);
            let attempt = attempt_map.get(&bucket).copied().unwrap_or_default();
            let cost = usage_map
                .get(&bucket)
                .and_then(|row| (row.0 > 0).then_some(row.1).flatten());
            let revenue = revenue_map.get(&bucket).copied();
            points.push(OperationsTrendPoint {
                date: label,
                customer_requests: count(attempt.0),
                failed_customer_requests: count(attempt.1),
                request_failure_rate: rate(attempt.1, attempt.0),
                upstream_attempts: count(attempt.2),
                successful_attempts: count(attempt.3),
                failed_attempts: count(attempt.4),
                failure_rate: rate(attempt.4, attempt.2),
                retry_count: count(attempt.5),
                average_ttft_ms: attempt
                    .6
                    .and_then(|sum| (attempt.7 > 0).then_some(sum / attempt.7 as f64)),
                average_tps: tps_map.get(&bucket).copied().flatten(),
                recorded_upstream_cost: cost,
                recognized_usage_revenue: revenue,
                gross_profit: revenue.zip(cost).map(|(income, expense)| income - expense),
            });
        }
        Ok(points)
    }
}

#[allow(clippy::too_many_arguments)]
fn append_risks(
    risks: &mut Vec<OperationsRisk>,
    meta: &ChannelMeta,
    attempt: &AttemptAggregate,
    usage: &UsageAggregate,
    billing: &BillingAggregate,
    success_rate: Option<f64>,
    quota_snapshot_at: Option<&str>,
    last_probe: Option<i64>,
    purchase_price_count: i64,
    days: i32,
    now: DateTime<Utc>,
) {
    let mut push = |code: &str,
                    severity: &str,
                    affected_count: Option<i32>,
                    total_count: Option<i32>,
                    observed_value: Option<f64>,
                    threshold_value: Option<f64>,
                    period_days: Option<i32>| {
        risks.push(OperationsRisk {
            code: code.into(),
            severity: severity.into(),
            channel_id: Some(count(meta.id)),
            channel_name: Some(meta.name.clone()),
            affected_count,
            total_count,
            observed_value,
            threshold_value,
            period_days,
        });
    };
    if usage.rows > usage.costed {
        push(
            "HISTORICAL_COST_INCOMPLETE",
            "critical",
            Some(count(usage.rows - usage.costed)),
            Some(count(usage.rows)),
            rate(usage.costed, usage.rows),
            Some(1.0),
            Some(days),
        );
    }
    if meta.status == "enabled" && purchase_price_count == 0 {
        push(
            "PURCHASE_PRICE_NOT_CONFIGURED",
            "warning",
            None,
            None,
            None,
            None,
            None,
        );
    }
    if attempt.total >= 5 && success_rate.unwrap_or(1.0) < 0.95 {
        push(
            "HIGH_ERROR_RATE",
            "warning",
            Some(count(attempt.failed)),
            Some(count(attempt.total)),
            success_rate,
            Some(0.95),
            Some(days),
        );
    }
    if meta.status == "enabled" && attempt.total == 0 {
        push(
            "NO_TRAFFIC",
            "info",
            Some(0),
            Some(0),
            None,
            None,
            Some(days),
        );
    }
    if meta.status == "enabled" && last_probe.is_none_or(|value| now.timestamp() - value > 86_400) {
        push(
            "STALE_HEALTH_PROBE",
            "warning",
            None,
            None,
            None,
            Some(24.0),
            None,
        );
    }
    let remaining = meta
        .quota_remaining
        .as_deref()
        .and_then(|value| value.parse::<f64>().ok());
    if remaining.is_some_and(|value| value <= 10.0) {
        push(
            "LOW_PROVIDER_QUOTA",
            "critical",
            None,
            None,
            remaining,
            Some(10.0),
            None,
        );
    }
    if (meta.quota_remaining.is_some() || meta.actual_quota_used.is_some())
        && quota_snapshot_at.is_none_or(|value| timestamp_is_stale(value, now, Duration::hours(24)))
    {
        push(
            "STALE_QUOTA_SNAPSHOT",
            "warning",
            None,
            None,
            None,
            Some(24.0),
            None,
        );
    }
    if billing.pending > 0 {
        push(
            "PENDING_CHARGE_ROWS",
            "warning",
            Some(count(billing.pending)),
            Some(count(usage.rows)),
            None,
            None,
            Some(days),
        );
    }
    if meta.observed_pricing_source.is_some()
        && meta
            .observed_pricing_at
            .is_none_or(|value| now - value > Duration::hours(6))
    {
        push(
            "UPSTREAM_PRICE_STALE",
            "warning",
            None,
            None,
            None,
            Some(6.0),
            None,
        );
    }
    if meta.observed_price_change_count > 0 {
        push(
            "UPSTREAM_PRICE_CHANGED",
            "warning",
            Some(count(meta.observed_price_change_count)),
            None,
            None,
            None,
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use sqlx::postgres::PgPoolOptions;
    use std::collections::BTreeSet;

    #[derive(Debug, Default)]
    struct ExplainReport {
        planning_ms: f64,
        execution_ms: f64,
        shared_hit_blocks: u64,
        shared_read_blocks: u64,
        temp_written_blocks: u64,
        node_types: BTreeSet<String>,
        indexes: BTreeSet<String>,
        relations: BTreeSet<String>,
    }

    fn visit_explain_node(value: &serde_json::Value, report: &mut ExplainReport) {
        if let Some(node_type) = value.get("Node Type").and_then(serde_json::Value::as_str) {
            report.node_types.insert(node_type.to_owned());
        }
        if let Some(index) = value.get("Index Name").and_then(serde_json::Value::as_str) {
            report.indexes.insert(index.to_owned());
        }
        if let Some(relation) = value
            .get("Relation Name")
            .and_then(serde_json::Value::as_str)
        {
            report.relations.insert(relation.to_owned());
        }
        report.shared_hit_blocks += value
            .get("Shared Hit Blocks")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        report.shared_read_blocks += value
            .get("Shared Read Blocks")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        report.temp_written_blocks += value
            .get("Temp Written Blocks")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if let Some(children) = value.get("Plans").and_then(serde_json::Value::as_array) {
            for child in children {
                visit_explain_node(child, report);
            }
        }
    }

    fn explain_report(value: &serde_json::Value) -> Result<ExplainReport, &'static str> {
        let statement = value
            .as_array()
            .and_then(|statements| statements.first())
            .ok_or("EXPLAIN JSON did not contain a statement")?;
        let mut report = ExplainReport {
            planning_ms: statement
                .get("Planning Time")
                .and_then(serde_json::Value::as_f64)
                .ok_or("EXPLAIN JSON did not contain Planning Time")?,
            execution_ms: statement
                .get("Execution Time")
                .and_then(serde_json::Value::as_f64)
                .ok_or("EXPLAIN JSON did not contain Execution Time")?,
            ..ExplainReport::default()
        };
        visit_explain_node(
            statement
                .get("Plan")
                .ok_or("EXPLAIN JSON did not contain Plan")?,
            &mut report,
        );
        Ok(report)
    }

    fn benchmark_setting(name: &str, default: u64, maximum: u64) -> Result<u64, String> {
        let value = match std::env::var(name) {
            Ok(raw) => raw
                .parse::<u64>()
                .map_err(|error| format!("{name} must be an integer: {error}"))?,
            Err(_) => default,
        };
        if value == 0 || value > maximum {
            return Err(format!("{name} must be between 1 and {maximum}"));
        }
        Ok(value)
    }

    async fn explain_operations_query(
        connection: &mut sqlx::PgConnection,
        name: &str,
        sql: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        expected_relation: &str,
        max_ms: f64,
    ) -> Result<ExplainReport, Box<dyn std::error::Error>> {
        let explain_sql = format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {sql}");
        let value: sqlx::types::Json<serde_json::Value> = sqlx::query_scalar(&explain_sql)
            .bind(start)
            .bind(end)
            .fetch_one(&mut *connection)
            .await?;
        let report = explain_report(&value.0)?;
        assert!(
            report.relations.contains(expected_relation),
            "{name} plan did not access {expected_relation}: {report:?}"
        );
        assert!(
            report.execution_ms <= max_ms,
            "{name} execution exceeded {max_ms} ms: {report:?}"
        );
        assert!(
            report.planning_ms.is_finite(),
            "{name} returned an invalid planning duration: {report:?}"
        );
        assert_eq!(
            report.temp_written_blocks, 0,
            "{name} spilled temporary blocks with the controlled fixture: {report:?}"
        );
        println!("postgres operations plan {name}: {report:?}");
        Ok(report)
    }

    #[test]
    fn rolling_bucket_labels_cover_seven_24_hour_buckets_without_an_extra_date() {
        let end = Utc
            .with_ymd_and_hms(2026, 8, 16, 12, 34, 56)
            .single()
            .expect("valid timestamp");
        let start = end - Duration::days(7);
        let labels = (0..7)
            .map(|bucket| rolling_bucket_label(start, bucket))
            .collect::<Vec<_>>();

        assert_eq!(labels.len(), 7);
        assert_eq!(labels.first().map(String::as_str), Some("2026-08-10"));
        assert_eq!(labels.last().map(String::as_str), Some("2026-08-16"));
    }

    #[tokio::test]
    async fn operations_falls_back_to_master_when_read_pool_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let unavailable = PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy("postgres://127.0.0.1:1/unreachable")?;
        let adapter =
            PgOperationsAdapter::new(database.pool.clone()).with_read_pool(Some(unavailable), true);

        let ledger = adapter.operations_ledger(7).await?;
        assert_eq!(ledger.trend.len(), 7);
        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_operations_uses_the_same_rolling_window_for_summary_and_trend()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model) \
             VALUES('openai','period-window','enabled','{}'::jsonb,'[]'::jsonb,'') RETURNING id",
        )
        .fetch_one(&pool)
        .await?;
        let end = Utc
            .with_ymd_and_hms(2026, 8, 16, 12, 0, 0)
            .single()
            .ok_or("invalid time")?;
        let start = end - Duration::days(7);

        for (request_id, created_at) in [
            (1001_i64, start - Duration::seconds(1)),
            (1002, start + Duration::seconds(1)),
            (1003, end - Duration::seconds(1)),
            (1004, end),
        ] {
            sqlx::query(
                "INSERT INTO request_executions( \
                   project_id,request_id,channel_id,model_id,request_body,status,created_at,updated_at) \
                 VALUES(1,$1,$2,'period-model','{}'::jsonb,'completed',$3,$3)",
            )
            .bind(request_id)
            .bind(channel_id)
            .bind(created_at)
            .execute(&pool)
            .await?;
        }

        let ledger = PgOperationsAdapter::new(pool)
            .with_now(end)
            .operations_ledger(7)
            .await?;

        assert_eq!(ledger.period_start, start.to_rfc3339());
        assert_eq!(ledger.period_end, end.to_rfc3339());
        assert_eq!(ledger.trend.len(), 7);
        assert_eq!(ledger.summary.customer_requests, 2);
        assert_eq!(ledger.summary.upstream_attempts, 2);
        assert_eq!(ledger.trend[0].customer_requests, 1);
        assert_eq!(ledger.trend[6].customer_requests, 1);
        assert_eq!(
            ledger
                .trend
                .iter()
                .map(|point| point.customer_requests)
                .sum::<i32>(),
            ledger.summary.customer_requests
        );
        assert_eq!(
            ledger
                .trend
                .iter()
                .map(|point| point.upstream_attempts)
                .sum::<i32>(),
            ledger.summary.upstream_attempts
        );

        database.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_operations_reports_cost_revenue_profit_and_failures_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let channel_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model) \
             VALUES('openai',$1,'enabled','{}'::jsonb,'[]'::jsonb,'') RETURNING id",
        )
        .bind(format!("pg-operations-{suffix}"))
        .fetch_one(&pool)
        .await?;
        let request_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO requests(project_id,model_id,request_body,status) \
             VALUES(1,'operations-model','{}'::jsonb,'completed') RETURNING id",
        )
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO request_executions(project_id,request_id,channel_id,credential_identity,model_id,request_body,status,response_status_code,error_message) \
             VALUES(1,$1,$2,'sha256:test-target','operations-model','{}'::jsonb,'failed',429,'rate limit')",
        )
        .bind(request_id)
        .bind(channel_id)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO request_executions(project_id,request_id,channel_id,credential_identity,model_id,request_body,status,response_status_code,metrics_latency_ms,metrics_first_token_latency_ms) \
             VALUES(1,$1,$2,'sha256:test-target','operations-model','{}'::jsonb,'completed',200,140,40)",
        )
        .bind(request_id)
        .bind(channel_id)
        .execute(&pool)
        .await?;
        let usage_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO usage_logs(request_id,api_key_id,channel_id,project_id,model_id, \
                                    prompt_tokens,completion_tokens,total_tokens,total_cost) \
             VALUES($1,1,$2,1,'operations-model',10,5,15,0.25) RETURNING id",
        )
        .bind(request_id)
        .bind(channel_id)
        .fetch_one(&pool)
        .await?;
        let charge_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO customer_charge_events(usage_log_id,request_id,amount,currency, \
              applied_rules_snapshot,usage_snapshot,calculation_snapshot,status) \
             VALUES($1,$2,15000.0,'STATION_CREDIT','{}'::jsonb,'{}'::jsonb, \
                    '{\"accounting_amount\":\"1.5\",\"accounting_currency_code\":\"CNY\",\"final_credit_amount\":\"15000\"}'::jsonb, \
                    'settled') RETURNING id",
        )
        .bind(usage_id)
        .bind(request_id)
        .fetch_one(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO charge_settlements(charge_event_id,amount_micros,subscription_amount_micros, \
                                            credit_amount_micros,status,detail_snapshot,created_at) \
             VALUES($1,15000000000,15000000000,0,'settled','{}'::jsonb,now())",
        )
        .bind(charge_id)
        .execute(&pool)
        .await?;
        let adapter = PgOperationsAdapter::new(pool.clone());
        let ledger = adapter.operations_ledger(7).await?;
        let channel = ledger
            .channels
            .iter()
            .find(|row| i64::from(row.channel_id) == channel_id)
            .ok_or("operations channel missing")?;
        assert_eq!(channel.failed_attempts, 1);
        assert_eq!(channel.retry_count, 1);
        assert_eq!(channel.average_ttft_ms, Some(40.0));
        assert_eq!(channel.average_tps, Some(50.0));
        assert_eq!(channel.tps_sample_count, 1);
        assert_eq!(
            channel.error_breakdown,
            vec![conduit_admin_graphql::operations::OperationsErrorBucket {
                category: "rate_limit".into(),
                count: 1,
            }]
        );
        assert_eq!(channel.recorded_upstream_cost.amount, Some(0.25));
        assert_eq!(channel.recognized_usage_revenue.amount, Some(1.5));
        assert_eq!(channel.gross_profit.amount, Some(1.25));
        assert_eq!(ledger.summary.recognized_usage_revenue.amount, Some(1.5));
        assert_eq!(ledger.summary.gross_profit.amount, Some(1.25));
        assert_eq!(
            ledger
                .trend
                .iter()
                .filter_map(|point| point.recognized_usage_revenue)
                .sum::<f64>(),
            1.5
        );
        let target = ledger
            .route_health
            .iter()
            .find(|row| i64::from(row.channel_id) == channel_id)
            .ok_or("route health target missing")?;
        assert_eq!(target.actual_model, "operations-model");
        assert_eq!(
            target.credential_identity.as_deref(),
            Some("sha256:test-target")
        );
        assert_eq!(target.health_status, "degraded");
        let flow = adapter.operations_flow(7, 50).await?;
        let flow_row = flow
            .rows
            .iter()
            .find(|row| row.channel_id == Some(count(channel_id)))
            .ok_or("operations flow row missing")?;
        assert_eq!(flow_row.requested_model, "operations-model");
        assert_eq!(flow_row.actual_model, "operations-model");
        assert_eq!(flow_row.total_tokens, 15);
        assert_eq!(flow_row.recognized_usage_revenue, Some(1.5));
        let model_series = adapter.operations_model_series(7).await?;
        let model_point = model_series
            .points
            .iter()
            .find(|point| point.requested_model == "operations-model")
            .ok_or("operations model series point missing")?;
        assert_eq!(model_point.recognized_usage_revenue, Some(1.5));
        database.cleanup().await?;
        Ok(())
    }

    /// Opt-in plan baseline for the actual SQL used by `load_from`.
    ///
    /// This deliberately records the planner's decision instead of requiring
    /// a named index: sequential scans can be correct at small cardinalities,
    /// and an index should only be added after this fixture demonstrates a
    /// measured regression. See `docs/postgres-performance-baseline.md`.
    #[tokio::test]
    #[ignore = "opt-in representative PostgreSQL Operations plan benchmark"]
    async fn postgres_operations_representative_plan_benchmark_when_explicitly_enabled()
    -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("CONDUIT_PG_BENCH").as_deref() != Ok("1") {
            return Ok(());
        }
        let dsn = std::env::var("CONDUIT_TEST_POSTGRES_DSN").map_err(|_| {
            std::io::Error::other("CONDUIT_TEST_POSTGRES_DSN is required when CONDUIT_PG_BENCH=1")
        })?;
        let rows = benchmark_setting("CONDUIT_PG_BENCH_OPERATIONS_ROWS", 20_000, 250_000)?;
        let channels = benchmark_setting("CONDUIT_PG_BENCH_OPERATIONS_CHANNELS", 16, 128)?;
        let max_ms = benchmark_setting("CONDUIT_PG_BENCH_OPERATIONS_MAX_MS", 10_000, 120_000)?;
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let pool = database.pool.clone();

        sqlx::query(
            "INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model) \
             SELECT 'openai','operations-bench-' || series,'enabled','{}'::jsonb,'[]'::jsonb,'' \
             FROM generate_series(1,$1::bigint) series",
        )
        .bind(channels as i64)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO request_executions( \
               project_id,request_id,channel_id,credential_identity,model_id,request_body,status, \
               response_status_code,metrics_latency_ms,metrics_first_token_latency_ms,created_at,updated_at) \
             SELECT (series % 100) + 1,((series - 1) / 2) + 1, \
                    ((((series - 1) / 2) % $2) + 1), \
                    'sha256:bench-' || (series % 4), \
                    'operations-model-' || (series % 8),'{}'::jsonb, \
                    CASE WHEN series % 10 = 0 THEN 'failed' ELSE 'completed' END, \
                    CASE WHEN series % 10 = 0 THEN 429 ELSE 200 END, \
                    100 + (series % 400),20 + (series % 80), \
                    now() - ((CASE WHEN series % 5 = 0 THEN series % 900 \
                                   ELSE series % 1209600 END)::text || ' seconds')::interval,now() \
             FROM generate_series(1,$1::bigint) series",
        )
        .bind(rows as i64)
        .bind(channels as i64)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO usage_logs( \
               request_id,api_key_id,channel_id,project_id,model_id,prompt_tokens, \
               completion_tokens,prompt_cached_tokens,total_tokens,total_cost,created_at,updated_at) \
             SELECT series,1,(((series - 1) % $2) + 1),(series % 100) + 1, \
                    'operations-model-' || ((series * 2) % 8),100,40,10,140,0.02, \
                    now() - (((series * 2) % 1209600)::text || ' seconds')::interval,now() \
             FROM generate_series(1,$1::bigint) series",
        )
        .bind((rows / 2) as i64)
        .bind(channels as i64)
        .execute(&pool)
        .await?;
        sqlx::query("ANALYZE request_executions")
            .execute(&pool)
            .await?;
        sqlx::query("ANALYZE usage_logs").execute(&pool).await?;
        sqlx::query("ANALYZE channels").execute(&pool).await?;

        let ledger_started = std::time::Instant::now();
        let ledger = PgOperationsAdapter::new(pool.clone())
            .operations_ledger(7)
            .await?;
        let ledger_ms = ledger_started.elapsed().as_secs_f64() * 1000.0;
        assert!(ledger.summary.upstream_attempts > 0);
        assert_eq!(ledger.channels.len(), channels as usize);
        assert!(
            ledger_ms <= max_ms as f64 * 4.0,
            "complete Operations load exceeded {} ms: {ledger_ms:.3} ms",
            max_ms * 4
        );
        println!(
            "postgres operations full load: rows={rows} channels={channels} elapsed_ms={ledger_ms:.3}"
        );

        let end = Utc::now();
        let start = end - Duration::days(7);
        let health_start = end - Duration::minutes(15);
        let mut connection = pool.acquire().await?;
        sqlx::query(&format!("SET statement_timeout = {max_ms}"))
            .execute(&mut *connection)
            .await?;
        explain_operations_query(
            &mut connection,
            "attempt_aggregate",
            OPERATIONS_ATTEMPT_AGGREGATE_SQL,
            start,
            end,
            "request_executions",
            max_ms as f64,
        )
        .await?;
        explain_operations_query(
            &mut connection,
            "route_health",
            OPERATIONS_ROUTE_HEALTH_SQL,
            health_start,
            end,
            "request_executions",
            max_ms as f64,
        )
        .await?;
        explain_operations_query(
            &mut connection,
            "usage_aggregate",
            OPERATIONS_USAGE_AGGREGATE_SQL,
            start,
            end,
            "usage_logs",
            max_ms as f64,
        )
        .await?;

        drop(connection);
        database.cleanup().await?;
        Ok(())
    }
}
