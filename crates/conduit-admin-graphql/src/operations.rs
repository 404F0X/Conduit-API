//! Administrator operations ledger.
//!
//! This slice deliberately reports usage economics, not cash accounting. The
//! host adapter reads immutable usage cost and successfully funded customer
//! settlements. Credit grants and subscription allowances remain funding
//! sources rather than cash receipts.

use std::sync::Arc;

use async_graphql::{Context, SimpleObject};

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsLedgerSummary")]
pub struct OperationsLedgerSummary {
    pub customer_requests: i32,
    pub upstream_attempts: i32,
    pub successful_attempts: i32,
    pub failed_attempts: i32,
    pub success_rate: Option<f64>,
    pub retry_count: i32,
    pub average_ttft_ms: Option<f64>,
    pub ttft_sample_count: i32,
    pub average_tps: Option<f64>,
    pub tps_sample_count: i32,
    pub error_breakdown: Vec<OperationsErrorBucket>,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cached_tokens: i32,
    pub total_tokens: i32,
    pub recorded_upstream_cost: OperationsMoneyMetric,
    pub recognized_usage_revenue: OperationsMoneyMetric,
    pub gross_profit: OperationsMoneyMetric,
    pub gross_margin: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsMoneyMetric")]
pub struct OperationsMoneyMetric {
    pub amount: Option<f64>,
    /// EXACT, PARTIAL or UNAVAILABLE.
    pub quality: String,
    pub coverage_rate: Option<f64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsAccountingCoverage")]
pub struct OperationsAccountingCoverage {
    pub usage_rows: i32,
    pub costed_usage_rows: i32,
    pub settled_usage_rows: i32,
    pub pending_charge_rows: i32,
    pub cost_coverage_rate: Option<f64>,
    pub billing_coverage_rate: Option<f64>,
    pub cost_complete: bool,
    pub billing_complete: bool,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsTrendPoint")]
pub struct OperationsTrendPoint {
    pub date: String,
    pub customer_requests: i32,
    pub failed_customer_requests: i32,
    pub request_failure_rate: Option<f64>,
    pub upstream_attempts: i32,
    pub successful_attempts: i32,
    pub failed_attempts: i32,
    pub failure_rate: Option<f64>,
    pub retry_count: i32,
    pub average_ttft_ms: Option<f64>,
    pub average_tps: Option<f64>,
    pub recorded_upstream_cost: Option<f64>,
    pub recognized_usage_revenue: Option<f64>,
    pub gross_profit: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsChannelRow")]
pub struct OperationsChannelRow {
    pub channel_id: i32,
    pub channel_name: String,
    pub channel_type: String,
    pub channel_status: String,
    pub customer_requests: i32,
    pub upstream_attempts: i32,
    pub successful_attempts: i32,
    pub failed_attempts: i32,
    pub success_rate: Option<f64>,
    pub retry_count: i32,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cached_tokens: i32,
    pub total_tokens: i32,
    pub average_latency_ms: Option<f64>,
    pub average_ttft_ms: Option<f64>,
    pub ttft_sample_count: i32,
    pub average_tps: Option<f64>,
    pub tps_sample_count: i32,
    pub error_breakdown: Vec<OperationsErrorBucket>,
    pub recorded_upstream_cost: OperationsMoneyMetric,
    pub recognized_usage_revenue: OperationsMoneyMetric,
    pub gross_profit: OperationsMoneyMetric,
    pub gross_margin: Option<f64>,
    pub cost_per_attempt: Option<f64>,
    pub usage_rows: i32,
    pub costed_usage_rows: i32,
    pub settled_usage_rows: i32,
    pub pending_charge_rows: i32,
    pub cost_coverage_rate: Option<f64>,
    pub billing_coverage_rate: Option<f64>,
    pub quota_currency: Option<String>,
    pub quota_remaining: Option<String>,
    pub actual_quota_used: Option<String>,
    pub quota_snapshot_at: Option<String>,
    pub observed_pricing_source: Option<String>,
    pub observed_pricing_at: Option<String>,
    pub observed_price_change_count: i32,
    pub last_probe_at: Option<String>,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsRouteHealthRow")]
pub struct OperationsRouteHealthRow {
    pub channel_id: i32,
    pub channel_name: String,
    /// Provider-facing model selected for this upstream attempt.
    pub actual_model: String,
    /// One-way credential fingerprint. This is never the provider secret.
    pub credential_identity: Option<String>,
    /// healthy, degraded, unhealthy, or unknown.
    pub health_status: String,
    pub upstream_attempts: i32,
    pub successful_attempts: i32,
    pub failed_attempts: i32,
    pub success_rate: Option<f64>,
    pub error_breakdown: Vec<OperationsErrorBucket>,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsErrorBucket")]
pub struct OperationsErrorBucket {
    /// Stable machine category: auth, rate_limit, timeout, upstream_5xx,
    /// connection, canceled, configuration, or unknown.
    pub category: String,
    pub count: i32,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsRisk")]
pub struct OperationsRisk {
    pub code: String,
    pub severity: String,
    pub channel_id: Option<i32>,
    pub channel_name: Option<String>,
    pub affected_count: Option<i32>,
    pub total_count: Option<i32>,
    pub observed_value: Option<f64>,
    pub threshold_value: Option<f64>,
    pub period_days: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsLedger")]
pub struct OperationsLedger {
    pub generated_at: String,
    pub period_start: String,
    pub period_end: String,
    pub period_days: i32,
    pub summary: OperationsLedgerSummary,
    pub coverage: OperationsAccountingCoverage,
    pub trend: Vec<OperationsTrendPoint>,
    pub channels: Vec<OperationsChannelRow>,
    pub route_health: Vec<OperationsRouteHealthRow>,
    pub risks: Vec<OperationsRisk>,
    pub accounting_scope_note: String,
}

/// One fully attributed successful metered-usage path for the Operations
/// flow view. Model groups are intentionally absent: a request may be
/// authorized by multiple overlapping grants, so attributing it to one group
/// without recording the winning entitlement would be fabricated data.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsFlowRow")]
pub struct OperationsFlowRow {
    pub user_id: Option<i32>,
    pub user_email: String,
    pub project_id: i32,
    pub project_name: String,
    pub api_key_id: Option<i32>,
    pub api_key_name: String,
    pub requested_model: String,
    pub actual_model: String,
    pub channel_id: Option<i32>,
    pub channel_name: String,
    pub metered_requests: i32,
    pub total_tokens: i32,
    pub recorded_upstream_cost: Option<f64>,
    pub recognized_usage_revenue: Option<f64>,
    pub settled_requests: i32,
    pub last_activity_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsFlow")]
pub struct OperationsFlow {
    pub generated_at: String,
    pub period_start: String,
    pub period_end: String,
    pub period_days: i32,
    pub usage_rows: i32,
    pub settled_usage_rows: i32,
    pub rows: Vec<OperationsFlowRow>,
    /// Stable translation key describing the attribution boundary.
    pub attribution_note: String,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsModelSeriesPoint")]
pub struct OperationsModelSeriesPoint {
    pub bucket_start: String,
    pub requested_model: String,
    pub metered_requests: i32,
    pub total_tokens: i32,
    pub recorded_upstream_cost: Option<f64>,
    pub recognized_usage_revenue: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "OperationsModelSeries")]
pub struct OperationsModelSeries {
    pub generated_at: String,
    pub period_start: String,
    pub period_end: String,
    pub period_days: i32,
    pub granularity: String,
    pub points: Vec<OperationsModelSeriesPoint>,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "ProviderQuotaObservation")]
pub struct ProviderQuotaObservation {
    pub id: i32,
    pub status: String,
    pub success: bool,
    pub currency: Option<String>,
    pub total: Option<String>,
    pub used: Option<String>,
    pub remaining: Option<String>,
    pub unlimited: Option<bool>,
    pub balance_source: Option<String>,
    pub error_message: Option<String>,
    pub observed_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "ProviderPriceChangeEvent")]
pub struct ProviderPriceChangeEvent {
    pub id: i32,
    pub upstream_model_id: String,
    pub group_name: String,
    pub billing_kind: String,
    pub event_type: String,
    pub field_name: Option<String>,
    pub from_value: Option<String>,
    pub to_value: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "ProviderObservationHistory")]
pub struct ProviderObservationHistory {
    pub quota: Vec<ProviderQuotaObservation>,
    pub price_changes: Vec<ProviderPriceChangeEvent>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum OperationsError {
    #[error("operations ledger service is not available")]
    ServiceUnavailable,
    #[error("failed to load operations ledger: {0}")]
    Query(String),
}

#[async_trait::async_trait]
pub trait OperationsServices: Send + Sync {
    async fn operations_ledger(
        &self,
        period_days: i32,
    ) -> Result<OperationsLedger, OperationsError>;

    async fn operations_flow(
        &self,
        period_days: i32,
        limit: i32,
    ) -> Result<OperationsFlow, OperationsError>;

    async fn operations_model_series(
        &self,
        period_days: i32,
    ) -> Result<OperationsModelSeries, OperationsError>;

    async fn provider_observation_history(
        &self,
        channel_id: &str,
        limit: i32,
    ) -> Result<ProviderObservationHistory, OperationsError>;
}

pub(crate) fn operations_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn OperationsServices>, String> {
    ctx.data::<Arc<dyn OperationsServices>>()
        .cloned()
        .map_err(|_| OperationsError::ServiceUnavailable.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeOperations;

    #[async_trait::async_trait]
    impl OperationsServices for FakeOperations {
        async fn operations_ledger(
            &self,
            period_days: i32,
        ) -> Result<OperationsLedger, OperationsError> {
            Ok(OperationsLedger {
                period_days,
                summary: OperationsLedgerSummary {
                    customer_requests: 3,
                    upstream_attempts: 4,
                    retry_count: 1,
                    average_ttft_ms: Some(42.0),
                    ttft_sample_count: 2,
                    average_tps: Some(18.5),
                    tps_sample_count: 2,
                    error_breakdown: vec![OperationsErrorBucket {
                        category: "timeout".into(),
                        count: 1,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            })
        }

        async fn operations_flow(
            &self,
            period_days: i32,
            _limit: i32,
        ) -> Result<OperationsFlow, OperationsError> {
            Ok(OperationsFlow {
                period_days,
                attribution_note: "METERED_SUCCESSFUL_USAGE_FLOW".into(),
                ..Default::default()
            })
        }

        async fn operations_model_series(
            &self,
            period_days: i32,
        ) -> Result<OperationsModelSeries, OperationsError> {
            Ok(OperationsModelSeries {
                period_days,
                granularity: "hour".into(),
                ..Default::default()
            })
        }

        async fn provider_observation_history(
            &self,
            channel_id: &str,
            limit: i32,
        ) -> Result<ProviderObservationHistory, OperationsError> {
            Ok(ProviderObservationHistory {
                quota: vec![ProviderQuotaObservation {
                    id: limit,
                    status: channel_id.to_string(),
                    observed_at: "2026-08-14T00:00:00Z".into(),
                    ..Default::default()
                }],
                price_changes: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn query_forwards_period_and_returns_request_attempt_split()
    -> Result<(), Box<dyn std::error::Error>> {
        let service: Arc<dyn OperationsServices> = Arc::new(FakeOperations);
        let schema = crate::admin_schema_builder().data(service).finish();
        let response = schema
            .execute("{ operationsLedger(periodDays: 30) { periodDays summary { customerRequests upstreamAttempts retryCount averageTtftMs ttftSampleCount averageTps tpsSampleCount errorBreakdown { category count } } trend { retryCount averageTtftMs averageTps } channels { retryCount ttftSampleCount averageTps tpsSampleCount errorBreakdown { category count } } } }")
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let json = response.data.into_json()?;
        assert_eq!(json["operationsLedger"]["periodDays"], 30);
        assert_eq!(json["operationsLedger"]["summary"]["customerRequests"], 3);
        assert_eq!(json["operationsLedger"]["summary"]["upstreamAttempts"], 4);
        assert_eq!(json["operationsLedger"]["summary"]["retryCount"], 1);
        assert_eq!(
            json["operationsLedger"]["summary"]["errorBreakdown"][0]["category"],
            "timeout"
        );
        Ok(())
    }

    #[tokio::test]
    async fn observation_query_forwards_channel_and_limit() -> Result<(), Box<dyn std::error::Error>>
    {
        let service: Arc<dyn OperationsServices> = Arc::new(FakeOperations);
        let schema = crate::admin_schema_builder().data(service).finish();
        let response = schema
            .execute(
                "{ providerObservationHistory(channelId: \"gid://conduit/Channel/7\", limit: 9) { quota { id status } } }",
            )
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let json = response.data.into_json()?;
        assert_eq!(json["providerObservationHistory"]["quota"][0]["id"], 9);
        assert_eq!(
            json["providerObservationHistory"]["quota"][0]["status"],
            "gid://conduit/Channel/7"
        );
        Ok(())
    }

    #[tokio::test]
    async fn flow_query_forwards_period() -> Result<(), Box<dyn std::error::Error>> {
        let service: Arc<dyn OperationsServices> = Arc::new(FakeOperations);
        let schema = crate::admin_schema_builder().data(service).finish();
        let response = schema
            .execute(
                "{ operationsFlow(periodDays: 30, limit: 25) { periodDays attributionNote rows { requestedModel actualModel } } }",
            )
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let json = response.data.into_json()?;
        assert_eq!(json["operationsFlow"]["periodDays"], 30);
        assert_eq!(
            json["operationsFlow"]["attributionNote"],
            "METERED_SUCCESSFUL_USAGE_FLOW"
        );
        Ok(())
    }

    #[tokio::test]
    async fn model_series_query_forwards_period() -> Result<(), Box<dyn std::error::Error>> {
        let service: Arc<dyn OperationsServices> = Arc::new(FakeOperations);
        let schema = crate::admin_schema_builder().data(service).finish();
        let response = schema
            .execute("{ operationsModelSeries(periodDays: 14) { periodDays granularity points { bucketStart requestedModel } } }")
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let json = response.data.into_json()?;
        assert_eq!(json["operationsModelSeries"]["periodDays"], 14);
        assert_eq!(json["operationsModelSeries"]["granularity"], "hour");
        Ok(())
    }
}
