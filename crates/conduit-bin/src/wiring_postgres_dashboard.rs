//! PostgreSQL implementation of the cross-project admin dashboard.
//!
//! PostgreSQL-native timestamps, booleans, bind parameters and date buckets
//! are used throughout.  Procurement cost is read only from `usage_logs`;
//! this surface has no revenue field and therefore never infers revenue from
//! balances, grants, or subscriptions.  Recognized revenue remains owned by
//! `charge_settlements` in the operations domain.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;
use chrono::{DateTime, Duration, FixedOffset, Offset, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};

use conduit_admin_graphql::dashboard::{
    APIKeyTokenUsageStats, APIKeyTokenUsageStatsInput, ChannelPerformanceStat, ChannelSuccessRate,
    CostStatsByAPIKey, CostStatsByChannel, CostStatsByModel, DailyRequestStats, DashboardError,
    DashboardOverview, DashboardServices, FastestChannel, FastestChannelsInput, FastestModel,
    HourlyRequestStats, ModelPerformanceStat, RequestStats, RequestStatsByAPIKey,
    RequestStatsByChannel, RequestStatsByModel, TokenStats, TokenStatsByAPIKey,
    TokenStatsByChannel, TokenStatsByModel, TopRequestsProjects,
};
use conduit_admin_graphql::scalars::TimeScalar;
use conduit_services::usage_service::{CalendarPeriods, get_calendar_periods};

const DAILY_STATS_DAYS: i64 = 30;
const TOP_PERFORMERS_LIMIT: usize = 6;
const PROBE_MAX_THROUGHPUT: f64 = 2000.0;
const TOKENS_EXPR: &str = "ul.completion_tokens + \
    COALESCE(ul.completion_reasoning_tokens, 0) + \
    COALESCE(ul.completion_audio_tokens, 0)";
const LATENCY_CORE: &str = "CASE WHEN se.stream = TRUE \
    AND se.metrics_first_token_latency_ms IS NOT NULL \
    THEN GREATEST(se.metrics_latency_ms - se.metrics_first_token_latency_ms, 0) \
    ELSE se.metrics_latency_ms END";

struct PgDashboardQueryAdapter {
    pool: PgPool,
    offset: FixedOffset,
}

impl PgDashboardQueryAdapter {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            offset: Utc.fix(),
        }
    }

    pub fn with_offset(mut self, offset: FixedOffset) -> Self {
        self.offset = offset;
        self
    }

    fn periods(&self) -> Result<CalendarPeriods, String> {
        get_calendar_periods(Utc::now(), self.offset).map_err(|error| error.to_string())
    }

    fn since_for_window(&self, window: Option<&str>) -> Result<Option<DateTime<Utc>>, String> {
        let Some(window) = window else {
            return Ok(None);
        };
        if window.is_empty() || window == "allTime" {
            return Ok(None);
        }
        let periods = self.periods()?;
        Ok(match window {
            "day" => Some(periods.today.start),
            "week" => Some(periods.this_week.start),
            "month" => Some(periods.this_month.start),
            _ => None,
        })
    }

    fn since_for_fastest_window(&self, window: Option<&str>) -> Result<DateTime<Utc>, String> {
        let periods = self.periods()?;
        Ok(match window.unwrap_or_default() {
            "week" => periods.this_week.start,
            "month" => periods.this_month.start,
            _ => periods.today.start,
        })
    }

    fn thirty_day_start(&self) -> Result<DateTime<Utc>, String> {
        Ok(self.periods()?.today.start - Duration::days(DAILY_STATS_DAYS - 1))
    }

    fn local_date_expr(&self, column: &str) -> String {
        format!(
            "to_char(({column} AT TIME ZONE 'UTC') + make_interval(secs => {}), 'YYYY-MM-DD')",
            self.offset.local_minus_utc()
        )
    }

    fn local_hour_expr(&self, column: &str) -> String {
        format!(
            "EXTRACT(HOUR FROM (({column} AT TIME ZONE 'UTC') + \
             make_interval(secs => {})))::BIGINT",
            self.offset.local_minus_utc()
        )
    }

    fn probe_local_date_expr(&self) -> String {
        format!(
            "to_char((to_timestamp(\"timestamp\") AT TIME ZONE 'UTC') + \
             make_interval(secs => {}), 'YYYY-MM-DD')",
            self.offset.local_minus_utc()
        )
    }

    async fn count_usage(
        &self,
        start: DateTime<Utc>,
        end: Option<DateTime<Utc>>,
    ) -> Result<i64, sqlx::Error> {
        match end {
            Some(end) => {
                sqlx::query_scalar(
                    "SELECT COUNT(*)::BIGINT FROM usage_logs \
                     WHERE created_at >= $1 AND created_at < $2",
                )
                .bind(start)
                .bind(end)
                .fetch_one(&self.pool)
                .await
            }
            None => {
                sqlx::query_scalar("SELECT COUNT(*)::BIGINT FROM usage_logs WHERE created_at >= $1")
                    .bind(start)
                    .fetch_one(&self.pool)
                    .await
            }
        }
    }

    async fn usage_token_sums(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<(i64, i64, i64), sqlx::Error> {
        match since {
            Some(since) => {
                sqlx::query_as(
                    "SELECT COALESCE(SUM(prompt_tokens),0)::BIGINT, \
                            COALESCE(SUM(completion_tokens),0)::BIGINT, \
                            COALESCE(SUM(prompt_cached_tokens),0)::BIGINT \
                     FROM usage_logs WHERE created_at >= $1",
                )
                .bind(since)
                .fetch_one(&self.pool)
                .await
            }
            None => {
                sqlx::query_as(
                    "SELECT COALESCE(SUM(prompt_tokens),0)::BIGINT, \
                            COALESCE(SUM(completion_tokens),0)::BIGINT, \
                            COALESCE(SUM(prompt_cached_tokens),0)::BIGINT FROM usage_logs",
                )
                .fetch_one(&self.pool)
                .await
            }
        }
    }

    fn build_daily_perf_stats_sql(
        &self,
        id_select: &str,
        id_column: &str,
        request_join: &str,
    ) -> String {
        let date = self.local_date_expr("se.created_at");
        let throughput = throughput_sql();
        format!(
            "WITH latest_execs AS ( \
               SELECT DISTINCT ON (se.request_id) se.request_id, {id_select}, \
                      se.metrics_latency_ms, se.metrics_first_token_latency_ms, \
                      se.stream, se.created_at \
               FROM request_executions se {request_join} \
               WHERE se.\"status\"='completed' AND se.metrics_latency_ms > 0 \
                 AND se.created_at >= $1 \
               ORDER BY se.request_id, se.id DESC \
             ), daily AS ( \
               SELECT {date} AS exec_date, se.{id_column} AS id, \
                      COALESCE(SUM({TOKENS_EXPR}),0)::BIGINT AS tokens_count, \
                      COALESCE(SUM(se.metrics_latency_ms),0)::BIGINT AS latency_ms, \
                      AVG(se.metrics_first_token_latency_ms::DOUBLE PRECISION) \
                        FILTER (WHERE se.metrics_first_token_latency_ms > 0) AS ttft_ms, \
                      COUNT(DISTINCT se.request_id)::BIGINT AS request_count, \
                      {throughput} AS throughput \
               FROM latest_execs se JOIN usage_logs ul ON ul.request_id=se.request_id \
               GROUP BY exec_date, se.{id_column} \
             ) SELECT exec_date,id,tokens_count,latency_ms,ttft_ms,request_count,throughput \
               FROM daily WHERE throughput IS NOT NULL AND throughput > 0 \
               ORDER BY exec_date DESC,throughput DESC"
        )
    }

    async fn channel_names(&self, ids: &[i64]) -> Result<HashMap<i64, String>, sqlx::Error> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id,name FROM channels WHERE deleted_at=0 AND id IN (",
        );
        {
            let mut values = builder.separated(",");
            for id in ids {
                values.push_bind(*id);
            }
        }
        builder.push(")");
        builder
            .build_query_as::<(i64, String)>()
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.into_iter().collect())
    }

    async fn build_channel_perf_response(
        &self,
        rows: Vec<(String, i64, i64, Option<f64>, Option<f64>)>,
        start: DateTime<Utc>,
    ) -> Result<Vec<ChannelPerformanceStat>, sqlx::Error> {
        let mut stats: BTreeMap<String, BTreeMap<i64, (i64, Option<f64>, Option<f64>)>> =
            BTreeMap::new();
        let mut totals = BTreeMap::<i64, i64>::new();
        for (date, channel_id, count, throughput, ttft) in rows {
            stats
                .entry(date)
                .or_default()
                .insert(channel_id, (count, throughput, ttft));
            *totals.entry(channel_id).or_default() += count;
        }
        let top: HashSet<i64> = confidence_top_n(
            totals.into_iter().collect(),
            |value: &(i64, i64)| value.1,
            |value| value.1 as f64,
            TOP_PERFORMERS_LIMIT,
        )
        .into_iter()
        .map(|(id, _)| id)
        .collect();
        let names = self
            .channel_names(&top.iter().copied().collect::<Vec<_>>())
            .await?;
        let start_date = start.with_timezone(&self.offset).date_naive();
        let mut output = Vec::new();
        for day in 0..DAILY_STATS_DAYS {
            let date = (start_date + Duration::days(day))
                .format("%Y-%m-%d")
                .to_string();
            let Some(day_stats) = stats.get(&date) else {
                continue;
            };
            for (channel_id, (count, throughput, ttft)) in day_stats {
                if !top.contains(channel_id) {
                    continue;
                }
                output.push(ChannelPerformanceStat {
                    date: date.clone(),
                    channel_id: channel_id.to_string().into(),
                    channel_name: names
                        .get(channel_id)
                        .filter(|name| !name.is_empty())
                        .cloned()
                        .unwrap_or_else(|| format!("channel-{channel_id}")),
                    throughput: throughput.filter(|value| *value > 0.0),
                    ttft_ms: *ttft,
                    request_count: clamp_i32(*count),
                });
            }
        }
        Ok(output)
    }
}

/// Dashboard reads are explicitly eventual-consistency safe. Each GraphQL
/// operation is attempted wholly against the replica and, when configured,
/// retried wholly against the master after a replica error. This deliberately
/// does not affect authentication, wallet, entitlement, or routing reads.
pub struct PgDashboardAdapter {
    master: PgDashboardQueryAdapter,
    read: Option<PgDashboardQueryAdapter>,
    fallback_on_replica_failure: bool,
}

impl PgDashboardAdapter {
    pub fn new(pool: PgPool) -> Self {
        Self {
            master: PgDashboardQueryAdapter::new(pool),
            read: None,
            fallback_on_replica_failure: false,
        }
    }

    pub fn with_read_pool(
        mut self,
        read_pool: Option<PgPool>,
        fallback_on_replica_failure: bool,
    ) -> Self {
        self.read = read_pool.map(PgDashboardQueryAdapter::new);
        self.fallback_on_replica_failure = fallback_on_replica_failure;
        self
    }

    pub fn with_offset(mut self, offset: FixedOffset) -> Self {
        self.master = self.master.with_offset(offset);
        self.read = self.read.map(|adapter| adapter.with_offset(offset));
        self
    }

    async fn eventual<T, F>(&self, operation: F) -> Result<T, DashboardError>
    where
        F: for<'a> Fn(
            &'a PgDashboardQueryAdapter,
        )
            -> Pin<Box<dyn Future<Output = Result<T, DashboardError>> + Send + 'a>>,
    {
        if let Some(read) = self.read.as_ref() {
            match operation(read).await {
                Ok(value) => return Ok(value),
                Err(_) if self.fallback_on_replica_failure => {}
                Err(error) => return Err(error),
            }
        }
        operation(&self.master).await
    }
}

fn throughput_sql() -> String {
    format!(
        "CASE WHEN SUM({LATENCY_CORE}) > 0 THEN \
         SUM({TOKENS_EXPR})::DOUBLE PRECISION * 1000.0 / SUM({LATENCY_CORE}) \
         ELSE 0.0 END"
    )
}

fn build_throughput_sql(select: &str, joins: &str, group: &str, limit: i64) -> String {
    let throughput = throughput_sql();
    format!(
        "WITH latest_execs AS ( \
           SELECT DISTINCT ON (request_id) id,request_id,channel_id,stream, \
                  metrics_latency_ms,metrics_first_token_latency_ms,created_at \
           FROM request_executions WHERE \"status\"='completed' \
             AND metrics_latency_ms > 0 AND created_at >= $1 \
           ORDER BY request_id,id DESC \
         ) SELECT {select}, COALESCE(SUM({TOKENS_EXPR}),0)::BIGINT AS tokens_count, \
                  COALESCE(SUM(se.metrics_latency_ms),0)::BIGINT AS latency_ms, \
                  COUNT(DISTINCT se.request_id)::BIGINT AS request_count, \
                  {throughput} AS throughput \
           FROM latest_execs se JOIN usage_logs ul ON ul.request_id=se.request_id \
           {joins} GROUP BY {group} ORDER BY throughput DESC LIMIT {limit}"
    )
}

fn normalized_fastest_limit(limit: Option<i32>) -> usize {
    match limit {
        Some(value) if value > 0 => value.min(100) as usize,
        _ => 5,
    }
}

fn confidence_level(count: i64, median: f64) -> i32 {
    if median == 0.0 || count < 100 {
        1
    } else if count >= 500 && count as f64 / median >= 1.5 {
        3
    } else if count as f64 / median >= 0.5 {
        2
    } else {
        1
    }
}

fn confidence_top_n<T>(
    items: Vec<T>,
    count: impl Fn(&T) -> i64,
    value: impl Fn(&T) -> f64,
    limit: usize,
) -> Vec<T> {
    if items.is_empty() {
        return items;
    }
    let mut counts = items.iter().map(&count).collect::<Vec<_>>();
    counts.sort_unstable();
    let middle = counts.len() / 2;
    let median = if counts.len().is_multiple_of(2) {
        (counts[middle - 1] + counts[middle]) as f64 / 2.0
    } else {
        counts[middle] as f64
    };
    let mut scored = items
        .into_iter()
        .map(|item| (confidence_level(count(&item), median), item))
        .collect::<Vec<_>>();
    if scored.iter().filter(|(score, _)| *score >= 2).count() >= limit {
        scored.retain(|(score, _)| *score >= 2);
    }
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| value(&right.1).total_cmp(&value(&left.1)))
    });
    scored.truncate(limit);
    scored.into_iter().map(|(_, item)| item).collect()
}

fn clamp_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn request_error(context: &str, error: sqlx::Error) -> DashboardError {
    DashboardError::RequestStats(format!("{context}: {error}"))
}

fn token_error(context: &str, error: sqlx::Error) -> DashboardError {
    DashboardError::TokenStats(format!("{context}: {error}"))
}

#[async_trait]
impl DashboardServices for PgDashboardQueryAdapter {
    async fn dashboard_overview(&self) -> Result<DashboardOverview, DashboardError> {
        let mut total = 0_i64;
        let mut failed = 0_i64;
        let rows = sqlx::query_as::<_, (String, i64)>(
            "SELECT \"status\",COUNT(*)::BIGINT FROM requests GROUP BY \"status\"",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| request_error("failed to get dashboard overview", error))?;
        for (status, count) in rows {
            total += count;
            if status == "failed" {
                failed = count;
            }
        }
        let request_stats = self.request_stats().await?;
        Ok(DashboardOverview {
            total_requests: clamp_i32(total),
            request_stats,
            failed_requests: clamp_i32(failed),
            average_response_time: None,
        })
    }

    async fn request_stats(&self) -> Result<RequestStats, DashboardError> {
        let periods = self.periods().map_err(DashboardError::RequestStats)?;
        Ok(RequestStats {
            requests_today: clamp_i32(
                self.count_usage(periods.today.start, None)
                    .await
                    .map_err(|error| request_error("failed to count today's requests", error))?,
            ),
            requests_this_week: clamp_i32(
                self.count_usage(periods.this_week.start, None)
                    .await
                    .map_err(|error| {
                        request_error("failed to count this week's requests", error)
                    })?,
            ),
            requests_last_week: clamp_i32(
                self.count_usage(periods.last_week.start, Some(periods.last_week.end))
                    .await
                    .map_err(|error| {
                        request_error("failed to count last week's requests", error)
                    })?,
            ),
            requests_this_month: clamp_i32(
                self.count_usage(periods.this_month.start, None)
                    .await
                    .map_err(|error| {
                        request_error("failed to count this month's requests", error)
                    })?,
            ),
        })
    }

    async fn token_stats(&self) -> Result<TokenStats, DashboardError> {
        let periods = self.periods().map_err(DashboardError::TokenStats)?;
        let today = self
            .usage_token_sums(Some(periods.today.start))
            .await
            .map_err(|error| token_error("failed to get today's token stats", error))?;
        let week = self
            .usage_token_sums(Some(periods.this_week.start))
            .await
            .map_err(|error| token_error("failed to get this week's token stats", error))?;
        let month = self
            .usage_token_sums(Some(periods.this_month.start))
            .await
            .map_err(|error| token_error("failed to get this month's token stats", error))?;
        let all = self
            .usage_token_sums(None)
            .await
            .map_err(|error| token_error("failed to get all-time token stats", error))?;
        let last_updated = Some(TimeScalar(Utc::now()));
        Ok(TokenStats {
            total_input_tokens_today: clamp_i32(today.0),
            total_output_tokens_today: clamp_i32(today.1),
            total_cached_tokens_today: clamp_i32(today.2),
            total_input_tokens_this_week: clamp_i32(week.0),
            total_output_tokens_this_week: clamp_i32(week.1),
            total_cached_tokens_this_week: clamp_i32(week.2),
            total_input_tokens_this_month: clamp_i32(month.0),
            total_output_tokens_this_month: clamp_i32(month.1),
            total_cached_tokens_this_month: clamp_i32(month.2),
            total_input_tokens_all_time: clamp_i32(all.0),
            total_output_tokens_all_time: clamp_i32(all.1),
            total_cached_tokens_all_time: clamp_i32(all.2),
            last_updated,
        })
    }

    async fn request_stats_by_channel(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<RequestStatsByChannel>, DashboardError> {
        let since = self
            .since_for_window(time_window.as_deref())
            .map_err(DashboardError::RequestStats)?;
        let rows: Vec<(String, i64)> = if let Some(since) = since {
            sqlx::query_as(
                "SELECT c.name,COUNT(u.id)::BIGINT AS request_count \
                 FROM usage_logs u JOIN channels c ON c.id=u.channel_id \
                 WHERE c.deleted_at=0 AND u.created_at >= $1 \
                 GROUP BY c.id,c.name ORDER BY request_count DESC LIMIT 10",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                "SELECT c.name,COUNT(u.id)::BIGINT AS request_count \
                 FROM usage_logs u JOIN channels c ON c.id=u.channel_id \
                 WHERE c.deleted_at=0 GROUP BY c.id,c.name \
                 ORDER BY request_count DESC LIMIT 10",
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| request_error("failed to get requests by channel", error))?;
        Ok(rows
            .into_iter()
            .map(|(channel_name, count)| RequestStatsByChannel {
                channel_name,
                count: clamp_i32(count),
            })
            .collect())
    }

    async fn request_stats_by_model(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<RequestStatsByModel>, DashboardError> {
        let since = self
            .since_for_window(time_window.as_deref())
            .map_err(DashboardError::RequestStats)?;
        let rows: Vec<(String, i64)> = if let Some(since) = since {
            sqlx::query_as(
                "SELECT model_id,COUNT(*)::BIGINT AS request_count FROM usage_logs \
                 WHERE created_at >= $1 GROUP BY model_id \
                 ORDER BY request_count DESC LIMIT 10",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                "SELECT model_id,COUNT(*)::BIGINT AS request_count FROM usage_logs \
                 GROUP BY model_id ORDER BY request_count DESC LIMIT 10",
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| request_error("failed to get requests by model", error))?;
        Ok(rows
            .into_iter()
            .map(|(model_id, count)| RequestStatsByModel {
                model_id,
                count: clamp_i32(count),
            })
            .collect())
    }

    async fn request_stats_by_api_key(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<RequestStatsByAPIKey>, DashboardError> {
        let since = self
            .since_for_window(time_window.as_deref())
            .map_err(DashboardError::RequestStats)?;
        let rows: Vec<(i64, String, i64)> = if let Some(since) = since {
            sqlx::query_as(
                "SELECT a.id,a.name,COUNT(*)::BIGINT AS request_count \
                 FROM usage_logs u JOIN api_keys a ON a.id=u.api_key_id \
                 WHERE a.deleted_at=0 AND u.created_at >= $1 \
                 GROUP BY a.id,a.name ORDER BY request_count DESC LIMIT 10",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                "SELECT a.id,a.name,COUNT(*)::BIGINT AS request_count \
                 FROM usage_logs u JOIN api_keys a ON a.id=u.api_key_id \
                 WHERE a.deleted_at=0 GROUP BY a.id,a.name \
                 ORDER BY request_count DESC LIMIT 10",
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| request_error("failed to get requests by API key", error))?;
        Ok(rows
            .into_iter()
            .map(|(id, name, count)| RequestStatsByAPIKey {
                api_key_id: id.to_string(),
                api_key_name: name,
                count: clamp_i32(count),
            })
            .collect())
    }

    async fn token_stats_by_api_key(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<TokenStatsByAPIKey>, DashboardError> {
        let since = self
            .since_for_window(time_window.as_deref())
            .map_err(DashboardError::TokenStats)?;
        let base = "SELECT a.id,a.name, \
            COALESCE(SUM(u.prompt_tokens),0)::BIGINT, \
            COALESCE(SUM(u.completion_tokens),0)::BIGINT, \
            COALESCE(SUM(u.prompt_cached_tokens),0)::BIGINT, \
            COALESCE(SUM(u.completion_reasoning_tokens),0)::BIGINT, \
            COALESCE(SUM(u.total_tokens),0)::BIGINT \
            FROM usage_logs u JOIN api_keys a ON a.id=u.api_key_id \
            WHERE a.deleted_at=0";
        let tail = " GROUP BY a.id,a.name \
            ORDER BY (SUM(u.prompt_tokens)+SUM(u.completion_tokens)) DESC LIMIT 10";
        let rows: Vec<(i64, String, i64, i64, i64, i64, i64)> = if let Some(since) = since {
            sqlx::query_as(&format!("{base} AND u.created_at >= $1{tail}"))
                .bind(since)
                .fetch_all(&self.pool)
                .await
        } else {
            sqlx::query_as(&format!("{base}{tail}"))
                .fetch_all(&self.pool)
                .await
        }
        .map_err(|error| token_error("failed to get tokens by API key", error))?;
        Ok(rows
            .into_iter()
            .map(
                |(id, name, input, output, cached, reasoning, total)| TokenStatsByAPIKey {
                    api_key_id: id.to_string().into(),
                    api_key_name: name,
                    input_tokens: clamp_i32(input),
                    output_tokens: clamp_i32(output),
                    cached_tokens: clamp_i32(cached),
                    reasoning_tokens: clamp_i32(reasoning),
                    total_tokens: clamp_i32(total),
                },
            )
            .collect())
    }

    async fn api_key_token_usage_stats(
        &self,
        input: Option<APIKeyTokenUsageStatsInput>,
    ) -> Result<Vec<APIKeyTokenUsageStats>, DashboardError> {
        const REQUIRED: &str = "apiKeyIds is required and must contain at least one API key";
        let Some(input) = input else {
            return Err(DashboardError::TokenStats(REQUIRED.to_string()));
        };
        let ids = input.api_key_ids.unwrap_or_default();
        if ids.is_empty() {
            return Err(DashboardError::TokenStats(REQUIRED.to_string()));
        }
        if ids.len() > 100 {
            return Err(DashboardError::TokenStats(
                "apiKeyIds cannot exceed 100 items".to_string(),
            ));
        }
        let since = self
            .since_for_window(input.time_window.as_deref())
            .map_err(DashboardError::TokenStats)?;
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT a.id,a.name,COALESCE(SUM(u.prompt_tokens),0)::BIGINT, \
             COALESCE(SUM(u.completion_tokens),0)::BIGINT, \
             COALESCE(SUM(u.prompt_cached_tokens),0)::BIGINT \
             FROM usage_logs u JOIN api_keys a ON a.id=u.api_key_id \
             WHERE a.deleted_at=0 AND u.api_key_id IN (",
        );
        {
            let mut values = builder.separated(",");
            for id in ids {
                values.push_bind(id);
            }
        }
        builder.push(")");
        if let Some(since) = since {
            builder.push(" AND u.created_at >= ").push_bind(since);
        }
        builder.push(" GROUP BY a.id,a.name ORDER BY a.id");
        let rows = builder
            .build_query_as::<(i64, String, i64, i64, i64)>()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| token_error("failed to get API key token usage stats", error))?;
        Ok(rows
            .into_iter()
            .map(|(id, name, input, output, cached)| APIKeyTokenUsageStats {
                api_key_id: id,
                api_key_name: name,
                total_input_tokens: clamp_i32(input),
                total_output_tokens: clamp_i32(output),
                total_cached_tokens: clamp_i32(cached),
            })
            .collect())
    }

    async fn daily_request_stats(&self) -> Result<Vec<DailyRequestStats>, DashboardError> {
        let error = |error: sqlx::Error| request_error("failed to get daily request stats", error);
        let now = Utc::now();
        let start = self
            .periods()
            .map_err(DashboardError::RequestStats)?
            .today
            .start
            - Duration::days(DAILY_STATS_DAYS - 1);
        let date = self.local_date_expr("created_at");
        let sql = format!(
            "SELECT {date} AS day,COUNT(*)::BIGINT, \
                    COALESCE(SUM(total_tokens),0)::BIGINT, \
                    COALESCE(SUM(total_cost),0.0)::DOUBLE PRECISION \
             FROM usage_logs WHERE created_at >= $1 AND created_at < $2 \
             GROUP BY day ORDER BY day"
        );
        let rows: Vec<(String, i64, i64, f64)> = sqlx::query_as(&sql)
            .bind(start)
            .bind(now)
            .fetch_all(&self.pool)
            .await
            .map_err(error)?;
        let values = rows
            .into_iter()
            .map(|(date, count, tokens, cost)| (date, (count, tokens, cost)))
            .collect::<HashMap<_, _>>();
        let start_date = start.with_timezone(&self.offset).date_naive();
        Ok((0..DAILY_STATS_DAYS)
            .map(|index| {
                let date = (start_date + Duration::days(index))
                    .format("%Y-%m-%d")
                    .to_string();
                let (count, tokens, cost) = values.get(&date).copied().unwrap_or_default();
                DailyRequestStats {
                    date,
                    count: clamp_i32(count),
                    tokens: clamp_i32(tokens),
                    cost,
                }
            })
            .collect())
    }

    async fn hourly_request_stats(
        &self,
        date: Option<String>,
    ) -> Result<Vec<HourlyRequestStats>, DashboardError> {
        let error = |message: String| {
            DashboardError::RequestStats(format!("failed to get hourly request stats: {message}"))
        };
        let day = match date.as_deref().filter(|value| !value.is_empty()) {
            Some(value) => chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map_err(|parse| error(format!("invalid date {value:?}: {parse}")))?,
            None => Utc::now().with_timezone(&self.offset).date_naive(),
        };
        let start_local = day
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| error("failed to build day start".to_string()))?;
        let start =
            (start_local - Duration::seconds(i64::from(self.offset.local_minus_utc()))).and_utc();
        let hour = self.local_hour_expr("created_at");
        let sql = format!(
            "SELECT {hour} AS hour,COUNT(*)::BIGINT FROM usage_logs \
             WHERE created_at >= $1 AND created_at < $2 GROUP BY hour ORDER BY hour"
        );
        let rows: Vec<(i64, i64)> = sqlx::query_as(&sql)
            .bind(start)
            .bind(start + Duration::days(1))
            .fetch_all(&self.pool)
            .await
            .map_err(|query| error(query.to_string()))?;
        let counts = rows.into_iter().collect::<HashMap<_, _>>();
        Ok((0..24)
            .map(|hour| HourlyRequestStats {
                hour,
                count: clamp_i32(counts.get(&i64::from(hour)).copied().unwrap_or_default()),
            })
            .collect())
    }

    async fn top_requests_projects(&self) -> Result<Vec<TopRequestsProjects>, DashboardError> {
        let rows: Vec<(i64, String, String, i64)> = sqlx::query_as(
            "SELECT p.id,p.name,p.description,COUNT(*)::BIGINT AS request_count \
             FROM usage_logs u JOIN projects p ON p.id=u.project_id \
             WHERE p.deleted_at=0 GROUP BY p.id,p.name,p.description \
             ORDER BY request_count DESC LIMIT 10",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|error| request_error("failed to get top projects", error))?;
        Ok(rows
            .into_iter()
            .map(|(id, name, description, count)| TopRequestsProjects {
                project_id: id.to_string(),
                project_name: name,
                project_description: description,
                request_count: clamp_i32(count),
            })
            .collect())
    }

    async fn channel_success_rates(
        &self,
        time_window: Option<String>,
        limit: Option<i32>,
    ) -> Result<Vec<ChannelSuccessRate>, DashboardError> {
        let window = match time_window.as_deref() {
            None | Some("") => "day",
            Some(value) => value,
        };
        let since = self
            .since_for_window(Some(window))
            .map_err(DashboardError::RequestStats)?;
        let rows: Vec<(i64, i64, i64)> = if let Some(since) = since {
            sqlx::query_as(
                "SELECT channel_id, \
                   SUM(CASE WHEN \"status\"='completed' THEN 1 ELSE 0 END)::BIGINT, \
                   SUM(CASE WHEN \"status\"='failed' THEN 1 ELSE 0 END)::BIGINT \
                 FROM request_executions WHERE channel_id IS NOT NULL AND created_at >= $1 \
                 GROUP BY channel_id",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                "SELECT channel_id, \
                   SUM(CASE WHEN \"status\"='completed' THEN 1 ELSE 0 END)::BIGINT, \
                   SUM(CASE WHEN \"status\"='failed' THEN 1 ELSE 0 END)::BIGINT \
                 FROM request_executions WHERE channel_id IS NOT NULL GROUP BY channel_id",
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| request_error("failed to get channel execution stats", error))?;
        let mut items = rows
            .into_iter()
            .map(|(id, success, failed)| {
                let total = success + failed;
                let rate = if total > 0 {
                    success as f64 * 100.0 / total as f64
                } else {
                    0.0
                };
                (id, success, failed, total, rate)
            })
            .collect::<Vec<_>>();
        items.sort_by(|left, right| right.3.cmp(&left.3));
        if let Some(limit) = limit
            && limit > 0
        {
            items.truncate(limit as usize);
        }
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id,name,\"type\",status,deleted_at FROM channels WHERE id IN (",
        );
        {
            let mut ids = builder.separated(",");
            for item in &items {
                ids.push_bind(item.0);
            }
        }
        builder.push(")");
        let details = builder
            .build_query_as::<(i64, String, String, String, i64)>()
            .fetch_all(&self.pool)
            .await
            .map_err(|error| request_error("failed to get channel details", error))?
            .into_iter()
            .map(|(id, name, kind, status, deleted)| (id, (name, kind, status, deleted)))
            .collect::<HashMap<_, _>>();
        Ok(items
            .into_iter()
            .map(|(id, success, failed, total, rate)| {
                let detail = details.get(&id);
                ChannelSuccessRate {
                    channel_id: id.to_string().into(),
                    channel_name: detail.map(|value| value.0.clone()).unwrap_or_default(),
                    channel_type: detail.map(|value| value.1.clone()).unwrap_or_default(),
                    channel_disabled: detail
                        .map(|value| value.2 != "enabled" || value.3 != 0)
                        .unwrap_or(true),
                    success_count: clamp_i32(success),
                    failed_count: clamp_i32(failed),
                    total_count: clamp_i32(total),
                    success_rate: rate,
                }
            })
            .collect())
    }

    async fn fastest_channels(
        &self,
        input: FastestChannelsInput,
    ) -> Result<Vec<FastestChannel>, DashboardError> {
        let limit = normalized_fastest_limit(input.limit);
        let since = self
            .since_for_fastest_window(input.time_window.as_deref())
            .map_err(DashboardError::TokenStats)?;
        let sql = build_throughput_sql(
            "se.channel_id,c.name,c.\"type\"",
            "JOIN channels c ON c.id=se.channel_id",
            "se.channel_id,c.name,c.\"type\"",
            (limit as i64 * 4).max(20),
        );
        let rows: Vec<(i64, String, String, i64, i64, i64, f64)> = sqlx::query_as(&sql)
            .bind(since)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| token_error("failed to query fastest channels", error))?;
        Ok(confidence_top_n(rows, |row| row.5, |row| row.6, limit)
            .into_iter()
            .map(
                |(id, name, kind, tokens, latency, count, throughput)| FastestChannel {
                    channel_id: id.to_string().into(),
                    channel_name: name,
                    channel_type: kind,
                    throughput,
                    tokens_count: clamp_i32(tokens),
                    latency_ms: clamp_i32(latency),
                    request_count: clamp_i32(count),
                },
            )
            .collect())
    }

    async fn fastest_models(
        &self,
        input: FastestChannelsInput,
    ) -> Result<Vec<FastestModel>, DashboardError> {
        let limit = normalized_fastest_limit(input.limit);
        let since = self
            .since_for_fastest_window(input.time_window.as_deref())
            .map_err(DashboardError::TokenStats)?;
        let sql = build_throughput_sql(
            "r.model_id,m.name",
            "JOIN requests r ON r.id=se.request_id \
             JOIN models m ON m.model_id=r.model_id AND m.deleted_at=0",
            "r.model_id,m.name",
            (limit as i64 * 4).max(20),
        );
        let rows: Vec<(String, String, i64, i64, i64, f64)> = sqlx::query_as(&sql)
            .bind(since)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| token_error("failed to query fastest models", error))?;
        Ok(confidence_top_n(rows, |row| row.4, |row| row.5, limit)
            .into_iter()
            .map(
                |(id, name, tokens, latency, count, throughput)| FastestModel {
                    model_id: id,
                    model_name: name,
                    throughput,
                    tokens_count: clamp_i32(tokens),
                    latency_ms: clamp_i32(latency),
                    request_count: clamp_i32(count),
                },
            )
            .collect())
    }

    async fn model_performance_stats(&self) -> Result<Vec<ModelPerformanceStat>, DashboardError> {
        let start = self.thirty_day_start().map_err(|message| {
            DashboardError::TokenStats(format!(
                "failed to query model performance stats: {message}"
            ))
        })?;
        let sql = self.build_daily_perf_stats_sql(
            "r.model_id",
            "model_id",
            "JOIN requests r ON r.id=se.request_id",
        );
        let rows: Vec<(String, String, i64, i64, Option<f64>, i64, Option<f64>)> =
            sqlx::query_as(&sql)
                .bind(start)
                .fetch_all(&self.pool)
                .await
                .map_err(|error| token_error("failed to query model performance stats", error))?;
        let mut order = Vec::<String>::new();
        let mut buckets = HashMap::<String, (i64, Vec<ModelPerformanceStat>)>::new();
        for (date, model_id, _tokens, _latency, ttft, count, throughput) in rows {
            let entry = buckets.entry(model_id.clone()).or_insert_with(|| {
                order.push(model_id.clone());
                (0, Vec::new())
            });
            entry.0 += count;
            entry.1.push(ModelPerformanceStat {
                date,
                model_id,
                throughput,
                ttft_ms: ttft,
                request_count: clamp_i32(count),
            });
        }
        let ranked = confidence_top_n(
            order
                .into_iter()
                .filter_map(|id| buckets.get(&id).map(|(count, _)| (id, *count)))
                .collect(),
            |value: &(String, i64)| value.1,
            |value| value.1 as f64,
            TOP_PERFORMERS_LIMIT,
        );
        let mut output = Vec::new();
        for (model_id, _) in ranked {
            if let Some((_, stats)) = buckets.remove(&model_id) {
                output.extend(stats);
            }
        }
        Ok(output)
    }

    async fn channel_performance_stats(
        &self,
    ) -> Result<Vec<ChannelPerformanceStat>, DashboardError> {
        let start = self.thirty_day_start().map_err(|message| {
            DashboardError::TokenStats(format!(
                "failed to get channel performance stats from probes: {message}"
            ))
        })?;
        let day = self.probe_local_date_expr();
        let probe_sql = format!(
            "SELECT {day} AS day,channel_id, \
                    COALESCE(SUM(total_request_count),0)::BIGINT AS request_count, \
                    SUM(avg_tokens_per_second * total_request_count)::DOUBLE PRECISION / \
                      NULLIF(SUM(total_request_count)::DOUBLE PRECISION,0.0) AS throughput, \
                    SUM(avg_time_to_first_token_ms * total_request_count)::DOUBLE PRECISION / \
                      NULLIF(SUM(total_request_count)::DOUBLE PRECISION,0.0) AS ttft \
             FROM channel_probes WHERE \"timestamp\" >= $1 \
               AND avg_tokens_per_second <= {PROBE_MAX_THROUGHPUT} \
             GROUP BY day,channel_id ORDER BY day,channel_id"
        );
        let probe_rows: Vec<(String, i64, i64, Option<f64>, Option<f64>)> =
            sqlx::query_as(&probe_sql)
                .bind(start.timestamp())
                .fetch_all(&self.pool)
                .await
                .map_err(|error| {
                    token_error("failed to get channel performance stats from probes", error)
                })?;
        if !probe_rows.is_empty() {
            return self
                .build_channel_perf_response(probe_rows, start)
                .await
                .map_err(|error| token_error("failed to resolve channel names", error));
        }
        let sql = self.build_daily_perf_stats_sql("se.channel_id", "channel_id", "");
        let rows: Vec<(String, Option<i64>, i64, i64, Option<f64>, i64, Option<f64>)> =
            sqlx::query_as(&sql)
                .bind(start)
                .fetch_all(&self.pool)
                .await
                .map_err(|error| {
                    token_error(
                        "failed to query channel performance stats from executions",
                        error,
                    )
                })?;
        let rows = rows
            .into_iter()
            .filter_map(|(date, id, _tokens, _latency, ttft, count, throughput)| {
                id.map(|id| (date, id, count, throughput, ttft))
            })
            .collect();
        self.build_channel_perf_response(rows, start)
            .await
            .map_err(|error| token_error("failed to resolve channel names", error))
    }

    async fn token_stats_by_channel(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<TokenStatsByChannel>, DashboardError> {
        let since = self
            .since_for_window(time_window.as_deref())
            .map_err(DashboardError::TokenStats)?;
        let base = "SELECT c.id,c.name, \
            COALESCE(SUM(u.prompt_tokens),0)::BIGINT, \
            COALESCE(SUM(u.completion_tokens),0)::BIGINT, \
            COALESCE(SUM(u.prompt_cached_tokens),0)::BIGINT, \
            COALESCE(SUM(u.completion_reasoning_tokens),0)::BIGINT, \
            COALESCE(SUM(u.total_tokens),0)::BIGINT \
            FROM usage_logs u JOIN channels c ON c.id=u.channel_id \
            WHERE c.deleted_at=0";
        let tail = " GROUP BY c.id,c.name \
            ORDER BY (SUM(u.prompt_tokens)+SUM(u.completion_tokens)) DESC LIMIT 10";
        let rows: Vec<(i64, String, i64, i64, i64, i64, i64)> = if let Some(since) = since {
            sqlx::query_as(&format!("{base} AND u.created_at >= $1{tail}"))
                .bind(since)
                .fetch_all(&self.pool)
                .await
        } else {
            sqlx::query_as(&format!("{base}{tail}"))
                .fetch_all(&self.pool)
                .await
        }
        .map_err(|error| token_error("failed to get tokens by channel", error))?;
        Ok(rows
            .into_iter()
            .map(
                |(id, name, input, output, cached, reasoning, total)| TokenStatsByChannel {
                    channel_id: id.to_string().into(),
                    channel_name: name,
                    input_tokens: clamp_i32(input),
                    output_tokens: clamp_i32(output),
                    cached_tokens: clamp_i32(cached),
                    reasoning_tokens: clamp_i32(reasoning),
                    total_tokens: clamp_i32(total),
                },
            )
            .collect())
    }

    async fn token_stats_by_model(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<TokenStatsByModel>, DashboardError> {
        let since = self
            .since_for_window(time_window.as_deref())
            .map_err(DashboardError::TokenStats)?;
        let base = "SELECT model_id,COALESCE(SUM(prompt_tokens),0)::BIGINT, \
            COALESCE(SUM(completion_tokens),0)::BIGINT, \
            COALESCE(SUM(prompt_cached_tokens),0)::BIGINT, \
            COALESCE(SUM(completion_reasoning_tokens),0)::BIGINT, \
            COALESCE(SUM(total_tokens),0)::BIGINT FROM usage_logs WHERE TRUE";
        let tail = " GROUP BY model_id \
            ORDER BY (SUM(prompt_tokens)+SUM(completion_tokens)) DESC LIMIT 10";
        let rows: Vec<(String, i64, i64, i64, i64, i64)> = if let Some(since) = since {
            sqlx::query_as(&format!("{base} AND created_at >= $1{tail}"))
                .bind(since)
                .fetch_all(&self.pool)
                .await
        } else {
            sqlx::query_as(&format!("{base}{tail}"))
                .fetch_all(&self.pool)
                .await
        }
        .map_err(|error| token_error("failed to get tokens by model", error))?;
        Ok(rows
            .into_iter()
            .map(
                |(id, input, output, cached, reasoning, total)| TokenStatsByModel {
                    model_id: id,
                    input_tokens: clamp_i32(input),
                    output_tokens: clamp_i32(output),
                    cached_tokens: clamp_i32(cached),
                    reasoning_tokens: clamp_i32(reasoning),
                    total_tokens: clamp_i32(total),
                },
            )
            .collect())
    }

    async fn cost_stats_by_channel(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<CostStatsByChannel>, DashboardError> {
        let since = self
            .since_for_window(time_window.as_deref())
            .map_err(DashboardError::TokenStats)?;
        let rows: Vec<(i64, String, f64)> = if let Some(since) = since {
            sqlx::query_as(
                "SELECT c.id,c.name,COALESCE(SUM(u.total_cost),0.0)::DOUBLE PRECISION AS cost \
                 FROM usage_logs u JOIN channels c ON c.id=u.channel_id \
                 WHERE c.deleted_at=0 AND u.created_at >= $1 GROUP BY c.id,c.name \
                 ORDER BY cost DESC LIMIT 10",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                "SELECT c.id,c.name,COALESCE(SUM(u.total_cost),0.0)::DOUBLE PRECISION AS cost \
                 FROM usage_logs u JOIN channels c ON c.id=u.channel_id \
                 WHERE c.deleted_at=0 GROUP BY c.id,c.name ORDER BY cost DESC LIMIT 10",
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| token_error("failed to get cost by channel", error))?;
        Ok(rows
            .into_iter()
            .map(|(id, name, cost)| CostStatsByChannel {
                channel_id: id.to_string().into(),
                channel_name: name,
                cost,
            })
            .collect())
    }

    async fn cost_stats_by_model(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<CostStatsByModel>, DashboardError> {
        let since = self
            .since_for_window(time_window.as_deref())
            .map_err(DashboardError::TokenStats)?;
        let rows: Vec<(String, f64)> = if let Some(since) = since {
            sqlx::query_as(
                "SELECT model_id,COALESCE(SUM(total_cost),0.0)::DOUBLE PRECISION AS cost \
                 FROM usage_logs WHERE created_at >= $1 GROUP BY model_id \
                 ORDER BY cost DESC LIMIT 10",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                "SELECT model_id,COALESCE(SUM(total_cost),0.0)::DOUBLE PRECISION AS cost \
                 FROM usage_logs GROUP BY model_id ORDER BY cost DESC LIMIT 10",
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| token_error("failed to get cost by model", error))?;
        Ok(rows
            .into_iter()
            .map(|(id, cost)| CostStatsByModel { model_id: id, cost })
            .collect())
    }

    async fn cost_stats_by_api_key(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<CostStatsByAPIKey>, DashboardError> {
        let since = self
            .since_for_window(time_window.as_deref())
            .map_err(DashboardError::TokenStats)?;
        let rows: Vec<(i64, String, f64)> = if let Some(since) = since {
            sqlx::query_as(
                "SELECT a.id,a.name,COALESCE(SUM(u.total_cost),0.0)::DOUBLE PRECISION AS cost \
                 FROM usage_logs u JOIN api_keys a ON a.id=u.api_key_id \
                 WHERE a.deleted_at=0 AND u.created_at >= $1 GROUP BY a.id,a.name \
                 ORDER BY cost DESC LIMIT 10",
            )
            .bind(since)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as(
                "SELECT a.id,a.name,COALESCE(SUM(u.total_cost),0.0)::DOUBLE PRECISION AS cost \
                 FROM usage_logs u JOIN api_keys a ON a.id=u.api_key_id \
                 WHERE a.deleted_at=0 GROUP BY a.id,a.name ORDER BY cost DESC LIMIT 10",
            )
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|error| token_error("failed to get cost by API key", error))?;
        Ok(rows
            .into_iter()
            .map(|(id, name, cost)| CostStatsByAPIKey {
                api_key_id: id.to_string().into(),
                api_key_name: name,
                cost,
            })
            .collect())
    }
}

#[async_trait]
impl DashboardServices for PgDashboardAdapter {
    async fn dashboard_overview(&self) -> Result<DashboardOverview, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.dashboard_overview()))
            .await
    }

    async fn request_stats(&self) -> Result<RequestStats, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.request_stats()))
            .await
    }

    async fn token_stats(&self) -> Result<TokenStats, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.token_stats()))
            .await
    }

    async fn request_stats_by_channel(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<RequestStatsByChannel>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.request_stats_by_channel(time_window.clone())))
            .await
    }

    async fn request_stats_by_model(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<RequestStatsByModel>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.request_stats_by_model(time_window.clone())))
            .await
    }

    async fn request_stats_by_api_key(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<RequestStatsByAPIKey>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.request_stats_by_api_key(time_window.clone())))
            .await
    }

    async fn token_stats_by_api_key(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<TokenStatsByAPIKey>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.token_stats_by_api_key(time_window.clone())))
            .await
    }

    async fn api_key_token_usage_stats(
        &self,
        input: Option<APIKeyTokenUsageStatsInput>,
    ) -> Result<Vec<APIKeyTokenUsageStats>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.api_key_token_usage_stats(input.clone())))
            .await
    }

    async fn daily_request_stats(&self) -> Result<Vec<DailyRequestStats>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.daily_request_stats()))
            .await
    }

    async fn hourly_request_stats(
        &self,
        date: Option<String>,
    ) -> Result<Vec<HourlyRequestStats>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.hourly_request_stats(date.clone())))
            .await
    }

    async fn top_requests_projects(&self) -> Result<Vec<TopRequestsProjects>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.top_requests_projects()))
            .await
    }

    async fn channel_success_rates(
        &self,
        time_window: Option<String>,
        limit: Option<i32>,
    ) -> Result<Vec<ChannelSuccessRate>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.channel_success_rates(time_window.clone(), limit)))
            .await
    }

    async fn fastest_channels(
        &self,
        input: FastestChannelsInput,
    ) -> Result<Vec<FastestChannel>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.fastest_channels(input.clone())))
            .await
    }

    async fn fastest_models(
        &self,
        input: FastestChannelsInput,
    ) -> Result<Vec<FastestModel>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.fastest_models(input.clone())))
            .await
    }

    async fn model_performance_stats(&self) -> Result<Vec<ModelPerformanceStat>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.model_performance_stats()))
            .await
    }

    async fn channel_performance_stats(
        &self,
    ) -> Result<Vec<ChannelPerformanceStat>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.channel_performance_stats()))
            .await
    }

    async fn token_stats_by_channel(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<TokenStatsByChannel>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.token_stats_by_channel(time_window.clone())))
            .await
    }

    async fn token_stats_by_model(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<TokenStatsByModel>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.token_stats_by_model(time_window.clone())))
            .await
    }

    async fn cost_stats_by_channel(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<CostStatsByChannel>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.cost_stats_by_channel(time_window.clone())))
            .await
    }

    async fn cost_stats_by_model(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<CostStatsByModel>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.cost_stats_by_model(time_window.clone())))
            .await
    }

    async fn cost_stats_by_api_key(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<CostStatsByAPIKey>, DashboardError> {
        self.eventual(|adapter| Box::pin(adapter.cost_stats_by_api_key(time_window.clone())))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    use sqlx::postgres::PgPoolOptions;

    type TestError = Box<dyn std::error::Error + Send + Sync>;

    fn local_time(offset: FixedOffset, date: NaiveDate, hour: u32, minute: u32) -> DateTime<Utc> {
        offset
            .from_local_datetime(&date.and_hms_opt(hour, minute, 0).expect("valid test time"))
            .single()
            .expect("fixed offsets have one local instant")
            .with_timezone(&Utc)
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[derive(Clone, Copy)]
    struct UsageSample<'a> {
        project_id: i64,
        api_key_id: i64,
        channel_id: i64,
        model_id: &'a str,
        request_status: &'a str,
        execution_status: &'a str,
        stream: bool,
        latency_ms: i64,
        ttft_ms: i64,
        prompt_tokens: i64,
        completion_tokens: i64,
        cached_tokens: i64,
        reasoning_tokens: i64,
        total_tokens: i64,
        total_cost: f64,
        created_at: DateTime<Utc>,
    }

    async fn insert_sample(pool: &PgPool, sample: UsageSample<'_>) -> Result<i64, sqlx::Error> {
        let request_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO requests \
             (api_key_id,project_id,model_id,request_body,channel_id,status,stream,created_at,updated_at) \
             VALUES($1,$2,$3,'{}'::jsonb,$4,$5,$6,$7,$7) RETURNING id",
        )
        .bind(sample.api_key_id)
        .bind(sample.project_id)
        .bind(sample.model_id)
        .bind(sample.channel_id)
        .bind(sample.request_status)
        .bind(sample.stream)
        .bind(sample.created_at)
        .fetch_one(pool)
        .await?;
        sqlx::query(
            "INSERT INTO request_executions \
             (project_id,request_id,channel_id,model_id,request_body,status,stream, \
              metrics_latency_ms,metrics_first_token_latency_ms,created_at,updated_at) \
             VALUES($1,$2,$3,$4,'{}'::jsonb,$5,$6,$7,$8,$9,$9)",
        )
        .bind(sample.project_id)
        .bind(request_id)
        .bind(sample.channel_id)
        .bind(sample.model_id)
        .bind(sample.execution_status)
        .bind(sample.stream)
        .bind(sample.latency_ms)
        .bind(sample.ttft_ms)
        .bind(sample.created_at)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO usage_logs \
             (request_id,api_key_id,channel_id,project_id,model_id,prompt_tokens, \
              completion_tokens,total_tokens,prompt_cached_tokens, \
              completion_reasoning_tokens,total_cost,created_at,updated_at) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$12)",
        )
        .bind(request_id)
        .bind(sample.api_key_id)
        .bind(sample.channel_id)
        .bind(sample.project_id)
        .bind(sample.model_id)
        .bind(sample.prompt_tokens)
        .bind(sample.completion_tokens)
        .bind(sample.total_tokens)
        .bind(sample.cached_tokens)
        .bind(sample.reasoning_tokens)
        .bind(sample.total_cost)
        .bind(sample.created_at)
        .execute(pool)
        .await?;
        Ok(request_id)
    }

    #[tokio::test]
    async fn dashboard_prefers_replica_and_falls_back_per_graphql_operation()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let master = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let replica = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        sqlx::query(
            "INSERT INTO requests(project_id,model_id,request_body,status) \
             VALUES(1,'dashboard-read-routing','{}'::jsonb,'completed')",
        )
        .execute(&master.pool)
        .await?;

        let adapter = PgDashboardAdapter::new(master.pool.clone())
            .with_read_pool(Some(replica.pool.clone()), true);
        let from_replica = adapter.dashboard_overview().await?;
        assert_eq!(from_replica.total_requests, 0);

        replica.pool.close().await;
        let from_master = adapter.dashboard_overview().await?;
        assert_eq!(from_master.total_requests, 1);

        let strict = PgDashboardAdapter::new(master.pool.clone())
            .with_read_pool(Some(replica.pool.clone()), false);
        assert!(strict.dashboard_overview().await.is_err());

        replica.cleanup().await?;
        master.cleanup().await?;
        Ok(())
    }

    #[tokio::test]
    async fn live_postgres_dashboard_runs_all_aggregates_with_local_buckets()
    -> Result<(), TestError> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };

        // Every connection in the test pool is pinned to an isolated schema.
        // This keeps top-10 queries deterministic even when the supplied test
        // database contains data from other opt-in integration tests.
        let admin_pool = PgPool::connect(&dsn).await?;
        let schema = format!("dashboard_test_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA \"{schema}\""))
            .execute(&admin_pool)
            .await?;
        let search_path = format!("SET search_path TO \"{schema}\"");
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(move |connection, _| {
                let search_path = search_path.clone();
                Box::pin(async move {
                    sqlx::query(&search_path).execute(connection).await?;
                    Ok(())
                })
            })
            .connect(&dsn)
            .await?;

        let outcome: Result<(), TestError> = async {
            conduit_db::connection::migrate_postgres_with_flag(&pool, false).await?;

            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let utc_now = Utc::now();
            let offset_hours = 12 - chrono::Timelike::hour(&utc_now) as i32;
            let offset = FixedOffset::east_opt(offset_hours * 60 * 60)
                .ok_or("failed to place the current hour at local noon")?;
            let today = utc_now.with_timezone(&offset).date_naive();
            let yesterday = today - Duration::days(1);
            let yesterday_late = local_time(offset, yesterday, 23, 30);
            let today_0030 = local_time(offset, today, 0, 30);
            let today_0130 = local_time(offset, today, 1, 30);
            let today_0230 = local_time(offset, today, 2, 30);

            let project_one = sqlx::query_scalar::<_, i64>(
                "INSERT INTO projects(name,description,status) \
                 VALUES($1,'first dashboard project','active') RETURNING id",
            )
            .bind(format!("dashboard-project-one-{suffix}"))
            .fetch_one(&pool)
            .await?;
            let project_two = sqlx::query_scalar::<_, i64>(
                "INSERT INTO projects(name,description,status) \
                 VALUES($1,'second dashboard project','active') RETURNING id",
            )
            .bind(format!("dashboard-project-two-{suffix}"))
            .fetch_one(&pool)
            .await?;
            let channel_one_name = format!("dashboard-channel-one-{suffix}");
            let channel_two_name = format!("dashboard-channel-two-{suffix}");
            let channel_one = sqlx::query_scalar::<_, i64>(
                "INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model) \
                 VALUES('openai',$1,'enabled','{}'::jsonb,'[]'::jsonb,'') RETURNING id",
            )
            .bind(&channel_one_name)
            .fetch_one(&pool)
            .await?;
            let channel_two = sqlx::query_scalar::<_, i64>(
                "INSERT INTO channels(type,name,status,credentials,supported_models,default_test_model) \
                 VALUES('anthropic',$1,'enabled','{}'::jsonb,'[]'::jsonb,'') RETURNING id",
            )
            .bind(&channel_two_name)
            .fetch_one(&pool)
            .await?;
            let api_key_one_name = format!("dashboard-key-one-{suffix}");
            let api_key_two_name = format!("dashboard-key-two-{suffix}");
            let api_key_one = sqlx::query_scalar::<_, i64>(
                "INSERT INTO api_keys(project_id,key,name,status) \
                 VALUES($1,$2,$3,'enabled') RETURNING id",
            )
            .bind(project_one)
            .bind(format!("conduit-dashboard-one-{suffix}"))
            .bind(&api_key_one_name)
            .fetch_one(&pool)
            .await?;
            let api_key_two = sqlx::query_scalar::<_, i64>(
                "INSERT INTO api_keys(project_id,key,name,status) \
                 VALUES($1,$2,$3,'enabled') RETURNING id",
            )
            .bind(project_two)
            .bind(format!("conduit-dashboard-two-{suffix}"))
            .bind(&api_key_two_name)
            .fetch_one(&pool)
            .await?;
            let model_one = format!("dashboard-model-one-{suffix}");
            let model_two = format!("dashboard-model-two-{suffix}");
            for (model_id, name) in [
                (&model_one, format!("Dashboard Model One {suffix}")),
                (&model_two, format!("Dashboard Model Two {suffix}")),
            ] {
                sqlx::query(
                    "INSERT INTO models \
                     (developer,model_id,type,name,icon,\"group\",model_card,settings,status) \
                     VALUES('test',$1,'chat',$2,'','test','{}'::jsonb,'{}'::jsonb,'enabled')",
                )
                .bind(model_id)
                .bind(name)
                .execute(&pool)
                .await?;
            }

            let samples = [
                UsageSample {
                    project_id: project_one,
                    api_key_id: api_key_one,
                    channel_id: channel_one,
                    model_id: &model_one,
                    request_status: "completed",
                    execution_status: "completed",
                    stream: false,
                    latency_ms: 1_000,
                    ttft_ms: 100,
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    cached_tokens: 2,
                    reasoning_tokens: 0,
                    total_tokens: 15,
                    total_cost: 0.25,
                    created_at: yesterday_late,
                },
                UsageSample {
                    project_id: project_one,
                    api_key_id: api_key_one,
                    channel_id: channel_one,
                    model_id: &model_one,
                    request_status: "completed",
                    execution_status: "completed",
                    stream: false,
                    latency_ms: 1_000,
                    ttft_ms: 200,
                    prompt_tokens: 100,
                    completion_tokens: 50,
                    cached_tokens: 20,
                    reasoning_tokens: 10,
                    total_tokens: 160,
                    total_cost: 1.25,
                    created_at: today_0030,
                },
                UsageSample {
                    project_id: project_two,
                    api_key_id: api_key_two,
                    channel_id: channel_two,
                    model_id: &model_two,
                    request_status: "failed",
                    execution_status: "failed",
                    stream: false,
                    latency_ms: 500,
                    ttft_ms: 150,
                    prompt_tokens: 200,
                    completion_tokens: 100,
                    cached_tokens: 30,
                    reasoning_tokens: 30,
                    total_tokens: 330,
                    total_cost: 2.5,
                    created_at: today_0130,
                },
                UsageSample {
                    project_id: project_two,
                    api_key_id: api_key_two,
                    channel_id: channel_two,
                    model_id: &model_two,
                    request_status: "completed",
                    execution_status: "completed",
                    stream: true,
                    latency_ms: 2_000,
                    ttft_ms: 500,
                    prompt_tokens: 300,
                    completion_tokens: 150,
                    cached_tokens: 40,
                    reasoning_tokens: 30,
                    total_tokens: 480,
                    total_cost: 3.75,
                    created_at: today_0230,
                },
            ];
            for sample in samples {
                insert_sample(&pool, sample).await?;
            }

            // A deliberately large settled customer charge must not affect any
            // Dashboard cost result: this service reports procurement cost only.
            sqlx::query(
                "INSERT INTO charge_settlements \
                 (charge_event_id,amount_micros,subscription_amount_micros,credit_amount_micros, \
                  status,detail_snapshot,created_at) \
                 VALUES(987654321,999000000,999000000,0,'settled','{}'::jsonb,$1)",
            )
            .bind(today_0230)
            .execute(&pool)
            .await?;

            let dashboard = PgDashboardAdapter::new(pool.clone()).with_offset(offset);

            let overview = dashboard.dashboard_overview().await?;
            assert_eq!(overview.total_requests, 4);
            assert_eq!(overview.failed_requests, 1);
            assert_eq!(overview.request_stats.requests_today, 3);

            let request_stats = dashboard.request_stats().await?;
            assert_eq!(request_stats.requests_today, 3);
            assert!(request_stats.requests_this_week >= 3);
            assert!(request_stats.requests_this_month >= 3);

            let token_stats = dashboard.token_stats().await?;
            assert_eq!(token_stats.total_input_tokens_today, 600);
            assert_eq!(token_stats.total_output_tokens_today, 300);
            assert_eq!(token_stats.total_cached_tokens_today, 90);
            assert_eq!(token_stats.total_input_tokens_all_time, 610);
            assert_eq!(token_stats.total_output_tokens_all_time, 305);
            assert_eq!(token_stats.total_cached_tokens_all_time, 92);

            let channels = dashboard
                .request_stats_by_channel(Some("day".to_string()))
                .await?;
            assert_eq!(
                channels
                    .iter()
                    .find(|row| row.channel_name == channel_one_name)
                    .map(|row| row.count),
                Some(1)
            );
            assert_eq!(
                channels
                    .iter()
                    .find(|row| row.channel_name == channel_two_name)
                    .map(|row| row.count),
                Some(2)
            );
            let models = dashboard
                .request_stats_by_model(Some("day".to_string()))
                .await?;
            assert_eq!(
                models
                    .iter()
                    .find(|row| row.model_id == model_one)
                    .map(|row| row.count),
                Some(1)
            );
            assert_eq!(
                models
                    .iter()
                    .find(|row| row.model_id == model_two)
                    .map(|row| row.count),
                Some(2)
            );
            let keys = dashboard
                .request_stats_by_api_key(Some("day".to_string()))
                .await?;
            assert_eq!(
                keys.iter()
                    .find(|row| row.api_key_id == api_key_one.to_string())
                    .map(|row| row.count),
                Some(1)
            );
            assert_eq!(
                keys.iter()
                    .find(|row| row.api_key_id == api_key_two.to_string())
                    .map(|row| row.count),
                Some(2)
            );

            let key_tokens = dashboard
                .token_stats_by_api_key(Some("day".to_string()))
                .await?;
            let key_two_tokens = key_tokens
                .iter()
                .find(|row| row.api_key_id.as_str() == api_key_two.to_string())
                .expect("second API key token stats");
            assert_eq!(key_two_tokens.input_tokens, 500);
            assert_eq!(key_two_tokens.output_tokens, 250);
            assert_eq!(key_two_tokens.cached_tokens, 70);
            assert_eq!(key_two_tokens.reasoning_tokens, 60);
            assert_eq!(key_two_tokens.total_tokens, 810);
            let selected_key_tokens = dashboard
                .api_key_token_usage_stats(Some(APIKeyTokenUsageStatsInput {
                    api_key_ids: Some(vec![api_key_one, api_key_two]),
                    time_window: Some("allTime".to_string()),
                }))
                .await?;
            let selected_one = selected_key_tokens
                .iter()
                .find(|row| row.api_key_id == api_key_one)
                .expect("selected first API key stats");
            assert_eq!(selected_one.total_input_tokens, 110);
            assert_eq!(selected_one.total_output_tokens, 55);
            assert_eq!(selected_one.total_cached_tokens, 22);

            let daily = dashboard.daily_request_stats().await?;
            assert_eq!(daily.len(), DAILY_STATS_DAYS as usize);
            let yesterday_row = daily
                .iter()
                .find(|row| row.date == yesterday.format("%Y-%m-%d").to_string())
                .expect("previous local day");
            assert_eq!(yesterday_row.count, 1);
            assert_eq!(yesterday_row.tokens, 15);
            assert_close(yesterday_row.cost, 0.25);
            let today_row = daily
                .iter()
                .find(|row| row.date == today.format("%Y-%m-%d").to_string())
                .expect("current local day");
            assert_eq!(today_row.count, 3);
            assert_eq!(today_row.tokens, 970);
            assert_close(today_row.cost, 7.5);

            let hourly = dashboard
                .hourly_request_stats(Some(today.format("%Y-%m-%d").to_string()))
                .await?;
            assert_eq!(hourly.len(), 24);
            assert_eq!(hourly[0].count, 1);
            assert_eq!(hourly[1].count, 1);
            assert_eq!(hourly[2].count, 1);

            let projects = dashboard.top_requests_projects().await?;
            assert_eq!(
                projects
                    .iter()
                    .find(|row| row.project_id == project_one.to_string())
                    .map(|row| row.request_count),
                Some(2)
            );
            assert_eq!(
                projects
                    .iter()
                    .find(|row| row.project_id == project_two.to_string())
                    .map(|row| row.request_count),
                Some(2)
            );

            let success = dashboard
                .channel_success_rates(Some("day".to_string()), None)
                .await?;
            let channel_one_success = success
                .iter()
                .find(|row| row.channel_id.as_str() == channel_one.to_string())
                .expect("first channel success stats");
            assert_eq!(channel_one_success.success_count, 1);
            assert_eq!(channel_one_success.failed_count, 0);
            assert_close(channel_one_success.success_rate, 100.0);
            let channel_two_success = success
                .iter()
                .find(|row| row.channel_id.as_str() == channel_two.to_string())
                .expect("second channel success stats");
            assert_eq!(channel_two_success.success_count, 1);
            assert_eq!(channel_two_success.failed_count, 1);
            assert_close(channel_two_success.success_rate, 50.0);

            let fastest_channels = dashboard
                .fastest_channels(FastestChannelsInput {
                    time_window: Some("day".to_string()),
                    limit: Some(10),
                })
                .await?;
            let fastest_channel_one = fastest_channels
                .iter()
                .find(|row| row.channel_id.as_str() == channel_one.to_string())
                .expect("first fastest channel");
            assert_close(fastest_channel_one.throughput, 60.0);
            let fastest_channel_two = fastest_channels
                .iter()
                .find(|row| row.channel_id.as_str() == channel_two.to_string())
                .expect("second fastest channel");
            assert_close(fastest_channel_two.throughput, 120.0);
            let fastest_models = dashboard
                .fastest_models(FastestChannelsInput {
                    time_window: Some("day".to_string()),
                    limit: Some(10),
                })
                .await?;
            assert_close(
                fastest_models
                    .iter()
                    .find(|row| row.model_id == model_one)
                    .expect("first fastest model")
                    .throughput,
                60.0,
            );
            assert_close(
                fastest_models
                    .iter()
                    .find(|row| row.model_id == model_two)
                    .expect("second fastest model")
                    .throughput,
                120.0,
            );

            let model_performance = dashboard.model_performance_stats().await?;
            assert_close(
                model_performance
                    .iter()
                    .find(|row| {
                        row.model_id == model_two
                            && row.date == today.format("%Y-%m-%d").to_string()
                    })
                    .and_then(|row| row.throughput)
                    .expect("second model daily throughput"),
                120.0,
            );

            // Without probes the channel chart must use completed executions.
            let execution_performance = dashboard.channel_performance_stats().await?;
            assert_close(
                execution_performance
                    .iter()
                    .find(|row| {
                        row.channel_id.as_str() == channel_one.to_string()
                            && row.date == today.format("%Y-%m-%d").to_string()
                    })
                    .and_then(|row| row.throughput)
                    .expect("execution-backed channel throughput"),
                60.0,
            );

            let tokens_by_channel = dashboard
                .token_stats_by_channel(Some("day".to_string()))
                .await?;
            assert_eq!(
                tokens_by_channel
                    .iter()
                    .find(|row| row.channel_id.as_str() == channel_two.to_string())
                    .map(|row| row.total_tokens),
                Some(810)
            );
            let tokens_by_model = dashboard
                .token_stats_by_model(Some("day".to_string()))
                .await?;
            assert_eq!(
                tokens_by_model
                    .iter()
                    .find(|row| row.model_id == model_one)
                    .map(|row| row.total_tokens),
                Some(160)
            );

            let costs_by_channel = dashboard
                .cost_stats_by_channel(Some("day".to_string()))
                .await?;
            assert_close(
                costs_by_channel
                    .iter()
                    .find(|row| row.channel_id.as_str() == channel_two.to_string())
                    .expect("second channel cost")
                    .cost,
                6.25,
            );
            let costs_by_model = dashboard
                .cost_stats_by_model(Some("day".to_string()))
                .await?;
            assert_close(
                costs_by_model
                    .iter()
                    .find(|row| row.model_id == model_one)
                    .expect("first model cost")
                    .cost,
                1.25,
            );
            let costs_by_key = dashboard
                .cost_stats_by_api_key(Some("day".to_string()))
                .await?;
            assert_close(
                costs_by_key
                    .iter()
                    .find(|row| row.api_key_id.as_str() == api_key_one.to_string())
                    .expect("first API key cost")
                    .cost,
                1.25,
            );

            // Probe rows take precedence and use request-count-weighted means;
            // the implausible >2000 tok/s sample must be ignored.
            let probe_at = local_time(offset, today, 3, 0).timestamp();
            for (channel_id, count, throughput, ttft) in [
                (channel_one, 2_i64, 100.0, 200.0),
                (channel_one, 6_i64, 200.0, 400.0),
                (channel_one, 100_i64, 3_000.0, 1.0),
                (channel_two, 4_i64, 80.0, 100.0),
            ] {
                sqlx::query(
                    "INSERT INTO channel_probes \
                     (channel_id,total_request_count,success_request_count, \
                      avg_tokens_per_second,avg_time_to_first_token_ms,timestamp) \
                     VALUES($1,$2,$2,$3,$4,$5)",
                )
                .bind(channel_id)
                .bind(count)
                .bind(throughput)
                .bind(ttft)
                .bind(probe_at)
                .execute(&pool)
                .await?;
            }
            let probe_performance = dashboard.channel_performance_stats().await?;
            let weighted = probe_performance
                .iter()
                .find(|row| {
                    row.channel_id.as_str() == channel_one.to_string()
                        && row.date == today.format("%Y-%m-%d").to_string()
                })
                .expect("probe-backed channel throughput");
            assert_eq!(weighted.request_count, 8);
            assert_close(weighted.throughput.expect("probe throughput"), 175.0);
            assert_close(weighted.ttft_ms.expect("probe TTFT"), 350.0);

            Ok(())
        }
        .await;

        pool.close().await;
        let cleanup = sqlx::query(&format!("DROP SCHEMA \"{schema}\" CASCADE"))
            .execute(&admin_pool)
            .await;
        admin_pool.close().await;
        outcome?;
        cleanup?;
        Ok(())
    }
}
