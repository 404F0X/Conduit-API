//! ADPT-REQUESTS — host adapters wiring the admin GraphQL Request/UsageLog
//! query domains to the PostgreSQL-backed repository contracts.
//!
//! Implements two host-injected service seams:
//!   - [`conduit_admin_graphql::request_usage::RequestQueryServices`] — the
//!     `Query.requests` connection query. Backed by [`RequestAdapter`].
//!   - [`conduit_admin_graphql::request_usage::UsageLogQueryServices`] — the
//!     `Query.usageLogs` connection query. Backed by [`UsageLogAdapter`].
//!
//! ## Go parity anchors
//!
//! ### Request connection (`Query.requests`)
//! - Go resolver `Query.requests` (ent.resolvers.go) loads the ent connection
//!   over `requests` with pagination + filtering. We port the bounded-materialization
//!   strategy: repo returns all rows (admin-scale table), adapter applies
//!   Relay forward pagination in-memory.
//! - `RequestWhereInput` predicates: project_id, status, source, model_id are
//!   covered; complex predicates (hasChannel, hasApiKey) are deferred.
//! - Sorting: `CREATED_AT`/`UPDATED_AT` ASC/DESC mirrors Go.
//!
//! ### UsageLog connection (`Query.usageLogs`)
//! - Similar pattern: repo loads all rows, adapter paginates in-memory.
//! - `UsageLogWhereInput`: project_id, channel_id covered.

use std::sync::Arc;

use async_trait::async_trait;

use conduit_admin_graphql::pagination::PageInfo;
use conduit_admin_graphql::request_usage::{
    CostItem, PriceItemCode, Request, RequestConnection, RequestConnectionArgs, RequestEdge,
    RequestOrderSelection, RequestOrderTerm, RequestQueryError, RequestQueryServices,
    RequestSource, RequestStatus, TierCost, UsageLog, UsageLogConnection, UsageLogConnectionArgs,
    UsageLogEdge, UsageLogOrderSelection, UsageLogOrderTerm, UsageLogQueryError,
    UsageLogQueryServices,
};
use conduit_admin_graphql::scalars::{
    CursorScalar, DecimalScalar, JsonRawMessageScalar, TimeScalar,
};
use conduit_db::RequestContext;
use conduit_db::repo::request_repo::{RequestListQuery, RequestListResult, RequestRepo};
use conduit_db::repo::usage_repo::{UsageListQuery, UsageListResult, UsageRepo};
use conduit_db::row::{RequestRow, UsageLogRow};
use rust_decimal::Decimal;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Request adapter
// ---------------------------------------------------------------------------

/// GraphQL-facing [`RequestQueryServices`] adapter backed by the live
/// [`conduit_db::repo::request_repo::RequestRepo`].
pub struct RequestAdapter {
    repo: Arc<dyn RequestRepo>,
}

impl RequestAdapter {
    pub fn new(repo: Arc<dyn RequestRepo>) -> Self {
        Self { repo }
    }

    /// Trusted admin context — no user/project filtering.
    fn ctx() -> RequestContext {
        RequestContext::new(conduit_db::PolicyContext::new(conduit_db::Principal::test()))
    }
}

#[async_trait]
impl RequestQueryServices for RequestAdapter {
    async fn requests(
        &self,
        args: RequestConnectionArgs,
    ) -> Result<RequestConnection, RequestQueryError> {
        let ctx = Self::ctx();

        // Build repo query from GraphQL args. For now, use a simple query
        // that loads all rows (bounded materialization). Filter/pagination
        // happens in-memory.
        let query = RequestListQuery {
            project_id: String::new(), // Admin sees all
            api_key_id: None,
            channel_id: None,
            model_id: None,
            source: None,
            status: None,
            start_at: None,
            end_at: None,
            limit: 1000, // Admin page load
            offset: 0,
        };

        let result: RequestListResult = self
            .repo
            .list_requests_unchecked(&ctx, &query)
            .await
            .map_err(|e| RequestQueryError::Query(e.to_string()))?;

        // Convert rows to GraphQL nodes
        let mut nodes: Vec<Request> = result.rows.into_iter().map(request_row_to_gql).collect();
        if let Some(filter) = args.where_filter.as_ref() {
            nodes.retain(|request| request_matches_filter(request, filter));
        }

        // Sort by the requested order (default: CREATED_AT DESC)
        let order = args.order_by.unwrap_or(RequestOrderSelection {
            direction: conduit_admin_graphql::request_usage::OrderDirection::Desc,
            term: RequestOrderTerm::Id,
        });
        sort_requests(&mut nodes, &order);

        // Apply forward pagination (after/first)
        let (edges, total_count, has_next_page, has_previous_page) =
            paginate_requests(nodes, args.first, args.after, args.last, args.before);
        let start_cursor = edges
            .first()
            .and_then(|edge| edge.as_ref())
            .map(|edge| edge.cursor.clone());
        let end_cursor = edges
            .last()
            .and_then(|edge| edge.as_ref())
            .map(|edge| edge.cursor.clone());

        Ok(RequestConnection {
            edges: Some(edges),
            page_info: PageInfo {
                has_next_page,
                has_previous_page,
                start_cursor,
                end_cursor,
            },
            total_count,
        })
    }
}

fn request_matches_filter(
    request: &Request,
    filter: &conduit_admin_graphql::request_usage::RequestWhereInput,
) -> bool {
    if filter.id.as_ref().is_some_and(|id| id != &request.id)
        || filter
            .project_id
            .as_ref()
            .is_some_and(|id| id != &request.project_id)
        || filter
            .trace_id
            .as_ref()
            .is_some_and(|id| request.trace_id.as_ref() != Some(id))
        || filter
            .api_key_id
            .as_ref()
            .is_some_and(|id| request.api_key_id.as_ref() != Some(id))
        || filter
            .channel_id
            .as_ref()
            .is_some_and(|id| request.channel_id.as_ref() != Some(id))
        || filter
            .model_id
            .as_ref()
            .is_some_and(|id| id != &request.model_id)
        || filter.status.is_some_and(|status| status != request.status)
        || filter.source.is_some_and(|source| source != request.source)
    {
        return false;
    }
    if filter.trace_id_is_nil == Some(true) && request.trace_id.is_some()
        || filter.trace_id_not_nil == Some(true) && request.trace_id.is_none()
        || filter.api_key_id_is_nil == Some(true) && request.api_key_id.is_some()
        || filter.api_key_id_not_nil == Some(true) && request.api_key_id.is_none()
        || filter.channel_id_is_nil == Some(true) && request.channel_id.is_some()
        || filter.channel_id_not_nil == Some(true) && request.channel_id.is_none()
    {
        return false;
    }
    true
}

/// Convert a [`RequestRow`] to the GraphQL [`Request`] shape.
pub(crate) fn request_row_to_gql(row: RequestRow) -> Request {
    // Parse content_storage_id as i64 (stored as string in DB, GraphQL expects i64)
    let content_storage_id: Option<i64> = row.content_storage_id.and_then(|s| s.parse().ok());

    Request {
        id: format!("gid://conduit/Request/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        api_key_id: row
            .api_key_id
            .map(|id| format!("gid://conduit/APIKey/{id}").into()),
        project_id: format!("gid://conduit/Project/{}", row.project_id).into(),
        trace_id: row
            .trace_id
            .map(|id| format!("gid://conduit/Trace/{id}").into()),
        data_storage_id: row
            .data_storage_id
            .map(|id| format!("gid://conduit/DataStorage/{id}").into()),
        source: request_source_from_str(&row.source),
        model_id: row.model_id,
        reasoning_effort: row.reasoning_effort,
        format: row.format,
        request_headers: row.request_headers.map(JsonRawMessageScalar),
        request_body: JsonRawMessageScalar(row.request_body),
        response_body: row.response_body.map(JsonRawMessageScalar),
        response_chunks: row.response_chunks.and_then(|chunks| {
            chunks
                .as_array()
                .map(|items| items.iter().cloned().map(JsonRawMessageScalar).collect())
        }),
        channel_id: row
            .channel_id
            .map(|id| format!("gid://conduit/Channel/{id}").into()),
        external_id: row.external_id,
        status: request_status_from_str(&row.status),
        stream: row.stream,
        client_ip: row.client_ip,
        metrics_latency_ms: row.metrics_latency_ms,
        metrics_first_token_latency_ms: row.metrics_first_token_latency_ms,
        metrics_reasoning_duration_ms: row.metrics_reasoning_duration_ms,
        content_saved: row.content_saved,
        content_storage_id,
        content_storage_key: row.content_storage_key,
        content_saved_at: row.content_saved_at.map(TimeScalar),
    }
}

fn request_source_from_str(s: &str) -> RequestSource {
    match s {
        "api" => RequestSource::Api,
        "playground" => RequestSource::Playground,
        "test" => RequestSource::Test,
        _ => RequestSource::Api,
    }
}

fn request_status_from_str(s: &str) -> RequestStatus {
    match s {
        "pending" => RequestStatus::Pending,
        "processing" => RequestStatus::Processing,
        "completed" => RequestStatus::Completed,
        "failed" => RequestStatus::Failed,
        "canceled" => RequestStatus::Canceled,
        _ => RequestStatus::Pending,
    }
}

fn sort_requests(nodes: &mut [Request], order: &RequestOrderSelection) {
    use RequestOrderTerm::*;
    use conduit_admin_graphql::request_usage::OrderDirection;
    match order.term {
        Id => match order.direction {
            OrderDirection::Asc => nodes.sort_by_key(|r| r.created_at.0),
            OrderDirection::Desc => nodes.sort_by_key(|r| std::cmp::Reverse(r.created_at.0)),
        },
        UpdatedAt => match order.direction {
            OrderDirection::Asc => nodes.sort_by_key(|r| r.updated_at.0),
            OrderDirection::Desc => nodes.sort_by_key(|r| std::cmp::Reverse(r.updated_at.0)),
        },
    }
}

fn paginate_requests(
    mut nodes: Vec<Request>,
    first: Option<i32>,
    after: Option<String>,
    last: Option<i32>,
    before: Option<String>,
) -> (Vec<Option<RequestEdge>>, i64, bool, bool) {
    let total = nodes.len() as i64;
    let (start_idx, end_idx) = pagination_window(nodes.len(), first, after, last, before);
    let page: Vec<_> = nodes.drain(start_idx..end_idx).collect();

    let edges: Vec<_> = page
        .into_iter()
        .enumerate()
        .map(|(i, node)| {
            Some(RequestEdge {
                node: Some(node),
                cursor: CursorScalar(format!("{}", start_idx + i)),
            })
        })
        .collect();

    (edges, total, end_idx < total as usize, start_idx > 0)
}

// ---------------------------------------------------------------------------
// UsageLog adapter
// ---------------------------------------------------------------------------

/// GraphQL-facing [`UsageLogQueryServices`] adapter backed by the live
/// [`conduit_db::repo::usage_repo::UsageRepo`].
pub struct UsageLogAdapter {
    repo: Arc<dyn UsageRepo>,
}

impl UsageLogAdapter {
    pub fn new(repo: Arc<dyn UsageRepo>) -> Self {
        Self { repo }
    }

    fn ctx() -> RequestContext {
        RequestContext::new(conduit_db::PolicyContext::new(conduit_db::Principal::test()))
    }
}

#[async_trait]
impl UsageLogQueryServices for UsageLogAdapter {
    async fn usage_logs(
        &self,
        args: UsageLogConnectionArgs,
    ) -> Result<UsageLogConnection, UsageLogQueryError> {
        let ctx = Self::ctx();

        let query = UsageListQuery {
            project_id: String::new(),
            api_key_id: None,
            channel_id: None,
            model_id: None,
            source: None,
            request_id: None,
            start_at: None,
            end_at: None,
            limit: 1000,
            offset: 0,
        };

        let result: UsageListResult = self
            .repo
            .list_usage_unchecked(&ctx, &query)
            .await
            .map_err(|e| UsageLogQueryError::Query(e.to_string()))?;

        let mut nodes: Vec<UsageLog> = result.rows.into_iter().map(usage_log_row_to_gql).collect();
        if let Some(filter) = args.where_filter.as_ref() {
            nodes.retain(|usage| usage_log_matches_filter(usage, filter));
        }

        let order = args.order_by.unwrap_or(UsageLogOrderSelection {
            direction: conduit_admin_graphql::request_usage::OrderDirection::Desc,
            term: UsageLogOrderTerm::Id,
        });
        sort_usage_logs(&mut nodes, &order);

        let (edges, total_count, has_next_page, has_previous_page) =
            paginate_usage_logs(nodes, args.first, args.after, args.last, args.before);
        let start_cursor = edges
            .first()
            .and_then(|edge| edge.as_ref())
            .map(|edge| edge.cursor.clone());
        let end_cursor = edges
            .last()
            .and_then(|edge| edge.as_ref())
            .map(|edge| edge.cursor.clone());

        Ok(UsageLogConnection {
            edges: Some(edges),
            page_info: PageInfo {
                has_next_page,
                has_previous_page,
                start_cursor,
                end_cursor,
            },
            total_count,
        })
    }
}

fn usage_log_matches_filter(
    usage: &UsageLog,
    filter: &conduit_admin_graphql::request_usage::UsageLogWhereInput,
) -> bool {
    !(filter.id.as_ref().is_some_and(|id| id != &usage.id)
        || filter
            .request_id
            .as_ref()
            .is_some_and(|id| id != &usage.request_id)
        || filter
            .project_id
            .as_ref()
            .is_some_and(|id| id != &usage.project_id)
        || filter
            .channel_id
            .as_ref()
            .is_some_and(|id| usage.channel_id.as_ref() != Some(id))
        || filter
            .model_id
            .as_ref()
            .is_some_and(|id| id != &usage.model_id))
}

pub(crate) fn usage_log_row_to_gql(row: UsageLogRow) -> UsageLog {
    // Parse api_key_id as i64 (it's stored as string in DB but GraphQL expects i64)
    let api_key_id: Option<i64> = row.api_key_id.and_then(|s| s.parse().ok());

    UsageLog {
        id: format!("gid://conduit/UsageLog/{}", row.id).into(),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        request_id: format!("gid://conduit/Request/{}", row.request_id).into(),
        api_key_id,
        project_id: format!("gid://conduit/Project/{}", row.project_id).into(),
        channel_id: row
            .channel_id
            .map(|id| format!("gid://conduit/Channel/{id}").into()),
        model_id: row.model_id,
        prompt_tokens: row.prompt_tokens,
        completion_tokens: row.completion_tokens,
        total_tokens: row.total_tokens,
        prompt_audio_tokens: if row.prompt_audio_tokens > 0 {
            Some(row.prompt_audio_tokens)
        } else {
            None
        },
        prompt_cached_tokens: if row.prompt_cached_tokens > 0 {
            Some(row.prompt_cached_tokens)
        } else {
            None
        },
        prompt_write_cached_tokens: if row.prompt_write_cached_tokens > 0 {
            Some(row.prompt_write_cached_tokens)
        } else {
            None
        },
        prompt_write_cached_tokens_5m: if row.prompt_write_cached_tokens_5m > 0 {
            Some(row.prompt_write_cached_tokens_5m)
        } else {
            None
        },
        prompt_write_cached_tokens_1h: if row.prompt_write_cached_tokens_1h > 0 {
            Some(row.prompt_write_cached_tokens_1h)
        } else {
            None
        },
        completion_audio_tokens: if row.completion_audio_tokens > 0 {
            Some(row.completion_audio_tokens)
        } else {
            None
        },
        completion_reasoning_tokens: if row.completion_reasoning_tokens > 0 {
            Some(row.completion_reasoning_tokens)
        } else {
            None
        },
        completion_accepted_prediction_tokens: if row.completion_accepted_prediction_tokens > 0 {
            Some(row.completion_accepted_prediction_tokens)
        } else {
            None
        },
        completion_rejected_prediction_tokens: if row.completion_rejected_prediction_tokens > 0 {
            Some(row.completion_rejected_prediction_tokens)
        } else {
            None
        },
        source: usage_log_source_from_str(&row.source),
        format: row.format,
        total_cost: row.total_cost,
        cost_items: cost_items_from_json(row.cost_items),
        cost_price_reference_id: row.cost_price_reference_id,
    }
}

fn usage_log_source_from_str(s: &str) -> conduit_admin_graphql::request_usage::UsageLogSource {
    match s {
        "api" => conduit_admin_graphql::request_usage::UsageLogSource::Api,
        "playground" => conduit_admin_graphql::request_usage::UsageLogSource::Playground,
        "test" => conduit_admin_graphql::request_usage::UsageLogSource::Test,
        _ => conduit_admin_graphql::request_usage::UsageLogSource::Api,
    }
}

fn sort_usage_logs(nodes: &mut [UsageLog], order: &UsageLogOrderSelection) {
    use UsageLogOrderTerm::*;
    use conduit_admin_graphql::request_usage::OrderDirection;
    match order.term {
        Id => match order.direction {
            OrderDirection::Asc => nodes.sort_by_key(|r| r.created_at.0),
            OrderDirection::Desc => nodes.sort_by_key(|r| std::cmp::Reverse(r.created_at.0)),
        },
        UpdatedAt => match order.direction {
            OrderDirection::Asc => nodes.sort_by_key(|r| r.updated_at.0),
            OrderDirection::Desc => nodes.sort_by_key(|r| std::cmp::Reverse(r.updated_at.0)),
        },
    }
}

fn paginate_usage_logs(
    mut nodes: Vec<UsageLog>,
    first: Option<i32>,
    after: Option<String>,
    last: Option<i32>,
    before: Option<String>,
) -> (Vec<Option<UsageLogEdge>>, i64, bool, bool) {
    let total = nodes.len() as i64;
    let (start_idx, end_idx) = pagination_window(nodes.len(), first, after, last, before);
    let page: Vec<_> = nodes.drain(start_idx..end_idx).collect();

    let edges: Vec<_> = page
        .into_iter()
        .enumerate()
        .map(|(i, node)| {
            Some(UsageLogEdge {
                node: Some(node),
                cursor: CursorScalar(format!("{}", start_idx + i)),
            })
        })
        .collect();

    (edges, total, end_idx < total as usize, start_idx > 0)
}

fn pagination_window(
    len: usize,
    first: Option<i32>,
    after: Option<String>,
    last: Option<i32>,
    before: Option<String>,
) -> (usize, usize) {
    let lower = after
        .as_deref()
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .map_or(0, |index| index.saturating_add(1).min(len));
    let upper = before
        .as_deref()
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .unwrap_or(len)
        .min(len)
        .max(lower);

    if let Some(last) = last {
        let limit = last.max(0).min(100) as usize;
        (upper.saturating_sub(limit).max(lower), upper)
    } else {
        let limit = first.unwrap_or(20).max(0).min(100) as usize;
        (lower, lower.saturating_add(limit).min(upper))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredCostItem {
    item_code: String,
    #[serde(default)]
    quantity: i64,
    #[serde(default)]
    subtotal: Decimal,
    #[serde(default)]
    tier_breakdown: Vec<StoredTierCost>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredTierCost {
    up_to: Option<i64>,
    #[serde(default)]
    units: i64,
    #[serde(default)]
    subtotal: Decimal,
}

fn cost_items_from_json(value: serde_json::Value) -> Option<Vec<CostItem>> {
    let stored: Vec<StoredCostItem> = serde_json::from_value(value).ok()?;
    Some(
        stored
            .into_iter()
            .filter_map(|item| {
                let item_code = match item.item_code.as_str() {
                    "prompt_tokens" => PriceItemCode::PromptTokens,
                    "completion_tokens" => PriceItemCode::CompletionTokens,
                    "prompt_cached_tokens" => PriceItemCode::PromptCachedTokens,
                    "prompt_write_cached_tokens" => PriceItemCode::PromptWriteCachedTokens,
                    _ => return None,
                };
                Some(CostItem {
                    item_code,
                    quantity: item.quantity,
                    tier_breakdown: (!item.tier_breakdown.is_empty()).then(|| {
                        item.tier_breakdown
                            .into_iter()
                            .map(|tier| TierCost {
                                up_to: tier.up_to,
                                units: tier.units,
                                subtotal: DecimalScalar(tier.subtotal),
                            })
                            .collect()
                    }),
                    subtotal: DecimalScalar(item.subtotal),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_source_parsing() {
        assert_eq!(request_source_from_str("api"), RequestSource::Api);
        assert_eq!(
            request_source_from_str("playground"),
            RequestSource::Playground
        );
        assert_eq!(request_source_from_str("test"), RequestSource::Test);
    }

    #[test]
    fn request_status_parsing() {
        assert_eq!(request_status_from_str("pending"), RequestStatus::Pending);
        assert_eq!(
            request_status_from_str("completed"),
            RequestStatus::Completed
        );
    }

    #[test]
    fn relay_window_honors_forward_and_backward_cursors() {
        assert_eq!(
            pagination_window(10, Some(3), Some("2".into()), None, None),
            (3, 6)
        );
        assert_eq!(
            pagination_window(10, None, None, Some(2), Some("7".into())),
            (5, 7)
        );
    }

    #[test]
    fn cost_items_are_materialized_for_graphql() -> Result<(), Box<dyn std::error::Error>> {
        let items = cost_items_from_json(serde_json::json!([{
            "itemCode": "prompt_tokens",
            "quantity": 12,
            "subtotal": "0.25",
            "tierBreakdown": [{"upTo": 100, "units": 12, "subtotal": "0.25"}]
        }]))
        .ok_or("valid stored cost items")?;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_code, PriceItemCode::PromptTokens);
        assert_eq!(items[0].quantity, 12);
        assert_eq!(items[0].subtotal.0.to_string(), "0.25");
        Ok(())
    }
}
