//! GAP-E — Thread + Trace GraphQL query slice.
//!
//! Bounded scope: the two ent-connection root queries the frontend threads and
//! traces pages depend on for first paint —
//!
//!   - `Query.threads(after, first, before, last, orderBy, where): ThreadConnection!`
//!     — Go resolver `queryResolver.Threads`
//!     (`conduit/internal/server/gql/ent.resolvers.go:486-500`): validate the
//!     pagination args, remap a `CREATED_AT` ordering request to
//!     `ent.DefaultThreadOrder` (order by ID) preserving direction, then
//!     delegate to ent `Thread.Query().Paginate(...)`.
//!   - `Query.traces(after, first, before, last, orderBy, where): TraceConnection!`
//!     — Go resolver `queryResolver.Traces` (`ent.resolvers.go:502-516`): same
//!     shape over `Trace.Query()`.
//!
//! Every GraphQL type/input these queries reference is copied field-for-field
//! from the captured contract snapshot
//! `tests/contracts/admin_graphql_schema.graphql`:
//!
//!   - `type Thread implements Node` (snapshot line 6935) — scalar/self-domain
//!     fields only; the cross-domain edge fields `project: Project!` and
//!     `traces(…): TraceConnection!` are pending (see below).
//!   - `type ThreadConnection` / `type ThreadEdge` (lines 6983 / 7000).
//!   - `input ThreadOrder` (line 7013) + `enum ThreadOrderField` (line 7026).
//!   - `input ThreadWhereInput` (line 7034, ent-generated).
//!   - `type Trace implements Node` (snapshot line 7109) — scalar/self-domain
//!     fields only; the cross-domain edge fields `project: Project!`,
//!     `thread: Thread` and `requests(…): RequestConnection!` are pending.
//!   - `type TraceConnection` / `type TraceEdge` (lines 7162 / 7179).
//!   - `input TraceOrder` (line 7192) + `enum TraceOrderField` (line 7205).
//!   - `input TraceWhereInput` (line 7213, ent-generated).
//!
//! `Thread` and `Trace` join the Relay `Node` interface (declared in
//! `crate::channel`) so `implements Node` renders exactly as the contract does.
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//! Cross-domain edge fields + their `has<Edge>With` filters reference other
//! entities' `*WhereInput` / `*Connection` types and belong to other slices:
//!
//!   - `Thread.project: Project!`, `Thread.traces(…): TraceConnection!`.
//!   - `Trace.project: Project!`, `Trace.thread: Thread`,
//!     `Trace.requests(…): RequestConnection!`.
//!   - `ThreadWhereInput.hasProjectWith: [ProjectWhereInput!]`,
//!     `hasTracesWith: [TraceWhereInput!]`.
//!   - `TraceWhereInput.hasProjectWith: [ProjectWhereInput!]`,
//!     `hasThreadWith: [ThreadWhereInput!]`,
//!     `hasRequestsWith: [RequestWhereInput!]`.
//!
//! The `has<Edge>: Boolean` existence predicates ARE implemented (they carry no
//! cross-domain type reference).

use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, InputObject, SimpleObject};

use crate::channel::OrderDirection;
use crate::pagination::PageInfo;
use crate::scalars::{CursorScalar, DecimalScalar, JsonRawMessageScalar, TimeScalar};

// ---------------------------------------------------------------------------
// Enums (snapshot-exact value spellings)
// ---------------------------------------------------------------------------

/// `enum ThreadOrderField { CREATED_AT UPDATED_AT }` — snapshot lines
/// 7026-7029.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ThreadOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

/// `enum TraceOrderField { CREATED_AT UPDATED_AT }` — snapshot lines
/// 7205-7208.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum TraceOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

// ---------------------------------------------------------------------------
// Output object types
// ---------------------------------------------------------------------------

/// `type Thread implements Node` — snapshot lines 6935-6982, scalar and
/// self-domain fields only. Cross-domain edge fields are pending (module doc).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
pub struct Thread {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    /// Project ID that this thread belongs to (snapshot line 6941).
    #[graphql(name = "projectID")]
    pub project_id: ID,
    /// Unique thread identifier for this thread (snapshot line 6945).
    #[graphql(name = "threadID")]
    pub thread_id: String,
}

/// `type ThreadEdge { node: Thread cursor: Cursor! }` — snapshot line 7000.
/// `node` is nullable in the contract (ent emits nullable edge nodes).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ThreadEdge {
    pub node: Option<Thread>,
    pub cursor: CursorScalar,
}

/// `type ThreadConnection` — snapshot line 6983. `edges` is a nullable list of
/// nullable edges (`[ThreadEdge]`), exactly as ent generates it.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ThreadConnection {
    pub edges: Option<Vec<Option<ThreadEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

/// `type Trace implements Node` — snapshot lines 7109-7161, scalar and
/// self-domain fields only. Cross-domain edge fields are pending (module doc).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
pub struct Trace {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    /// Project ID that this trace belongs to (snapshot line 7115).
    #[graphql(name = "projectID")]
    pub project_id: ID,
    /// Unique trace identifier (snapshot line 7119).
    #[graphql(name = "traceID")]
    pub trace_id: String,
    /// Thread ID that this trace belongs to — nullable (snapshot line 7125).
    #[graphql(name = "threadID")]
    pub thread_id: Option<ID>,
}

#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct UsageMetadata {
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub total_cached_tokens: i64,
    pub total_cached_write_tokens: i64,
    pub total_cost: DecimalScalar,
}

impl Default for UsageMetadata {
    fn default() -> Self {
        Self {
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            total_cached_write_tokens: 0,
            total_cost: DecimalScalar(rust_decimal::Decimal::ZERO),
        }
    }
}

fn text_from_content(content: &serde_json::Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    content.as_array()?.iter().find_map(|part| {
        part.get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
}

fn first_user_query_from_body(body: &serde_json::Value) -> Option<String> {
    for key in ["messages", "input", "contents"] {
        if let Some(items) = body.get(key).and_then(serde_json::Value::as_array) {
            for item in items {
                let role = item
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if role == "user" {
                    if let Some(text) = item.get("content").and_then(text_from_content) {
                        return Some(text);
                    }
                    if let Some(parts) = item.get("parts").and_then(text_from_content) {
                        return Some(parts);
                    }
                }
            }
        }
    }
    body.get("prompt")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

async fn first_trace_request(
    trace: &Trace,
    ctx: &Context<'_>,
) -> Result<Option<crate::request_usage::Request>, String> {
    let services = crate::request_usage::request_query_services(ctx)?;
    let conn = services
        .requests(crate::request_usage::RequestConnectionArgs {
            access: crate::request_usage::request_read_access_scope(ctx)?,
            first: Some(1),
            order_by: Some(crate::request_usage::RequestOrderSelection {
                direction: crate::request_usage::OrderDirection::Asc,
                term: crate::request_usage::RequestOrderTerm::Id,
            }),
            where_filter: Some(crate::request_usage::RequestWhereInput {
                trace_id: Some(trace.id.clone()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .map_err(|err| err.to_string())?;
    Ok(conn
        .edges
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .next()
        .and_then(|edge| edge.node))
}

async fn trace_requests(
    trace: &Trace,
    ctx: &Context<'_>,
) -> Result<Vec<crate::request_usage::Request>, String> {
    let services = crate::request_usage::request_query_services(ctx)?;
    let conn = services
        .requests(crate::request_usage::RequestConnectionArgs {
            access: crate::request_usage::request_read_access_scope(ctx)?,
            order_by: Some(crate::request_usage::RequestOrderSelection {
                direction: crate::request_usage::OrderDirection::Asc,
                term: crate::request_usage::RequestOrderTerm::Id,
            }),
            where_filter: Some(crate::request_usage::RequestWhereInput {
                trace_id: Some(trace.id.clone()),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .map_err(|err| err.to_string())?;
    Ok(conn
        .edges
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .filter_map(|edge| edge.node)
        .collect())
}

async fn usage_metadata_for_requests(
    requests: &[crate::request_usage::Request],
    ctx: &Context<'_>,
) -> Result<UsageMetadata, String> {
    let services = crate::request_usage::usage_log_query_services(ctx)?;
    let mut metadata = UsageMetadata::default();
    for request in requests {
        let conn = services
            .usage_logs(crate::request_usage::UsageLogConnectionArgs {
                access: crate::request_usage::request_read_access_scope(ctx)?,
                where_filter: Some(crate::request_usage::UsageLogWhereInput {
                    request_id: Some(request.id.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .map_err(|err| err.to_string())?;
        for usage in conn
            .edges
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .filter_map(|edge| edge.node)
        {
            metadata.total_input_tokens += usage.prompt_tokens;
            metadata.total_output_tokens += usage.completion_tokens;
            metadata.total_tokens += usage.total_tokens;
            metadata.total_cached_tokens += usage.prompt_cached_tokens.unwrap_or_default();
            metadata.total_cached_write_tokens +=
                usage.prompt_write_cached_tokens.unwrap_or_default();
            if let Some(cost) = usage.total_cost {
                metadata.total_cost.0 += cost
                    .to_string()
                    .parse::<rust_decimal::Decimal>()
                    .unwrap_or_default();
            }
        }
    }
    Ok(metadata)
}

fn first_text_from_response(body: &serde_json::Value) -> Option<String> {
    body.pointer("/choices/0/message/content")
        .and_then(text_from_content)
        .or_else(|| body.get("content").and_then(text_from_content))
        .or_else(|| {
            body.pointer("/candidates/0/content/parts")
                .and_then(text_from_content)
        })
        .or_else(|| {
            body.get("output_text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
}

#[ComplexObject]
impl Thread {
    async fn project(&self, ctx: &Context<'_>) -> Result<crate::project::Project, String> {
        crate::policy::authorize_current(ctx, conduit_auth::scopes::slug::READ_PROJECTS)
            .map_err(|error| error.to_string())?;
        let services = crate::project::project_query_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::READ_PROJECTS,
        )
        .map_err(|error| error.to_string())?;
        let conn = services
            .projects_with_access(
                &access,
                crate::project::ProjectConnectionArgs {
                    first: Some(1),
                    where_filter: Some(crate::project::ProjectWhereInput {
                        id: Some(self.project_id.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|err| err.to_string())?;
        conn.edges
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .next()
            .and_then(|edge| edge.node)
            .ok_or_else(|| "thread's project was not found".to_string())
    }

    #[allow(clippy::too_many_arguments)]
    async fn traces(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<TraceOrder>,
        #[graphql(name = "where")] where_filter: Option<TraceWhereInput>,
    ) -> Result<TraceConnection, String> {
        let services = trace_query_services(ctx)?;
        services
            .traces(TraceConnectionArgs {
                access: crate::request_usage::request_read_access_scope(ctx)?,
                after: after.map(|cursor| cursor.0),
                first,
                before: before.map(|cursor| cursor.0),
                last,
                order_by: resolve_trace_order(order_by),
                where_filter: Some(TraceWhereInput {
                    thread_id: Some(self.id.clone()),
                    ..where_filter.unwrap_or_default()
                }),
            })
            .await
            .map_err(|err| err.to_string())
    }

    async fn first_user_query(&self, ctx: &Context<'_>) -> Result<Option<String>, String> {
        let traces = self
            .traces(
                ctx,
                None,
                Some(1),
                None,
                None,
                Some(TraceOrder {
                    direction: OrderDirection::Asc,
                    field: TraceOrderField::CreatedAt,
                }),
                None,
            )
            .await?;
        let Some(trace) = traces
            .edges
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .next()
            .and_then(|edge| edge.node)
        else {
            return Ok(None);
        };
        trace.first_user_query(ctx).await
    }

    async fn usage_metadata(&self, ctx: &Context<'_>) -> Result<Option<UsageMetadata>, String> {
        let traces = self.traces(ctx, None, None, None, None, None, None).await?;
        let mut requests = Vec::new();
        for trace in traces
            .edges
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .filter_map(|edge| edge.node)
        {
            requests.extend(trace_requests(&trace, ctx).await?);
        }
        Ok(Some(usage_metadata_for_requests(&requests, ctx).await?))
    }
}

#[ComplexObject]
impl Trace {
    async fn project(&self, ctx: &Context<'_>) -> Result<crate::project::Project, String> {
        crate::policy::authorize_current(ctx, conduit_auth::scopes::slug::READ_PROJECTS)
            .map_err(|error| error.to_string())?;
        let services = crate::project::project_query_services(ctx)?;
        let access = crate::policy::AdminAccessScope::from_graphql_context(
            ctx,
            conduit_auth::scopes::slug::READ_PROJECTS,
        )
        .map_err(|error| error.to_string())?;
        let conn = services
            .projects_with_access(
                &access,
                crate::project::ProjectConnectionArgs {
                    first: Some(1),
                    where_filter: Some(crate::project::ProjectWhereInput {
                        id: Some(self.project_id.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|err| err.to_string())?;
        conn.edges
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .next()
            .and_then(|edge| edge.node)
            .ok_or_else(|| "trace's project was not found".to_string())
    }

    async fn thread(&self, ctx: &Context<'_>) -> Result<Option<Thread>, String> {
        let Some(id) = self.thread_id.clone() else {
            return Ok(None);
        };
        let services = thread_query_services(ctx)?;
        let conn = services
            .threads(ThreadConnectionArgs {
                access: crate::request_usage::request_read_access_scope(ctx)?,
                first: Some(1),
                where_filter: Some(ThreadWhereInput {
                    id: Some(id),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .map_err(|err| err.to_string())?;
        Ok(conn
            .edges
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .next()
            .and_then(|edge| edge.node))
    }

    #[allow(clippy::too_many_arguments)]
    async fn requests(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<crate::request_usage::RequestOrder>,
        #[graphql(name = "where")] where_filter: Option<crate::request_usage::RequestWhereInput>,
    ) -> Result<crate::request_usage::RequestConnection, String> {
        let services = crate::request_usage::request_query_services(ctx)?;
        services
            .requests(crate::request_usage::RequestConnectionArgs {
                access: crate::request_usage::request_read_access_scope(ctx)?,
                after: after.map(|cursor| cursor.0),
                first,
                before: before.map(|cursor| cursor.0),
                last,
                order_by: crate::request_usage::resolve_request_order(order_by),
                where_filter: Some(crate::request_usage::RequestWhereInput {
                    trace_id: Some(self.id.clone()),
                    ..where_filter.unwrap_or_default()
                }),
            })
            .await
            .map_err(|err| err.to_string())
    }

    async fn first_user_query(&self, ctx: &Context<'_>) -> Result<Option<String>, String> {
        Ok(first_trace_request(self, ctx)
            .await?
            .and_then(|request| first_user_query_from_body(&request.request_body.0)))
    }

    async fn first_text(&self, ctx: &Context<'_>) -> Result<Option<String>, String> {
        Ok(trace_requests(self, ctx)
            .await?
            .into_iter()
            .find_map(|request| {
                request
                    .response_body
                    .as_ref()
                    .and_then(|body| first_text_from_response(&body.0))
            }))
    }

    async fn usage_metadata(&self, ctx: &Context<'_>) -> Result<Option<UsageMetadata>, String> {
        Ok(Some(
            usage_metadata_for_requests(&trace_requests(self, ctx).await?, ctx).await?,
        ))
    }

    async fn raw_root_segment(&self) -> Option<JsonRawMessageScalar> {
        None
    }
}

/// `type TraceEdge { node: Trace cursor: Cursor! }` — snapshot line 7179.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct TraceEdge {
    pub node: Option<Trace>,
    pub cursor: CursorScalar,
}

/// `type TraceConnection` — snapshot line 7162.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct TraceConnection {
    pub edges: Option<Vec<Option<TraceEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

// ---------------------------------------------------------------------------
// Order inputs
// ---------------------------------------------------------------------------

/// `input ThreadOrder { direction: OrderDirection! = ASC field:
/// ThreadOrderField! }` — snapshot lines 7013-7022.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ThreadOrder {
    /// Defaults to ASC when omitted, matching the ent-generated contract.
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: ThreadOrderField,
}

/// `input TraceOrder { direction: OrderDirection! = ASC field:
/// TraceOrderField! }` — snapshot lines 7192-7201.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct TraceOrder {
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: TraceOrderField,
}

// ---------------------------------------------------------------------------
// Where inputs (ent-generated predicate grammar)
// ---------------------------------------------------------------------------

/// `input ThreadWhereInput` — snapshot lines 7034-7104. Implemented:
/// `not`/`and`/`or`, every scalar-field predicate family, and the
/// `has<Edge>: Boolean` existence predicates. The `has<Edge>With` fields are
/// pending (module doc).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct ThreadWhereInput {
    pub not: Option<Box<ThreadWhereInput>>,
    pub and: Option<Vec<ThreadWhereInput>>,
    pub or: Option<Vec<ThreadWhereInput>>,
    // id field predicates
    pub id: Option<ID>,
    #[graphql(name = "idNEQ")]
    pub id_neq: Option<ID>,
    pub id_in: Option<Vec<ID>>,
    pub id_not_in: Option<Vec<ID>>,
    #[graphql(name = "idGT")]
    pub id_gt: Option<ID>,
    #[graphql(name = "idGTE")]
    pub id_gte: Option<ID>,
    #[graphql(name = "idLT")]
    pub id_lt: Option<ID>,
    #[graphql(name = "idLTE")]
    pub id_lte: Option<ID>,
    // created_at field predicates
    pub created_at: Option<TimeScalar>,
    #[graphql(name = "createdAtNEQ")]
    pub created_at_neq: Option<TimeScalar>,
    pub created_at_in: Option<Vec<TimeScalar>>,
    pub created_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "createdAtGT")]
    pub created_at_gt: Option<TimeScalar>,
    #[graphql(name = "createdAtGTE")]
    pub created_at_gte: Option<TimeScalar>,
    #[graphql(name = "createdAtLT")]
    pub created_at_lt: Option<TimeScalar>,
    #[graphql(name = "createdAtLTE")]
    pub created_at_lte: Option<TimeScalar>,
    // updated_at field predicates
    pub updated_at: Option<TimeScalar>,
    #[graphql(name = "updatedAtNEQ")]
    pub updated_at_neq: Option<TimeScalar>,
    pub updated_at_in: Option<Vec<TimeScalar>>,
    pub updated_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "updatedAtGT")]
    pub updated_at_gt: Option<TimeScalar>,
    #[graphql(name = "updatedAtGTE")]
    pub updated_at_gte: Option<TimeScalar>,
    #[graphql(name = "updatedAtLT")]
    pub updated_at_lt: Option<TimeScalar>,
    #[graphql(name = "updatedAtLTE")]
    pub updated_at_lte: Option<TimeScalar>,
    // project_id field predicates (reference id → eq/neq/in/notIn only)
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    #[graphql(name = "projectIDNEQ")]
    pub project_id_neq: Option<ID>,
    #[graphql(name = "projectIDIn")]
    pub project_id_in: Option<Vec<ID>>,
    #[graphql(name = "projectIDNotIn")]
    pub project_id_not_in: Option<Vec<ID>>,
    // thread_id field predicates (String → full family)
    #[graphql(name = "threadID")]
    pub thread_id: Option<String>,
    #[graphql(name = "threadIDNEQ")]
    pub thread_id_neq: Option<String>,
    #[graphql(name = "threadIDIn")]
    pub thread_id_in: Option<Vec<String>>,
    #[graphql(name = "threadIDNotIn")]
    pub thread_id_not_in: Option<Vec<String>>,
    #[graphql(name = "threadIDGT")]
    pub thread_id_gt: Option<String>,
    #[graphql(name = "threadIDGTE")]
    pub thread_id_gte: Option<String>,
    #[graphql(name = "threadIDLT")]
    pub thread_id_lt: Option<String>,
    #[graphql(name = "threadIDLTE")]
    pub thread_id_lte: Option<String>,
    #[graphql(name = "threadIDContains")]
    pub thread_id_contains: Option<String>,
    #[graphql(name = "threadIDHasPrefix")]
    pub thread_id_has_prefix: Option<String>,
    #[graphql(name = "threadIDHasSuffix")]
    pub thread_id_has_suffix: Option<String>,
    #[graphql(name = "threadIDEqualFold")]
    pub thread_id_equal_fold: Option<String>,
    #[graphql(name = "threadIDContainsFold")]
    pub thread_id_contains_fold: Option<String>,
    // edge existence predicates (`has<Edge>With` variants pending — module doc)
    pub has_project: Option<bool>,
    pub has_traces: Option<bool>,
}

/// `input TraceWhereInput` — snapshot lines 7213-7300. Implemented: same
/// grammar as [`ThreadWhereInput`] plus the nullable `threadID` reference
/// (which adds `*IsNil` / `*NotNil`). `has<Edge>With` fields pending.
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct TraceWhereInput {
    pub not: Option<Box<TraceWhereInput>>,
    pub and: Option<Vec<TraceWhereInput>>,
    pub or: Option<Vec<TraceWhereInput>>,
    // id field predicates
    pub id: Option<ID>,
    #[graphql(name = "idNEQ")]
    pub id_neq: Option<ID>,
    pub id_in: Option<Vec<ID>>,
    pub id_not_in: Option<Vec<ID>>,
    #[graphql(name = "idGT")]
    pub id_gt: Option<ID>,
    #[graphql(name = "idGTE")]
    pub id_gte: Option<ID>,
    #[graphql(name = "idLT")]
    pub id_lt: Option<ID>,
    #[graphql(name = "idLTE")]
    pub id_lte: Option<ID>,
    // created_at field predicates
    pub created_at: Option<TimeScalar>,
    #[graphql(name = "createdAtNEQ")]
    pub created_at_neq: Option<TimeScalar>,
    pub created_at_in: Option<Vec<TimeScalar>>,
    pub created_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "createdAtGT")]
    pub created_at_gt: Option<TimeScalar>,
    #[graphql(name = "createdAtGTE")]
    pub created_at_gte: Option<TimeScalar>,
    #[graphql(name = "createdAtLT")]
    pub created_at_lt: Option<TimeScalar>,
    #[graphql(name = "createdAtLTE")]
    pub created_at_lte: Option<TimeScalar>,
    // updated_at field predicates
    pub updated_at: Option<TimeScalar>,
    #[graphql(name = "updatedAtNEQ")]
    pub updated_at_neq: Option<TimeScalar>,
    pub updated_at_in: Option<Vec<TimeScalar>>,
    pub updated_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "updatedAtGT")]
    pub updated_at_gt: Option<TimeScalar>,
    #[graphql(name = "updatedAtGTE")]
    pub updated_at_gte: Option<TimeScalar>,
    #[graphql(name = "updatedAtLT")]
    pub updated_at_lt: Option<TimeScalar>,
    #[graphql(name = "updatedAtLTE")]
    pub updated_at_lte: Option<TimeScalar>,
    // project_id field predicates (reference id → eq/neq/in/notIn only)
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    #[graphql(name = "projectIDNEQ")]
    pub project_id_neq: Option<ID>,
    #[graphql(name = "projectIDIn")]
    pub project_id_in: Option<Vec<ID>>,
    #[graphql(name = "projectIDNotIn")]
    pub project_id_not_in: Option<Vec<ID>>,
    // trace_id field predicates (String → full family)
    #[graphql(name = "traceID")]
    pub trace_id: Option<String>,
    #[graphql(name = "traceIDNEQ")]
    pub trace_id_neq: Option<String>,
    #[graphql(name = "traceIDIn")]
    pub trace_id_in: Option<Vec<String>>,
    #[graphql(name = "traceIDNotIn")]
    pub trace_id_not_in: Option<Vec<String>>,
    #[graphql(name = "traceIDGT")]
    pub trace_id_gt: Option<String>,
    #[graphql(name = "traceIDGTE")]
    pub trace_id_gte: Option<String>,
    #[graphql(name = "traceIDLT")]
    pub trace_id_lt: Option<String>,
    #[graphql(name = "traceIDLTE")]
    pub trace_id_lte: Option<String>,
    #[graphql(name = "traceIDContains")]
    pub trace_id_contains: Option<String>,
    #[graphql(name = "traceIDHasPrefix")]
    pub trace_id_has_prefix: Option<String>,
    #[graphql(name = "traceIDHasSuffix")]
    pub trace_id_has_suffix: Option<String>,
    #[graphql(name = "traceIDEqualFold")]
    pub trace_id_equal_fold: Option<String>,
    #[graphql(name = "traceIDContainsFold")]
    pub trace_id_contains_fold: Option<String>,
    // thread_id field predicates (nullable reference id → eq/neq/in/notIn +
    // IsNil/NotNil)
    #[graphql(name = "threadID")]
    pub thread_id: Option<ID>,
    #[graphql(name = "threadIDNEQ")]
    pub thread_id_neq: Option<ID>,
    #[graphql(name = "threadIDIn")]
    pub thread_id_in: Option<Vec<ID>>,
    #[graphql(name = "threadIDNotIn")]
    pub thread_id_not_in: Option<Vec<ID>>,
    #[graphql(name = "threadIDIsNil")]
    pub thread_id_is_nil: Option<bool>,
    #[graphql(name = "threadIDNotNil")]
    pub thread_id_not_nil: Option<bool>,
    // edge existence predicates (`has<Edge>With` variants pending — module doc)
    pub has_project: Option<bool>,
    pub has_thread: Option<bool>,
    pub has_requests: Option<bool>,
}

// ---------------------------------------------------------------------------
// Ordering resolution (Go ent.resolvers.go:490-493 / 506-509)
// ---------------------------------------------------------------------------

/// Internal ordering terms the service layer receives. `Id` is NOT part of the
/// GraphQL `*OrderField` enum — it is ent's `DefaultThreadOrder` /
/// `DefaultTraceOrder` (order by primary key), which the Go resolver
/// substitutes when the client asks for `CREATED_AT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionOrderTerm {
    /// ent `Default*Order` — ascending/descending by row ID.
    Id,
    UpdatedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionOrderSelection {
    pub direction: OrderDirection,
    pub term: ConnectionOrderTerm,
}

/// Lower a GraphQL `ThreadOrder` argument into a service-level selection,
/// mirroring Go `Query.threads` (ent.resolvers.go:490-493): a `CREATED_AT`
/// request is remapped to `ent.DefaultThreadOrder` (order by ID) with the
/// requested direction preserved; `UPDATED_AT` maps one-to-one.
pub fn resolve_thread_order(order_by: Option<ThreadOrder>) -> Option<ConnectionOrderSelection> {
    order_by.map(|order| ConnectionOrderSelection {
        direction: order.direction,
        term: match order.field {
            ThreadOrderField::CreatedAt => ConnectionOrderTerm::Id,
            ThreadOrderField::UpdatedAt => ConnectionOrderTerm::UpdatedAt,
        },
    })
}

/// Lower a GraphQL `TraceOrder` argument, mirroring Go `Query.traces`
/// (ent.resolvers.go:506-509).
pub fn resolve_trace_order(order_by: Option<TraceOrder>) -> Option<ConnectionOrderSelection> {
    order_by.map(|order| ConnectionOrderSelection {
        direction: order.direction,
        term: match order.field {
            TraceOrderField::CreatedAt => ConnectionOrderTerm::Id,
            TraceOrderField::UpdatedAt => ConnectionOrderTerm::UpdatedAt,
        },
    })
}

// ---------------------------------------------------------------------------
// Service traits (host-injected, mirroring the Go resolver's dependency on
// `r.client.Thread` / `r.client.Trace`)
// ---------------------------------------------------------------------------

/// Error surface for the thread/trace query services. Messages mirror the Go
/// `fmt.Errorf` wrappers so frontend error handling stays stable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ThreadTraceError {
    #[error("thread service is not available")]
    ThreadServiceUnavailable,
    #[error("trace service is not available")]
    TraceServiceUnavailable,
    /// Go `validatePaginationArgs` (gql_pagination.go): `first` and `last`
    /// cannot be combined / must be non-negative.
    #[error("{0}")]
    InvalidPagination(String),
    #[error("failed to query threads: {0}")]
    QueryThreads(String),
    #[error("failed to query traces: {0}")]
    QueryTraces(String),
}

/// Arguments for the `threads` connection query, passed through from the
/// GraphQL layer verbatim (Go hands them straight to ent's `Paginate`).
#[derive(Debug, Clone, Default)]
pub struct ThreadConnectionArgs {
    pub access: crate::policy::AdminAccessScope,
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<ConnectionOrderSelection>,
    pub where_filter: Option<ThreadWhereInput>,
}

/// Arguments for the `traces` connection query.
#[derive(Debug, Clone, Default)]
pub struct TraceConnectionArgs {
    pub access: crate::policy::AdminAccessScope,
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<ConnectionOrderSelection>,
    pub where_filter: Option<TraceWhereInput>,
}

/// Backs `Query.threads` (Go ent.resolvers.go:486: `r.client.Thread.Query()
/// .Paginate(...)`).
#[async_trait::async_trait]
pub trait ThreadQueryServices: Send + Sync {
    async fn threads(
        &self,
        args: ThreadConnectionArgs,
    ) -> Result<ThreadConnection, ThreadTraceError>;
}

/// Backs `Query.traces` (Go ent.resolvers.go:502: `r.client.Trace.Query()
/// .Paginate(...)`).
#[async_trait::async_trait]
pub trait TraceQueryServices: Send + Sync {
    async fn traces(&self, args: TraceConnectionArgs) -> Result<TraceConnection, ThreadTraceError>;
}

/// Resolves the injected [`ThreadQueryServices`] from the async-graphql context
/// data bag, surfacing the Go-equivalent "service unavailable" message when no
/// service was wired.
pub(crate) fn thread_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ThreadQueryServices>, String> {
    match ctx.data::<Arc<dyn ThreadQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ThreadTraceError::ThreadServiceUnavailable.to_string()),
    }
}

/// Resolves the injected [`TraceQueryServices`] from the async-graphql context
/// data bag.
pub(crate) fn trace_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn TraceQueryServices>, String> {
    match ctx.data::<Arc<dyn TraceQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ThreadTraceError::TraceServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Resolver wiring (for the coordinator).
//
// IMPORTANT: async-graphql's `#[Object]` macro generates the resolver trait
// impls for the root type, so a root's `#[Object] impl` block CANNOT be split
// across modules (two blocks on the same type → E0119). This slice therefore
// does NOT contribute its own `#[Object] impl QueryRoot`; instead it exposes
// the typed service-lookup helpers + the order-lowering functions + the types,
// and the resolver method bodies are pasted into the single
// `#[Object] impl QueryRoot` in `lib.rs`. The reference bodies:
//
// ```ignore
// /// Mirrors Go `Query.threads` (ent.resolvers.go:486-500).
// #[allow(clippy::too_many_arguments)]
// async fn threads(
//     &self,
//     ctx: &Context<'_>,
//     after: Option<CursorScalar>,
//     first: Option<i32>,
//     before: Option<CursorScalar>,
//     last: Option<i32>,
//     order_by: Option<threads_ext::ThreadOrder>,
//     #[graphql(name = "where")] where_filter: Option<threads_ext::ThreadWhereInput>,
// ) -> Result<threads_ext::ThreadConnection, String> {
//     let services = threads_ext::thread_query_services(ctx)?;
//     let args = threads_ext::ThreadConnectionArgs {
//         after: after.map(|c| c.0),
//         first,
//         before: before.map(|c| c.0),
//         last,
//         order_by: threads_ext::resolve_thread_order(order_by),
//         where_filter,
//     };
//     services.threads(args).await.map_err(|e| e.to_string())
// }
//
// /// Mirrors Go `Query.traces` (ent.resolvers.go:502-516).
// #[allow(clippy::too_many_arguments)]
// async fn traces(
//     &self,
//     ctx: &Context<'_>,
//     after: Option<CursorScalar>,
//     first: Option<i32>,
//     before: Option<CursorScalar>,
//     last: Option<i32>,
//     order_by: Option<threads_ext::TraceOrder>,
//     #[graphql(name = "where")] where_filter: Option<threads_ext::TraceWhereInput>,
// ) -> Result<threads_ext::TraceConnection, String> {
//     let services = threads_ext::trace_query_services(ctx)?;
//     let args = threads_ext::TraceConnectionArgs {
//         after: after.map(|c| c.0),
//         first,
//         before: before.map(|c| c.0),
//         last,
//         order_by: threads_ext::resolve_trace_order(order_by),
//         where_filter,
//     };
//     services.traces(args).await.map_err(|e| e.to_string())
// }
// ```
//
// `Thread` and `Trace` are also added to the `crate::channel::Node` interface
// enum so `implements Node` renders in the SDL.
// ===========================================================================

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::{Name, Value};

    use super::*;
    use crate::pagination::connection_from_offset_page;
    use crate::sdl_parity::{assert_block_parity, snapshot_text};
    use crate::{AdminSchema, admin_schema_builder, build_admin_schema};

    type TestError = Box<dyn std::error::Error>;

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn as_object(value: &Value) -> &async_graphql::indexmap::IndexMap<Name, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected Value::Object, got {other:?}"),
        }
    }

    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    fn sample_thread(id: i64, thread_id: &str) -> Thread {
        Thread {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            project_id: ID::from("1"),
            thread_id: thread_id.to_owned(),
        }
    }

    fn sample_trace(id: i64, trace_id: &str) -> Trace {
        Trace {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            project_id: ID::from("1"),
            trace_id: trace_id.to_owned(),
            thread_id: Some(ID::from("7")),
        }
    }

    // ---------------------------------------------------------------------
    // In-memory fake services.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct InMemoryThreadTraceService {
        threads: Arc<Mutex<Vec<Thread>>>,
        traces: Arc<Mutex<Vec<Trace>>>,
        captured_thread_args: Arc<Mutex<Vec<ThreadConnectionArgs>>>,
        captured_trace_args: Arc<Mutex<Vec<TraceConnectionArgs>>>,
    }

    #[async_trait::async_trait]
    impl ThreadQueryServices for InMemoryThreadTraceService {
        async fn threads(
            &self,
            args: ThreadConnectionArgs,
        ) -> Result<ThreadConnection, ThreadTraceError> {
            lock(&self.captured_thread_args).push(args.clone());
            let mut nodes: Vec<Thread> = lock(&self.threads).clone();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        ConnectionOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        ConnectionOrderTerm::UpdatedAt => a.updated_at.0.cmp(&b.updated_at.0),
                    };
                    match selection.direction {
                        OrderDirection::Asc => ordering,
                        OrderDirection::Desc => ordering.reverse(),
                    }
                });
            }
            let total_count = nodes.len() as i64;
            let page_size = match args.first {
                Some(first) => usize::try_from(first).unwrap_or(0),
                None => nodes.len(),
            };
            let connection = connection_from_offset_page(nodes, 0, page_size);
            Ok(ThreadConnection {
                edges: Some(
                    connection
                        .edges
                        .into_iter()
                        .map(|edge| {
                            Some(ThreadEdge {
                                node: Some(edge.node),
                                cursor: CursorScalar(edge.cursor),
                            })
                        })
                        .collect(),
                ),
                page_info: connection.page_info,
                total_count,
            })
        }
    }

    #[async_trait::async_trait]
    impl TraceQueryServices for InMemoryThreadTraceService {
        async fn traces(
            &self,
            args: TraceConnectionArgs,
        ) -> Result<TraceConnection, ThreadTraceError> {
            lock(&self.captured_trace_args).push(args.clone());
            let mut nodes: Vec<Trace> = lock(&self.traces).clone();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        ConnectionOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        ConnectionOrderTerm::UpdatedAt => a.updated_at.0.cmp(&b.updated_at.0),
                    };
                    match selection.direction {
                        OrderDirection::Asc => ordering,
                        OrderDirection::Desc => ordering.reverse(),
                    }
                });
            }
            let total_count = nodes.len() as i64;
            let page_size = match args.first {
                Some(first) => usize::try_from(first).unwrap_or(0),
                None => nodes.len(),
            };
            let connection = connection_from_offset_page(nodes, 0, page_size);
            Ok(TraceConnection {
                edges: Some(
                    connection
                        .edges
                        .into_iter()
                        .map(|edge| {
                            Some(TraceEdge {
                                node: Some(edge.node),
                                cursor: CursorScalar(edge.cursor),
                            })
                        })
                        .collect(),
                ),
                page_info: connection.page_info,
                total_count,
            })
        }
    }

    fn schema_with(store: &InMemoryThreadTraceService) -> AdminSchema {
        let thread: Arc<dyn ThreadQueryServices> = Arc::new(store.clone());
        let trace: Arc<dyn TraceQueryServices> = Arc::new(store.clone());
        admin_schema_builder()
            .data(thread)
            .data(trace)
            .data(read_requests_context())
            .finish()
    }

    fn read_requests_context() -> conduit_auth::RequestContext {
        let mut context = conduit_auth::RequestContext::new();
        let _ = context.set_principal(
            conduit_auth::Principal::user("thread-trace-test")
                .with_scope(conduit_auth::scopes::slug::READ_REQUESTS),
        );
        context
    }

    fn bare_schema() -> AdminSchema {
        build_admin_schema()
    }

    // -----------------------------------------------------------------
    // SDL parity
    // -----------------------------------------------------------------

    #[test]
    fn sdl_thread_type_matches_snapshot_minus_pending_edges() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        crate::sdl_parity::assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "type Thread",
            "type Thread",
            &[],
            &["firstUserQuery: String", "usageMetadata: UsageMetadata"],
        )?;
        assert!(sdl.contains("type Thread implements Node {"));
        assert!(snapshot.contains("type Thread implements Node {"));
        Ok(())
    }

    #[test]
    fn sdl_trace_type_matches_snapshot_minus_pending_edges() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        crate::sdl_parity::assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "type Trace",
            "type Trace",
            &[],
            &[
                "firstUserQuery: String",
                "firstText: String",
                "rawRootSegment: JSONRawMessage",
                "usageMetadata: UsageMetadata",
            ],
        )?;
        assert!(sdl.contains("type Trace implements Node {"));
        assert!(snapshot.contains("type Trace implements Node {"));
        Ok(())
    }

    #[test]
    fn sdl_connection_and_edge_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "type ThreadConnection",
            "type ThreadEdge",
            "type TraceConnection",
            "type TraceEdge",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_enums_and_orders_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "enum ThreadOrderField",
            "enum TraceOrderField",
            "input ThreadOrder",
            "input TraceOrder",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_where_inputs_match_snapshot_minus_pending_edge_filters() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input ThreadWhereInput",
            "input ThreadWhereInput",
            &[
                "hasProjectWith: [ProjectWhereInput!]",
                "hasTracesWith: [TraceWhereInput!]",
            ],
        )?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input TraceWhereInput",
            "input TraceWhereInput",
            &[
                "hasProjectWith: [ProjectWhereInput!]",
                "hasThreadWith: [ThreadWhereInput!]",
                "hasRequestsWith: [RequestWhereInput!]",
            ],
        )?;
        Ok(())
    }

    #[test]
    fn sdl_query_root_carries_threads_and_traces() {
        let sdl = bare_schema().sdl();
        assert!(
            sdl.contains("threads(") && sdl.contains("): ThreadConnection!"),
            "Query.threads missing: {sdl}"
        );
        assert!(
            sdl.contains("traces(") && sdl.contains("): TraceConnection!"),
            "Query.traces missing: {sdl}"
        );
    }

    // -----------------------------------------------------------------
    // Order lowering (Go CREATED_AT → DefaultOrder remap)
    // -----------------------------------------------------------------

    #[test]
    fn resolve_thread_order_remaps_created_at_to_id() {
        let selection = resolve_thread_order(Some(ThreadOrder {
            direction: OrderDirection::Desc,
            field: ThreadOrderField::CreatedAt,
        }));
        match selection {
            Some(sel) => {
                assert_eq!(sel.term, ConnectionOrderTerm::Id);
                assert_eq!(sel.direction, OrderDirection::Desc);
            }
            None => panic!("expected a selection"),
        }
    }

    #[test]
    fn resolve_trace_order_maps_updated_at_one_to_one() {
        let selection = resolve_trace_order(Some(TraceOrder {
            direction: OrderDirection::Asc,
            field: TraceOrderField::UpdatedAt,
        }));
        match selection {
            Some(sel) => assert_eq!(sel.term, ConnectionOrderTerm::UpdatedAt),
            None => panic!("expected a selection"),
        }
        assert!(resolve_trace_order(None).is_none());
    }

    // -----------------------------------------------------------------
    // Resolver happy paths
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn threads_returns_connection_with_total_count() {
        let store = InMemoryThreadTraceService::default();
        {
            let mut guard = lock(&store.threads);
            guard.push(sample_thread(1, "t-1"));
            guard.push(sample_thread(2, "t-2"));
        }
        let schema = schema_with(&store);

        let resp = schema
            .execute("{ threads { totalCount edges { node { id threadID projectID } cursor } } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let threads = match obj.get(&Name::new("threads")) {
            Some(v) => as_object(v),
            None => panic!("threads field missing"),
        };
        match threads.get(&Name::new("totalCount")) {
            Some(Value::Number(n)) => assert_eq!(n.as_i64(), Some(2)),
            other => panic!("totalCount unexpected: {other:?}"),
        }
        // The captured order_by is None (no orderBy arg supplied).
        let captured = lock(&store.captured_thread_args);
        assert_eq!(captured.len(), 1);
        assert!(captured[0].order_by.is_none());
    }

    #[tokio::test]
    async fn threads_forwards_order_and_where_args() {
        let store = InMemoryThreadTraceService::default();
        lock(&store.threads).push(sample_thread(1, "t-1"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{ threads(orderBy: { field: CREATED_AT, direction: DESC }, where: { threadID: "t-1" }) { totalCount } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&store.captured_thread_args);
        assert_eq!(captured.len(), 1);
        // CREATED_AT lowered to Id (ent DefaultThreadOrder), direction kept.
        match &captured[0].order_by {
            Some(sel) => {
                assert_eq!(sel.term, ConnectionOrderTerm::Id);
                assert_eq!(sel.direction, OrderDirection::Desc);
            }
            None => panic!("expected order_by to be forwarded"),
        }
        match &captured[0].where_filter {
            Some(w) => assert_eq!(w.thread_id.as_deref(), Some("t-1")),
            None => panic!("expected where_filter to be forwarded"),
        }
    }

    #[tokio::test]
    async fn traces_returns_connection_with_nullable_thread_id() {
        let store = InMemoryThreadTraceService::default();
        {
            let mut guard = lock(&store.traces);
            guard.push(sample_trace(1, "tr-1"));
            let mut orphan = sample_trace(2, "tr-2");
            orphan.thread_id = None;
            guard.push(orphan);
        }
        let schema = schema_with(&store);

        let resp = schema
            .execute("{ traces { totalCount edges { node { id traceID threadID } } } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let traces = match obj.get(&Name::new("traces")) {
            Some(v) => as_object(v),
            None => panic!("traces field missing"),
        };
        match traces.get(&Name::new("totalCount")) {
            Some(Value::Number(n)) => assert_eq!(n.as_i64(), Some(2)),
            other => panic!("totalCount unexpected: {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Service-unavailable fallback
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn threads_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema.execute("{ threads { totalCount } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("thread service is not available"),
            "unexpected msg: {msg}"
        );
    }

    #[tokio::test]
    async fn traces_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema.execute("{ traces { totalCount } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("trace service is not available"),
            "unexpected msg: {msg}"
        );
    }
}
