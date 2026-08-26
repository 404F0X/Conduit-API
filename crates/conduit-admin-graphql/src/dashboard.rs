//! RUST-P12-001 S07 (continuation) — Dashboard statistics GraphQL slice.
//!
//! Ports a cohesive subset of the dashboard operations declared in
//! `conduit/internal/server/gql/dashboard.graphql` (Go) and resolved in
//! `conduit/internal/server/gql/dashboard.resolvers.go`. The Rust SDL must
//! match the captured snapshot at `tests/contracts/admin_graphql_schema.graphql`
//! (lines 1098-1338 for the dashboard block).
//!
//! ## Operations ported this slice (3 queries)
//!
//!   - `Query.dashboardOverview: DashboardOverview!` — Go resolver
//!     `DashboardOverview` (`dashboard.resolvers.go` lines 38-85) groups
//!     `request.status` counts (total + failed) and embeds the
//!     [`RequestStats`] sub-object. The Go resolver intentionally leaves
//!     `averageResponseTime` as `nil` (TODO marker at line 81) — we mirror
//!     that by surfacing `Option<f64>` (None on the wire).
//!   - `Query.requestStats: RequestStats!` — Go resolver `RequestStats`
//!     (`dashboard.resolvers.go` lines 91-138) computes four calendar-period
//!     counts (today / this week / last week / this month) against
//!     `usage_logs`. Calendar period boundaries come from
//!     `xtime.GetCalendarPeriods(loc)`.
//!   - `Query.tokenStats: TokenStats!` — Go resolver `TokenStats`
//!     (`dashboard.resolvers.go` lines 684-882) aggregates input/output/cached
//!     token sums for the four calendar windows (today / this week / this
//!     month / all-time) plus a `lastUpdated: Time` stamp. The Go resolver
//!     uses a stale-while-revalidate cache for the all-time slice; we
//!     model the contract (return type + optional timestamp) and let the
//!     host service own caching policy.
//!
//! ## Operations NOT ported this slice (pending — left for follow-up slices)
//!
//! To keep the diff reviewable, only the three "scalar aggregate" queries
//! above are landed. The remaining dashboard operations carry heavier
//! dialect-specific SQL (`qb.BuildThroughputQuery`,
//! `qb.BuildDailyPerformanceStatsQuery`) or JOIN + sort + limit pipelines
//! that depend on crates outside this slice scope. They are:
//!
//!   - `requestStatsByChannel(timeWindow)` — usage_logs JOIN channels GROUP BY.
//!   - `requestStatsByModel(timeWindow)` — usage_logs GROUP BY model_id.
//!   - `requestStatsByAPIKey(timeWindow)` — usage_logs GROUP BY api_key_id + APIKey lookup.
//!   - `tokenStatsByAPIKey(timeWindow)` — token sums GROUP BY api_key_id.
//!   - `apiKeyTokenUsageStats(input)` — multi-key token usage + top models.
//!   - `dailyRequestStats` — 30-day dialect-specific date expr.
//!   - `topRequestsProjects` — usage_logs GROUP BY project_id + Project lookup.
//!   - `channelSuccessRates(timeWindow, limit)` — request_execution CASE WHEN.
//!   - `fastestChannels(input)` — `qb.BuildThroughputQuery` (dialect-aware).
//!   - `fastestModels(input)` — same.
//!   - `modelPerformanceStats` — daily throughput by model.
//!   - `channelPerformanceStats` — daily throughput by channel + probe stats.
//!   - `tokenStatsByChannel(timeWindow)` — token sums JOIN channels.
//!   - `tokenStatsByModel(timeWindow)` — token sums GROUP BY model_id.
//!   - `costStatsByChannel(timeWindow)` — cost SUM JOIN channels.
//!   - `costStatsByModel(timeWindow)` — cost SUM GROUP BY model_id.
//!   - `costStatsByAPIKey(timeWindow)` — cost SUM GROUP BY api_key_id + APIKey lookup.
//!
//! Each pending operation output type is NOT registered in this slice
//! (registering an unused type would surface as an invented SDL block in the
//! `sdl_matches_snapshot_for_dashboard_slice` parity test). A follow-up slice
//! will register them when at least one of their resolvers is ported.
//!
//! ## Service wiring
//!
//! The admin-graphql crate stays free of DB/IO concerns. The host wires a
//! concrete implementation of [`DashboardServices`] into the schema data bag
//! at build time; resolver-level tests inject an in-memory fake (mirrors the
//! dependency-injection pattern used by `system::SystemSettingsServices`).

use std::sync::Arc;

use async_graphql::{Context, ID, SimpleObject};

use crate::scalars::TimeScalar;

// ===========================================================================
// Output types — mirrors of the Go GraphQL types in `dashboard.graphql`.
// All field names are plain camelCase; no acronym rename is needed in this
// slice (unlike `apiKeyID` / `channelID` in the pending by-* slices).
// ===========================================================================

/// GraphQL `DashboardOverview` (snapshot lines 1098-1103).
///
/// Mirrors Go struct `DashboardOverview` (generated; see
/// `dashboard.resolvers.go:38-85`). The Go resolver leaves
/// `averageResponseTime` as `nil`; we surface it as `Option<f64>` so the
/// emitted SDL matches the snapshot nullable `Float` field (snapshot
/// line 1102).
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "DashboardOverview")]
pub struct DashboardOverview {
    /// `totalRequests: Int!` — sum over all `request.status` groups.
    pub total_requests: i32,
    /// `requestStats: RequestStats!` — embedded sub-object.
    pub request_stats: RequestStats,
    /// `failedRequests: Int!` — count where `status = failed`.
    pub failed_requests: i32,
    /// `averageResponseTime: Float` — TODO in Go (always nil). Mirrored
    /// as `Option<f64>` so SDL emits nullable `Float`.
    pub average_response_time: Option<f64>,
}

/// GraphQL `RequestStats` (snapshot lines 1105-1110).
///
/// Mirrors Go struct `RequestStats` populated by `dashboard.resolvers.go:
/// 91-138`. Each field is a `COUNT(*)` over `usage_logs` filtered by
/// `created_at` window boundaries produced from
/// `xtime.GetCalendarPeriods(loc)` (loc from `systemService.TimeLocation`).
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "RequestStats")]
pub struct RequestStats {
    /// `requestsToday: Int!`
    pub requests_today: i32,
    /// `requestsThisWeek: Int!`
    pub requests_this_week: i32,
    /// `requestsLastWeek: Int!`
    pub requests_last_week: i32,
    /// `requestsThisMonth: Int!`
    pub requests_this_month: i32,
}

/// GraphQL `TokenStats` (snapshot lines 1173-1190).
///
/// Mirrors Go struct `TokenStats` populated by `dashboard.resolvers.go:
/// 684-882`. The four windows (today / this week / this month / all-time)
/// each carry input/output/cached token sums; `lastUpdated` is the
/// cache-timestamp of the all-time slice (`nil` when the synchronous
/// fallback path failed — we mirror that with `Option`).
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "TokenStats")]
pub struct TokenStats {
    /// `totalInputTokensToday: Int!`
    pub total_input_tokens_today: i32,
    /// `totalOutputTokensToday: Int!`
    pub total_output_tokens_today: i32,
    /// `totalCachedTokensToday: Int!`
    pub total_cached_tokens_today: i32,
    /// `totalInputTokensThisWeek: Int!`
    pub total_input_tokens_this_week: i32,
    /// `totalOutputTokensThisWeek: Int!`
    pub total_output_tokens_this_week: i32,
    /// `totalCachedTokensThisWeek: Int!`
    pub total_cached_tokens_this_week: i32,
    /// `totalInputTokensThisMonth: Int!`
    pub total_input_tokens_this_month: i32,
    /// `totalOutputTokensThisMonth: Int!`
    pub total_output_tokens_this_month: i32,
    /// `totalCachedTokensThisMonth: Int!`
    pub total_cached_tokens_this_month: i32,
    /// `totalInputTokensAllTime: Int!`
    pub total_input_tokens_all_time: i32,
    /// `totalOutputTokensAllTime: Int!`
    pub total_output_tokens_all_time: i32,
    /// `totalCachedTokensAllTime: Int!`
    pub total_cached_tokens_all_time: i32,
    /// `lastUpdated: Time` — cache timestamp for the all-time slice. Go
    /// populates this whenever the all-time slice completes (sync or
    /// cache-hit). `None` only on hard-TTL + sync-failure. The snapshot
    /// uses the gqlgen `Time` scalar (snapshot line 1189).
    pub last_updated: Option<TimeScalar>,
}

// ===========================================================================
// Output types — remaining dashboard analytics (stub types for compilation).
// These mirror the Go GraphQL types declared in `dashboard.graphql` and
// resolved in `dashboard.resolvers.go`. Full field sets will be populated
// when the host service implementations land.
// ===========================================================================

/// GraphQL `RequestStatsByChannel` — per-channel request breakdown.
/// Go: `channelName: String!`, `count: Int!`.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "RequestStatsByChannel")]
pub struct RequestStatsByChannel {
    pub channel_name: String,
    pub count: i32,
}

/// GraphQL `RequestStatsByModel` — per-model request breakdown.
/// Go: `modelId: String!`, `count: Int!`.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "RequestStatsByModel")]
pub struct RequestStatsByModel {
    pub model_id: String,
    pub count: i32,
}

/// GraphQL `RequestStatsByAPIKey` — per-API-key request breakdown.
/// Go: `apiKeyId: ID!`, `apiKeyName: String!`, `count: Int!`.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "RequestStatsByAPIKey")]
pub struct RequestStatsByAPIKey {
    #[graphql(name = "apiKeyId")]
    pub api_key_id: String,
    pub api_key_name: String,
    pub count: i32,
}

/// GraphQL `TokenStatsByAPIKey` — per-API-key token breakdown.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "TokenStatsByAPIKey")]
pub struct TokenStatsByAPIKey {
    pub api_key_id: ID,
    pub api_key_name: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cached_tokens: i32,
    pub reasoning_tokens: i32,
    pub total_tokens: i32,
}

/// GraphQL `APIKeyTokenUsageStatsInput` — input for API-key token usage query.
#[derive(Debug, Clone, Default, PartialEq, async_graphql::InputObject)]
#[graphql(name = "APIKeyTokenUsageStatsInput")]
pub struct APIKeyTokenUsageStatsInput {
    pub api_key_ids: Option<Vec<i64>>,
    pub time_window: Option<String>,
}

/// GraphQL `APIKeyTokenUsageStats` — per-API-key token usage stats.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyTokenUsageStats")]
pub struct APIKeyTokenUsageStats {
    pub api_key_id: i64,
    pub api_key_name: String,
    pub total_input_tokens: i32,
    pub total_output_tokens: i32,
    pub total_cached_tokens: i32,
}

/// GraphQL `DailyRequestStats` — daily request count.
/// Go: `date: String!`, `count: Int!`, `tokens: Int!`, `cost: Float!`.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "DailyRequestStats")]
pub struct DailyRequestStats {
    pub date: String,
    pub count: i32,
    pub tokens: i32,
    pub cost: f64,
}

/// GraphQL `HourlyRequestStats` — hourly request count.
/// Go: `hour: Int!`, `count: Int!`.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "HourlyRequestStats")]
pub struct HourlyRequestStats {
    pub hour: i32,
    pub count: i32,
}

/// GraphQL `TopRequestsProjects` — top projects by request count.
/// Go: `projectId: ID!`, `projectName: String!`, `projectDescription: String!`, `requestCount: Int!`.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "TopRequestsProjects")]
pub struct TopRequestsProjects {
    pub project_id: String,
    pub project_name: String,
    pub project_description: String,
    pub request_count: i32,
}

/// GraphQL `ChannelSuccessRate` — per-channel success rate.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "ChannelSuccessRate")]
pub struct ChannelSuccessRate {
    pub channel_id: ID,
    pub channel_name: String,
    pub channel_type: String,
    pub channel_disabled: bool,
    pub success_count: i32,
    pub failed_count: i32,
    pub total_count: i32,
    pub success_rate: f64,
}

/// GraphQL `FastestChannelsInput` — input for fastest channels/models query.
#[derive(Debug, Clone, Default, PartialEq, async_graphql::InputObject)]
#[graphql(name = "FastestChannelsInput")]
pub struct FastestChannelsInput {
    pub time_window: Option<String>,
    pub limit: Option<i32>,
}

/// GraphQL `FastestChannel` — throughput-ranked channel.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "FastestChannel")]
pub struct FastestChannel {
    pub channel_id: ID,
    pub channel_name: String,
    pub channel_type: String,
    pub throughput: f64,
    pub tokens_count: i32,
    pub latency_ms: i32,
    pub request_count: i32,
}

/// GraphQL `FastestModel` — throughput-ranked model.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "FastestModel")]
pub struct FastestModel {
    pub model_id: String,
    pub model_name: String,
    pub throughput: f64,
    pub tokens_count: i32,
    pub latency_ms: i32,
    pub request_count: i32,
}

/// GraphQL `ModelPerformanceStat` — model performance stats.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "ModelPerformanceStat")]
pub struct ModelPerformanceStat {
    pub date: String,
    pub model_id: String,
    pub throughput: Option<f64>,
    pub ttft_ms: Option<f64>,
    pub request_count: i32,
}

/// GraphQL `ChannelPerformanceStat` — channel performance stats.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "ChannelPerformanceStat")]
pub struct ChannelPerformanceStat {
    pub date: String,
    pub channel_id: ID,
    pub channel_name: String,
    pub throughput: Option<f64>,
    pub ttft_ms: Option<f64>,
    pub request_count: i32,
}

/// GraphQL `TokenStatsByChannel` — per-channel token breakdown.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "TokenStatsByChannel")]
pub struct TokenStatsByChannel {
    pub channel_id: ID,
    pub channel_name: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cached_tokens: i32,
    pub reasoning_tokens: i32,
    pub total_tokens: i32,
}

/// GraphQL `TokenStatsByModel` — per-model token breakdown.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "TokenStatsByModel")]
pub struct TokenStatsByModel {
    pub model_id: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cached_tokens: i32,
    pub reasoning_tokens: i32,
    pub total_tokens: i32,
}

/// GraphQL `CostStatsByChannel` — per-channel cost breakdown.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "CostStatsByChannel")]
pub struct CostStatsByChannel {
    pub channel_id: ID,
    pub channel_name: String,
    pub cost: f64,
}

/// GraphQL `CostStatsByModel` — per-model cost breakdown.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "CostStatsByModel")]
pub struct CostStatsByModel {
    pub model_id: String,
    pub cost: f64,
}

/// GraphQL `CostStatsByAPIKey` — per-API-key cost breakdown.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "CostStatsByAPIKey")]
pub struct CostStatsByAPIKey {
    pub api_key_id: ID,
    pub api_key_name: String,
    pub cost: f64,
}

// ===========================================================================
// Service trait (host-injected)
// ===========================================================================

/// Error surface for the dashboard slice. The GraphQL layer surfaces these
/// as field errors; messages mirror the Go `fmt.Errorf("...: %w")` prefixes
/// (e.g. `dashboard.resolvers.go:187` "failed to get requests by channel").
#[derive(Debug, Clone, thiserror::Error)]
pub enum DashboardError {
    #[error("dashboard service is not available")]
    ServiceUnavailable,
    #[error("failed to get dashboard overview: {0}")]
    Overview(String),
    #[error("failed to get request stats: {0}")]
    RequestStats(String),
    #[error("failed to get token stats: {0}")]
    TokenStats(String),
}

/// Trait the host wires to back the dashboard operations. Each method
/// corresponds to one Go resolver; the signatures stay synchronous to the
/// resolver layer (the trait itself is async-trait-shaped via async-graphql
/// `async` resolver methods).
///
/// The host owns all calendar-period / timezone concerns (Go reaches into
/// `systemService.TimeLocation(ctx)` + `xtime.GetCalendarPeriods(loc)`) so
/// the admin-graphql crate stays free of those dependencies.
#[async_trait::async_trait]
pub trait DashboardServices: Send + Sync {
    /// Mirrors Go resolver `DashboardOverview` (`dashboard.resolvers.go:38-85`):
    /// return the aggregate (`totalRequests`, `failedRequests`, embedded
    /// [`RequestStats`], and the placeholder `averageResponseTime` which
    /// the Go resolver always leaves as `nil` — surface `None` here).
    async fn dashboard_overview(&self) -> Result<DashboardOverview, DashboardError>;

    /// Mirrors Go resolver `RequestStats` (`dashboard.resolvers.go:91-138`):
    /// compute the four calendar-period counts against `usage_logs`.
    async fn request_stats(&self) -> Result<RequestStats, DashboardError>;

    /// Mirrors Go resolver `TokenStats` (`dashboard.resolvers.go:684-882`):
    /// compute the twelve token sums across the four windows and the
    /// `lastUpdated` cache timestamp (or `None` when the all-time slice
    /// is unavailable). The host owns the stale-while-revalidate logic.
    async fn token_stats(&self) -> Result<TokenStats, DashboardError>;

    /// `requestStatsByChannel(timeWindow)` — usage_logs JOIN channels GROUP BY.
    async fn request_stats_by_channel(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<RequestStatsByChannel>, DashboardError>;

    /// `requestStatsByModel(timeWindow)` — usage_logs GROUP BY model_id.
    async fn request_stats_by_model(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<RequestStatsByModel>, DashboardError>;

    /// `requestStatsByAPIKey(timeWindow)` — usage_logs GROUP BY api_key_id.
    async fn request_stats_by_api_key(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<RequestStatsByAPIKey>, DashboardError>;

    /// `tokenStatsByAPIKey(timeWindow)` — token sums GROUP BY api_key_id.
    async fn token_stats_by_api_key(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<TokenStatsByAPIKey>, DashboardError>;

    /// `apiKeyTokenUsageStats(input)` — multi-key token usage.
    async fn api_key_token_usage_stats(
        &self,
        input: Option<APIKeyTokenUsageStatsInput>,
    ) -> Result<Vec<APIKeyTokenUsageStats>, DashboardError>;

    /// `dailyRequestStats` — 30-day daily counts.
    async fn daily_request_stats(&self) -> Result<Vec<DailyRequestStats>, DashboardError>;

    /// `hourlyRequestStats(date)` — hourly counts for a given date.
    async fn hourly_request_stats(
        &self,
        date: Option<String>,
    ) -> Result<Vec<HourlyRequestStats>, DashboardError>;

    /// `topRequestsProjects` — top projects by request count.
    async fn top_requests_projects(&self) -> Result<Vec<TopRequestsProjects>, DashboardError>;

    /// `channelSuccessRates(timeWindow, limit)` — per-channel success rates.
    async fn channel_success_rates(
        &self,
        time_window: Option<String>,
        limit: Option<i32>,
    ) -> Result<Vec<ChannelSuccessRate>, DashboardError>;

    /// `fastestChannels(input)` — throughput-ranked channels.
    async fn fastest_channels(
        &self,
        input: FastestChannelsInput,
    ) -> Result<Vec<FastestChannel>, DashboardError>;

    /// `fastestModels(input)` — throughput-ranked models.
    async fn fastest_models(
        &self,
        input: FastestChannelsInput,
    ) -> Result<Vec<FastestModel>, DashboardError>;

    /// `modelPerformanceStats` — model performance stats.
    async fn model_performance_stats(&self) -> Result<Vec<ModelPerformanceStat>, DashboardError>;

    /// `channelPerformanceStats` — channel performance stats.
    async fn channel_performance_stats(
        &self,
    ) -> Result<Vec<ChannelPerformanceStat>, DashboardError>;

    /// `tokenStatsByChannel(timeWindow)` — per-channel token breakdown.
    async fn token_stats_by_channel(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<TokenStatsByChannel>, DashboardError>;

    /// `tokenStatsByModel(timeWindow)` — per-model token breakdown.
    async fn token_stats_by_model(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<TokenStatsByModel>, DashboardError>;

    /// `costStatsByChannel(timeWindow)` — per-channel cost breakdown.
    async fn cost_stats_by_channel(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<CostStatsByChannel>, DashboardError>;

    /// `costStatsByModel(timeWindow)` — per-model cost breakdown.
    async fn cost_stats_by_model(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<CostStatsByModel>, DashboardError>;

    /// `costStatsByAPIKey(timeWindow)` — per-API-key cost breakdown.
    async fn cost_stats_by_api_key(
        &self,
        time_window: Option<String>,
    ) -> Result<Vec<CostStatsByAPIKey>, DashboardError>;
}

/// Resolves the injected [`DashboardServices`] from the async-graphql context
/// data bag. If no service was wired (e.g. the bare SDL-smoke schema),
/// returns the Go-equivalent "service unavailable" error message.
pub(crate) fn dashboard_services(ctx: &Context<'_>) -> Result<Arc<dyn DashboardServices>, String> {
    match ctx.data::<Arc<dyn DashboardServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(DashboardError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Resolver wiring — Query methods live on `QueryRoot` (lib.rs). The
// `#[Object] impl QueryRoot` macro cannot be split across modules, so this
// slice exposes the typed service-lookup helper and the QueryRoot in lib.rs
// delegates to it.
// ===========================================================================

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_graphql::{EmptySubscription, Name, Schema, Value};
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::mutation::MutationRoot;

    /// Build a `DateTime<Utc>` from a known-good timestamp without panicking.
    fn ts(
        y: i32,
        m: u32,
        d: u32,
        h: u32,
        mi: u32,
        s: u32,
    ) -> Result<chrono::DateTime<Utc>, String> {
        match Utc.with_ymd_and_hms(y, m, d, h, mi, s) {
            chrono::LocalResult::Single(dt) => Ok(dt),
            other => Err(format!("ambiguous/invalid timestamp: {other:?}")),
        }
    }

    /// Extract the inner `Value::Object` or panic with a clear message.
    fn as_object(value: &Value) -> &async_graphql::indexmap::IndexMap<Name, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected Value::Object, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------
    // In-memory fake service. Mirrors the Go resolver call sequence without
    // touching any DB / HTTP. Tests override only the fields they care about.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct FakeDashboardServices {
        overview: DashboardOverview,
        overview_error: Option<DashboardError>,
        request_stats: RequestStats,
        request_stats_error: Option<DashboardError>,
        token_stats: TokenStats,
        token_stats_error: Option<DashboardError>,
    }

    #[async_trait::async_trait]
    impl DashboardServices for FakeDashboardServices {
        async fn dashboard_overview(&self) -> Result<DashboardOverview, DashboardError> {
            match &self.overview_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.overview.clone()),
            }
        }

        async fn request_stats(&self) -> Result<RequestStats, DashboardError> {
            match &self.request_stats_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.request_stats.clone()),
            }
        }

        async fn token_stats(&self) -> Result<TokenStats, DashboardError> {
            match &self.token_stats_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.token_stats.clone()),
            }
        }

        async fn request_stats_by_channel(
            &self,
            _time_window: Option<String>,
        ) -> Result<Vec<RequestStatsByChannel>, DashboardError> {
            Ok(vec![])
        }

        async fn request_stats_by_model(
            &self,
            _time_window: Option<String>,
        ) -> Result<Vec<RequestStatsByModel>, DashboardError> {
            Ok(vec![])
        }

        async fn request_stats_by_api_key(
            &self,
            _time_window: Option<String>,
        ) -> Result<Vec<RequestStatsByAPIKey>, DashboardError> {
            Ok(vec![])
        }

        async fn token_stats_by_api_key(
            &self,
            _time_window: Option<String>,
        ) -> Result<Vec<TokenStatsByAPIKey>, DashboardError> {
            Ok(vec![])
        }

        async fn api_key_token_usage_stats(
            &self,
            _input: Option<APIKeyTokenUsageStatsInput>,
        ) -> Result<Vec<APIKeyTokenUsageStats>, DashboardError> {
            Ok(vec![])
        }

        async fn daily_request_stats(&self) -> Result<Vec<DailyRequestStats>, DashboardError> {
            Ok(vec![])
        }

        async fn hourly_request_stats(
            &self,
            _date: Option<String>,
        ) -> Result<Vec<HourlyRequestStats>, DashboardError> {
            Ok(vec![])
        }

        async fn top_requests_projects(&self) -> Result<Vec<TopRequestsProjects>, DashboardError> {
            Ok(vec![])
        }

        async fn channel_success_rates(
            &self,
            _time_window: Option<String>,
            _limit: Option<i32>,
        ) -> Result<Vec<ChannelSuccessRate>, DashboardError> {
            Ok(vec![])
        }

        async fn fastest_channels(
            &self,
            _input: FastestChannelsInput,
        ) -> Result<Vec<FastestChannel>, DashboardError> {
            Ok(vec![])
        }

        async fn fastest_models(
            &self,
            _input: FastestChannelsInput,
        ) -> Result<Vec<FastestModel>, DashboardError> {
            Ok(vec![])
        }

        async fn model_performance_stats(
            &self,
        ) -> Result<Vec<ModelPerformanceStat>, DashboardError> {
            Ok(vec![])
        }

        async fn channel_performance_stats(
            &self,
        ) -> Result<Vec<ChannelPerformanceStat>, DashboardError> {
            Ok(vec![])
        }

        async fn token_stats_by_channel(
            &self,
            _time_window: Option<String>,
        ) -> Result<Vec<TokenStatsByChannel>, DashboardError> {
            Ok(vec![])
        }

        async fn token_stats_by_model(
            &self,
            _time_window: Option<String>,
        ) -> Result<Vec<TokenStatsByModel>, DashboardError> {
            Ok(vec![])
        }

        async fn cost_stats_by_channel(
            &self,
            _time_window: Option<String>,
        ) -> Result<Vec<CostStatsByChannel>, DashboardError> {
            Ok(vec![])
        }

        async fn cost_stats_by_model(
            &self,
            _time_window: Option<String>,
        ) -> Result<Vec<CostStatsByModel>, DashboardError> {
            Ok(vec![])
        }

        async fn cost_stats_by_api_key(
            &self,
            _time_window: Option<String>,
        ) -> Result<Vec<CostStatsByAPIKey>, DashboardError> {
            Ok(vec![])
        }
    }

    type TestSchema = Schema<crate::QueryRoot, MutationRoot, EmptySubscription>;

    fn schema_with_services(services: FakeDashboardServices) -> TestSchema {
        let arc: Arc<dyn DashboardServices> = Arc::new(services);
        crate::admin_schema_builder().data(arc).finish()
    }

    fn data_object<const N: usize>(fields: [(&'static str, Value); N]) -> Value {
        let mut map = async_graphql::indexmap::IndexMap::new();
        for (name, value) in fields {
            map.insert(Name::new(name), value);
        }
        Value::Object(map)
    }

    // ---- SDL parity ---------------------------------------------------

    /// Cross-check: the SDL the resolver emits must agree with the captured
    /// snapshot at `tests/contracts/admin_graphql_schema.graphql` for the
    /// dashboard slice. Uses `sdl_parity::assert_block_parity` so the
    /// comparison ignores field ordering, directives, and default values.
    #[test]
    fn sdl_matches_snapshot_for_dashboard_slice() -> Result<(), Box<dyn std::error::Error>> {
        let arc: Arc<dyn DashboardServices> = Arc::new(FakeDashboardServices::default());
        let sdl = crate::admin_schema_builder().data(arc).finish().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;

        // Every output type ported this slice — full field set must match
        // exactly. None of these types carry acronym fields, so no pending
        // list is needed.
        for header in [
            "type DashboardOverview",
            "type RequestStats",
            "type TokenStats",
        ] {
            crate::sdl_parity::assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }

        Ok(())
    }

    // ---- resolver semantics: dashboardOverview ------------------------

    #[tokio::test]
    async fn dashboard_overview_resolver_returns_service_payload() {
        // Go resolver (dashboard.resolvers.go:38-85) wraps the RequestStats
        // sub-resolver result; we mirror by embedding it directly.
        let inner = RequestStats {
            requests_today: 7,
            requests_this_week: 50,
            requests_last_week: 40,
            requests_this_month: 200,
        };
        let services = FakeDashboardServices {
            overview: DashboardOverview {
                total_requests: 100,
                request_stats: inner.clone(),
                failed_requests: 5,
                // Go always leaves this nil (TODO at line 81).
                average_response_time: None,
            },
            ..Default::default()
        };
        let schema = schema_with_services(services);

        let resp = schema
            .execute(
                "{ dashboardOverview { totalRequests failedRequests averageResponseTime \
                  requestStats { requestsToday requestsThisWeek requestsLastWeek requestsThisMonth } } }",
            )
            .await;
        assert!(
            resp.errors.is_empty(),
            "unexpected errors: {:?}",
            resp.errors
        );

        let expected = data_object([(
            "dashboardOverview",
            data_object([
                ("totalRequests", Value::Number(100.into())),
                ("failedRequests", Value::Number(5.into())),
                ("averageResponseTime", Value::Null),
                (
                    "requestStats",
                    data_object([
                        ("requestsToday", Value::Number(7.into())),
                        ("requestsThisWeek", Value::Number(50.into())),
                        ("requestsLastWeek", Value::Number(40.into())),
                        ("requestsThisMonth", Value::Number(200.into())),
                    ]),
                ),
            ]),
        )]);
        assert_eq!(resp.data, expected);
    }

    #[tokio::test]
    async fn dashboard_overview_resolver_surfaces_error() {
        let services = FakeDashboardServices {
            overview_error: Some(DashboardError::Overview("boom".to_string())),
            ..Default::default()
        };
        let schema = schema_with_services(services);

        let resp = schema
            .execute("{ dashboardOverview { totalRequests } }")
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to get dashboard overview: boom"),
            "unexpected msg: {msg}"
        );
    }

    // ---- resolver semantics: requestStats -----------------------------

    #[tokio::test]
    async fn request_stats_resolver_returns_service_payload() {
        let services = FakeDashboardServices {
            request_stats: RequestStats {
                requests_today: 1,
                requests_this_week: 2,
                requests_last_week: 3,
                requests_this_month: 4,
            },
            ..Default::default()
        };
        let schema = schema_with_services(services);

        let resp = schema
            .execute(
                "{ requestStats { requestsToday requestsThisWeek requestsLastWeek requestsThisMonth } }",
            )
            .await;
        assert!(
            resp.errors.is_empty(),
            "unexpected errors: {:?}",
            resp.errors
        );

        let expected = data_object([(
            "requestStats",
            data_object([
                ("requestsToday", Value::Number(1.into())),
                ("requestsThisWeek", Value::Number(2.into())),
                ("requestsLastWeek", Value::Number(3.into())),
                ("requestsThisMonth", Value::Number(4.into())),
            ]),
        )]);
        assert_eq!(resp.data, expected);
    }

    #[tokio::test]
    async fn request_stats_resolver_surfaces_error() {
        let services = FakeDashboardServices {
            request_stats_error: Some(DashboardError::RequestStats("offline".to_string())),
            ..Default::default()
        };
        let schema = schema_with_services(services);

        let resp = schema.execute("{ requestStats { requestsToday } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to get request stats: offline"),
            "unexpected msg: {msg}"
        );
    }

    // ---- resolver semantics: tokenStats -------------------------------

    #[tokio::test]
    async fn token_stats_resolver_returns_service_payload_with_last_updated() {
        let last_updated = match ts(2026, 7, 6, 12, 0, 0) {
            Ok(dt) => dt,
            Err(msg) => panic!("hardcoded timestamp should always parse: {msg}"),
        };
        let services = FakeDashboardServices {
            token_stats: TokenStats {
                total_input_tokens_today: 10,
                total_output_tokens_today: 20,
                total_cached_tokens_today: 30,
                total_input_tokens_this_week: 40,
                total_output_tokens_this_week: 50,
                total_cached_tokens_this_week: 60,
                total_input_tokens_this_month: 70,
                total_output_tokens_this_month: 80,
                total_cached_tokens_this_month: 90,
                total_input_tokens_all_time: 100,
                total_output_tokens_all_time: 110,
                total_cached_tokens_all_time: 120,
                last_updated: Some(TimeScalar(last_updated)),
            },
            ..Default::default()
        };
        let schema = schema_with_services(services);

        // Pull `lastUpdated` plus one token field so we exercise both the
        // Time scalar and the Int path.
        let resp = schema
            .execute("{ tokenStats { totalInputTokensToday totalInputTokensAllTime lastUpdated } }")
            .await;
        assert!(
            resp.errors.is_empty(),
            "unexpected errors: {:?}",
            resp.errors
        );

        let map = as_object(&resp.data);
        let inner = match map.get("tokenStats") {
            Some(Value::Object(o)) => o,
            other => panic!("expected tokenStats object, got {other:?}"),
        };
        assert_eq!(
            inner.get("totalInputTokensToday"),
            Some(&Value::Number(10.into()))
        );
        assert_eq!(
            inner.get("totalInputTokensAllTime"),
            Some(&Value::Number(100.into()))
        );
        let last = match inner.get("lastUpdated") {
            Some(v) => v,
            None => panic!("lastUpdated field missing from response"),
        };
        match last {
            Value::String(s) => {
                // Time scalar wire format is RFC3339 (scalars.rs TimeScalar).
                assert!(
                    s.starts_with("2026-07-06T12:00:00"),
                    "unexpected lastUpdated: {s}"
                );
            }
            other => panic!("expected String for lastUpdated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn token_stats_resolver_emits_null_last_updated_when_none() {
        // Mirrors Go hard-TTL + sync-failure path (dashboard.resolvers.go:
        // 870-872): all-time numbers are zeroed and `lastUpdated` is nil.
        let services = FakeDashboardServices {
            token_stats: TokenStats {
                last_updated: None,
                ..Default::default()
            },
            ..Default::default()
        };
        let schema = schema_with_services(services);

        let resp = schema.execute("{ tokenStats { lastUpdated } }").await;
        assert!(
            resp.errors.is_empty(),
            "unexpected errors: {:?}",
            resp.errors
        );

        let map = as_object(&resp.data);
        let inner = match map.get("tokenStats") {
            Some(Value::Object(o)) => o,
            other => panic!("expected tokenStats object, got {other:?}"),
        };
        assert_eq!(inner.get("lastUpdated"), Some(&Value::Null));
    }

    #[tokio::test]
    async fn token_stats_resolver_surfaces_error() {
        let services = FakeDashboardServices {
            token_stats_error: Some(DashboardError::TokenStats("down".to_string())),
            ..Default::default()
        };
        let schema = schema_with_services(services);

        let resp = schema
            .execute("{ tokenStats { totalInputTokensToday } }")
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to get token stats: down"),
            "unexpected msg: {msg}"
        );
    }

    // ---- service-unavailable fallback --------------------------------

    #[tokio::test]
    async fn resolvers_surface_service_unavailable_when_unwired() {
        // Schema with NO dashboard service injected — every resolver must
        // surface the "service unavailable" failure mode (mirrors the
        // `system.rs` slice unwired test).
        let schema: TestSchema = crate::admin_schema_builder().finish();

        for query in [
            "{ dashboardOverview { totalRequests } }",
            "{ requestStats { requestsToday } }",
            "{ tokenStats { totalInputTokensToday } }",
        ] {
            let resp = schema.execute(query).await;
            assert_eq!(resp.errors.len(), 1, "query: {query}");
            let msg = format!("{}", resp.errors[0]);
            assert!(
                msg.contains("dashboard service is not available"),
                "unexpected msg for `{query}`: {msg}"
            );
        }
    }
}
