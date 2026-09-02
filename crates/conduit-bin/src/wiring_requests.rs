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
use conduit_admin_graphql::policy::AdminAccessScope;
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
use conduit_db::repo::request_repo::{
    RequestListOrderField, RequestListQuery, RequestListResult, RequestRepo,
};
use conduit_db::repo::usage_repo::{
    UsageListOrderField, UsageListQuery, UsageListResult, UsageRepo,
};
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

    /// Internal repository context. Caller visibility is already represented
    /// by the authorized `AdminAccessScope` passed to this adapter.
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
        let order = args.order_by.unwrap_or(RequestOrderSelection {
            direction: conduit_admin_graphql::request_usage::OrderDirection::Desc,
            term: RequestOrderTerm::Id,
        });
        let mut query = request_list_query(&args.access, args.where_filter.as_ref(), order)?;
        if project_filter_conflicts(
            &args.access,
            args.where_filter
                .as_ref()
                .and_then(|f| f.project_id.as_ref()),
        ) {
            return Ok(empty_request_connection());
        }
        let total_count = self
            .repo
            .count_requests_unchecked(&ctx, &query)
            .await
            .map_err(|e| RequestQueryError::Query(e.to_string()))?;
        let total_len = usize::try_from(total_count).unwrap_or(usize::MAX);
        let (start_idx, end_idx) =
            pagination_window(total_len, args.first, args.after, args.last, args.before);
        query.offset = u32::try_from(start_idx).unwrap_or(u32::MAX);
        query.limit = u32::try_from(end_idx.saturating_sub(start_idx)).unwrap_or(u32::MAX);
        let result: RequestListResult = self
            .repo
            .list_requests_unchecked(&ctx, &query)
            .await
            .map_err(|e| RequestQueryError::Query(e.to_string()))?;
        let edges: Vec<_> = result
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                Some(RequestEdge {
                    node: Some(request_row_to_gql(row)),
                    cursor: CursorScalar((start_idx + index).to_string()),
                })
            })
            .collect();
        let has_next_page = end_idx < total_len;
        let has_previous_page = start_idx > 0;
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
            total_count: i64::try_from(total_count).unwrap_or(i64::MAX),
        })
    }
}

fn database_id(value: &str, expected_type: &str) -> String {
    conduit_admin_graphql::node::parse_guid(value)
        .ok()
        .filter(|guid| guid.typ == expected_type)
        .map_or_else(|| value.to_owned(), |guid| guid.id.to_string())
}

fn project_filter_conflicts(
    access: &AdminAccessScope,
    requested: Option<&async_graphql::ID>,
) -> bool {
    match (access, requested) {
        (AdminAccessScope::Project(project_id), Some(requested)) => {
            database_id(project_id, "Project") != database_id(requested.as_str(), "Project")
        }
        _ => false,
    }
}

fn nil_filter(is_nil: Option<bool>, not_nil: Option<bool>) -> Option<bool> {
    if is_nil == Some(true) {
        Some(true)
    } else if not_nil == Some(true) {
        Some(false)
    } else {
        None
    }
}

fn request_list_query(
    access: &AdminAccessScope,
    filter: Option<&conduit_admin_graphql::request_usage::RequestWhereInput>,
    order: RequestOrderSelection,
) -> Result<RequestListQuery, RequestQueryError> {
    let filter = filter.cloned().unwrap_or_default();
    let project_id = match access {
        AdminAccessScope::Project(project_id) => database_id(project_id, "Project"),
        AdminAccessScope::Global => filter
            .project_id
            .as_ref()
            .map(|id| database_id(id.as_str(), "Project"))
            .unwrap_or_default(),
    };
    Ok(RequestListQuery {
        project_id,
        id: filter
            .id
            .as_ref()
            .map(|id| database_id(id.as_str(), "Request")),
        api_key_id: filter
            .api_key_id
            .as_ref()
            .map(|id| database_id(id.as_str(), "APIKey")),
        channel_id: filter
            .channel_id
            .as_ref()
            .map(|id| database_id(id.as_str(), "Channel")),
        trace_id: filter
            .trace_id
            .as_ref()
            .map(|id| database_id(id.as_str(), "Trace")),
        trace_id_is_nil: nil_filter(filter.trace_id_is_nil, filter.trace_id_not_nil),
        api_key_id_is_nil: nil_filter(filter.api_key_id_is_nil, filter.api_key_id_not_nil),
        channel_id_is_nil: nil_filter(filter.channel_id_is_nil, filter.channel_id_not_nil),
        model_id: filter.model_id,
        source: filter.source.map(|source| match source {
            RequestSource::Api => "api".to_string(),
            RequestSource::Playground => "playground".to_string(),
            RequestSource::Test => "test".to_string(),
        }),
        status: filter.status.map(|status| match status {
            RequestStatus::Pending => "pending".to_string(),
            RequestStatus::Processing => "processing".to_string(),
            RequestStatus::Completed => "completed".to_string(),
            RequestStatus::Failed => "failed".to_string(),
            RequestStatus::Canceled => "canceled".to_string(),
        }),
        start_at: None,
        end_at: None,
        limit: 0,
        offset: 0,
        order_field: match order.term {
            RequestOrderTerm::Id => RequestListOrderField::Id,
            RequestOrderTerm::UpdatedAt => RequestListOrderField::UpdatedAt,
        },
        descending: matches!(
            order.direction,
            conduit_admin_graphql::request_usage::OrderDirection::Desc
        ),
    })
}

fn empty_request_connection() -> RequestConnection {
    RequestConnection {
        edges: Some(Vec::new()),
        page_info: PageInfo {
            has_next_page: false,
            has_previous_page: false,
            start_cursor: None,
            end_cursor: None,
        },
        total_count: 0,
    }
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
        let order = args.order_by.unwrap_or(UsageLogOrderSelection {
            direction: conduit_admin_graphql::request_usage::OrderDirection::Desc,
            term: UsageLogOrderTerm::Id,
        });
        let mut query = usage_list_query(&args.access, args.where_filter.as_ref(), order);
        if project_filter_conflicts(
            &args.access,
            args.where_filter
                .as_ref()
                .and_then(|f| f.project_id.as_ref()),
        ) {
            return Ok(empty_usage_connection());
        }
        let total_count = self
            .repo
            .count_usage_unchecked(&ctx, &query)
            .await
            .map_err(|e| UsageLogQueryError::Query(e.to_string()))?;
        let total_len = usize::try_from(total_count).unwrap_or(usize::MAX);
        let (start_idx, end_idx) =
            pagination_window(total_len, args.first, args.after, args.last, args.before);
        query.offset = u32::try_from(start_idx).unwrap_or(u32::MAX);
        query.limit = u32::try_from(end_idx.saturating_sub(start_idx)).unwrap_or(u32::MAX);
        let result: UsageListResult = self
            .repo
            .list_usage_unchecked(&ctx, &query)
            .await
            .map_err(|e| UsageLogQueryError::Query(e.to_string()))?;
        let edges: Vec<_> = result
            .rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                Some(UsageLogEdge {
                    node: Some(usage_log_row_to_gql(row)),
                    cursor: CursorScalar((start_idx + index).to_string()),
                })
            })
            .collect();
        let has_next_page = end_idx < total_len;
        let has_previous_page = start_idx > 0;
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
            total_count: i64::try_from(total_count).unwrap_or(i64::MAX),
        })
    }
}

fn usage_list_query(
    access: &AdminAccessScope,
    filter: Option<&conduit_admin_graphql::request_usage::UsageLogWhereInput>,
    order: UsageLogOrderSelection,
) -> UsageListQuery {
    let filter = filter.cloned().unwrap_or_default();
    let project_id = match access {
        AdminAccessScope::Project(project_id) => database_id(project_id, "Project"),
        AdminAccessScope::Global => filter
            .project_id
            .as_ref()
            .map(|id| database_id(id.as_str(), "Project"))
            .unwrap_or_default(),
    };
    UsageListQuery {
        project_id,
        id: filter
            .id
            .as_ref()
            .map(|id| database_id(id.as_str(), "UsageLog")),
        api_key_id: None,
        channel_id: filter
            .channel_id
            .as_ref()
            .map(|id| database_id(id.as_str(), "Channel")),
        model_id: filter.model_id.clone(),
        source: None,
        request_id: filter
            .request_id
            .as_ref()
            .map(|id| database_id(id.as_str(), "Request")),
        start_at: None,
        end_at: None,
        limit: 0,
        offset: 0,
        order_field: match order.term {
            UsageLogOrderTerm::Id => UsageListOrderField::Id,
            UsageLogOrderTerm::UpdatedAt => UsageListOrderField::UpdatedAt,
        },
        descending: matches!(
            order.direction,
            conduit_admin_graphql::request_usage::OrderDirection::Desc
        ),
    }
}

fn empty_usage_connection() -> UsageLogConnection {
    UsageLogConnection {
        edges: Some(Vec::new()),
        page_info: PageInfo {
            has_next_page: false,
            has_previous_page: false,
            start_cursor: None,
            end_cursor: None,
        },
        total_count: 0,
    }
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
