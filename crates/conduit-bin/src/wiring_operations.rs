//! Shared administrator-operations aggregation helpers for PostgreSQL.

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use conduit_admin_graphql::operations::{OperationsErrorBucket, OperationsMoneyMetric};
use conduit_services::RouteHealthSample;

pub(crate) const CASH_ACCOUNTING_NOTE: &str = "USAGE_GROSS_PROFIT_SCOPE";

#[derive(Clone, Default)]
pub(crate) struct AttemptAggregate {
    pub(crate) requests: i64,
    pub(crate) total: i64,
    pub(crate) success: i64,
    pub(crate) failed: i64,
    pub(crate) avg_latency: Option<f64>,
    pub(crate) avg_ttft: Option<f64>,
    pub(crate) ttft_samples: i64,
    pub(crate) retries: i64,
    pub(crate) last_activity: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct TpsAggregate {
    pub(crate) average: Option<f64>,
    pub(crate) samples: i64,
}

#[derive(Clone, Default)]
pub(crate) struct UsageAggregate {
    pub(crate) rows: i64,
    pub(crate) costed: i64,
    pub(crate) cost: Option<f64>,
    pub(crate) input: i64,
    pub(crate) output: i64,
    pub(crate) cached: i64,
    pub(crate) total: i64,
    pub(crate) last_activity: Option<String>,
}

#[derive(Clone, Default)]
pub(crate) struct BillingAggregate {
    pub(crate) billable: i64,
    pub(crate) settled: i64,
    pub(crate) pending: i64,
    pub(crate) revenue: Option<f64>,
}

pub(crate) fn count(value: i64) -> i32 {
    value.clamp(0, i64::from(i32::MAX)) as i32
}

pub(crate) fn rate(part: i64, total: i64) -> Option<f64> {
    if total <= 0 {
        None
    } else {
        Some(part as f64 / total as f64)
    }
}

pub(crate) fn weighted_average<I>(values: I) -> Option<f64>
where
    I: IntoIterator<Item = (Option<f64>, i64)>,
{
    let (weighted_sum, samples) = values.into_iter().fold(
        (0.0, 0_i64),
        |(sum, count), (average, sample_count)| match (average, sample_count) {
            (Some(value), samples) if samples > 0 => {
                (sum + value * samples as f64, count + samples)
            }
            _ => (sum, count),
        },
    );
    (samples > 0).then_some(weighted_sum / samples as f64)
}

pub(crate) fn error_buckets(
    rows: impl IntoIterator<Item = (String, i64)>,
) -> Vec<OperationsErrorBucket> {
    let mut buckets = rows
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(category, value)| OperationsErrorBucket {
            category,
            count: count(value),
        })
        .collect::<Vec<_>>();
    buckets.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.category.cmp(&right.category))
    });
    buckets
}

pub(crate) fn route_health_sample(
    attempts: i64,
    successes: i64,
    errors: &[(String, i64)],
) -> RouteHealthSample {
    let count_category = |name: &str| {
        errors
            .iter()
            .filter(|(category, _)| category == name)
            .map(|(_, count)| *count)
            .sum()
    };
    RouteHealthSample {
        attempts,
        successes,
        auth_failures: count_category("auth"),
        configuration_failures: count_category("configuration"),
        transient_failures: errors
            .iter()
            .filter(|(category, _)| {
                matches!(
                    category.as_str(),
                    "rate_limit" | "timeout" | "upstream_5xx" | "connection" | "canceled"
                )
            })
            .map(|(_, count)| *count)
            .sum(),
    }
}

pub(crate) const ERROR_CATEGORY_SQL: &str = "CASE \
    WHEN status = 'canceled' THEN 'canceled' \
    WHEN response_status_code IN (401, 403) OR lower(COALESCE(error_message,'')) LIKE '%unauthoriz%' \
      OR lower(COALESCE(error_message,'')) LIKE '%authentication%' \
      OR lower(COALESCE(error_message,'')) LIKE '%credential%' THEN 'auth' \
    WHEN response_status_code = 429 OR lower(COALESCE(error_message,'')) LIKE '%rate limit%' \
      OR lower(COALESCE(error_message,'')) LIKE '%too many request%' THEN 'rate_limit' \
    WHEN response_status_code IN (408, 504) OR lower(COALESCE(error_message,'')) LIKE '%timeout%' \
      OR lower(COALESCE(error_message,'')) LIKE '%timed out%' \
      OR lower(COALESCE(error_message,'')) LIKE '%deadline%' THEN 'timeout' \
    WHEN response_status_code >= 500 THEN 'upstream_5xx' \
    WHEN lower(COALESCE(error_message,'')) LIKE '%connection%' \
      OR lower(COALESCE(error_message,'')) LIKE '%connect error%' \
      OR lower(COALESCE(error_message,'')) LIKE '%dns%' \
      OR lower(COALESCE(error_message,'')) LIKE '%tls%' THEN 'connection' \
    WHEN lower(COALESCE(error_message,'')) LIKE '%no candidate%' \
      OR lower(COALESCE(error_message,'')) LIKE '%not configured%' \
      OR lower(COALESCE(error_message,'')) LIKE '%unsupported%' \
      OR lower(COALESCE(error_message,'')) LIKE '%invalid url%' THEN 'configuration' \
    ELSE 'unknown' END";

pub(crate) fn cost_metric(cost: Option<f64>, costed: i64, usage: i64) -> OperationsMoneyMetric {
    if usage == 0 {
        return OperationsMoneyMetric {
            amount: Some(0.0),
            quality: "EXACT".into(),
            coverage_rate: None,
            reason: Some("NO_METERED_USAGE".into()),
        };
    }
    if costed == 0 {
        return OperationsMoneyMetric {
            amount: None,
            quality: "UNAVAILABLE".into(),
            coverage_rate: Some(0.0),
            reason: Some("NO_RECORDED_COST".into()),
        };
    }
    let complete = costed == usage;
    OperationsMoneyMetric {
        amount: cost,
        quality: if complete { "EXACT" } else { "PARTIAL" }.into(),
        coverage_rate: rate(costed, usage),
        reason: (!complete).then(|| "PARTIAL_RECORDED_COST".into()),
    }
}

pub(crate) fn revenue_metric(
    revenue: Option<f64>,
    settled: i64,
    usage: i64,
) -> OperationsMoneyMetric {
    if usage == 0 {
        return OperationsMoneyMetric {
            amount: Some(0.0),
            quality: "EXACT".into(),
            coverage_rate: None,
            reason: Some("NO_METERED_USAGE".into()),
        };
    }
    if settled == 0 {
        return OperationsMoneyMetric {
            amount: None,
            quality: "UNAVAILABLE".into(),
            coverage_rate: Some(0.0),
            reason: Some("NO_SETTLED_USAGE_REVENUE".into()),
        };
    }
    let complete = settled == usage;
    OperationsMoneyMetric {
        amount: revenue,
        quality: if complete { "EXACT" } else { "PARTIAL" }.into(),
        coverage_rate: rate(settled, usage),
        reason: (!complete).then(|| "PARTIAL_SETTLED_USAGE_REVENUE".into()),
    }
}

pub(crate) fn profit_metric(
    revenue: &OperationsMoneyMetric,
    cost: &OperationsMoneyMetric,
) -> (OperationsMoneyMetric, Option<f64>) {
    let (Some(revenue_amount), Some(cost_amount)) = (revenue.amount, cost.amount) else {
        return (
            OperationsMoneyMetric {
                amount: None,
                quality: "UNAVAILABLE".into(),
                coverage_rate: None,
                reason: Some("PROFIT_REQUIRES_REVENUE_AND_COST".into()),
            },
            None,
        );
    };
    let quality = if revenue.quality == "EXACT" && cost.quality == "EXACT" {
        "EXACT"
    } else {
        "PARTIAL"
    };
    let coverage_rate = match (revenue.coverage_rate, cost.coverage_rate) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    };
    let profit = revenue_amount - cost_amount;
    (
        OperationsMoneyMetric {
            amount: Some(profit),
            quality: quality.into(),
            coverage_rate,
            reason: (quality == "PARTIAL").then(|| "PARTIAL_GROSS_PROFIT".into()),
        },
        (revenue_amount > 0.0).then_some(profit / revenue_amount),
    )
}

pub(crate) fn timestamp_is_stale(value: &str, now: DateTime<Utc>, max_age: Duration) -> bool {
    let parsed = DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
                .map(|timestamp| timestamp.and_utc())
        });
    parsed.map_or(true, |timestamp| now - timestamp > max_age)
}
