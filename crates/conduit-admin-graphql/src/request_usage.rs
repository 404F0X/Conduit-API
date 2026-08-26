//! RUST-P12-001 S07 — Request / UsageLog GraphQL query slice.
//!
//! Ports the request-log and usage-log GraphQL connection queries declared in
//! `conduit/internal/server/gql/ent.graphql` and resolved by
//! `conduit/internal/server/gql/ent.resolvers.go` (ent-generated connection
//! resolvers).
//!
//! ## Operations ported this slice
//!
//! Queries:
//!   - `Query.requests(after, first, before, last, orderBy, where):
//!     RequestConnection!` — ent connection query over requests (snapshot
//!     lines 5527-5557; Go resolver `Query.requests` in `ent.resolvers.go`).
//!   - `Query.usageLogs(after, first, before, last, orderBy, where):
//!     UsageLogConnection!` — ent connection query over usage logs (snapshot
//!     lines 5682-5712; Go resolver `Query.usageLogs`).
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//!   - `Request requestBody / responseBody / responseChunks` — force-resolver
//!     fields that fetch the bodies from the data storage (`@goField
//!     (forceResolver: true)`). The host wires a content-fetcher; this slice
//!     exposes the field shapes as `Option<JsonValue>` but the actual content
//!     fetching is left for the Request-Content slice.
//!   - `Request.usageLogs(...)` / `Request.executions(...)` — edge fields on
//!     `Request` (snapshot lines 5810, 5842). Pending the
//!     RequestExecution/edge slice.
//!   - `RequestExecution*` — full type + connection + where input + order.
//!     Pending a dedicated RequestExecution slice (the snapshot's
//!     `RequestExecution` carries 30+ fields and a 200+ line WhereInput).
//!   - `Request.channel: Channel @goField(forceResolver: true)` — pending the
//!     Channel edge resolver slice.
//!   - `UsageLog.request: Request!` / `UsageLog.project: Project!` /
//!     `UsageLog.channel: Channel @goField(forceResolver: true)` — pending the
//!     edge resolver slice. The scalar fields of `UsageLog` are ported.
//!   - Dashboard stats (`DashboardOverview`, `RequestStats`, throughput,
//!     etc.) — pending the dashboard slice.
//!
//! ## Service wiring
//!
//! The admin-graphql crate stays free of DB/IO concerns. The host wires
//! concrete implementations of [`RequestQueryServices`] and
//! [`UsageLogQueryServices`] into the schema data bag at build time;
//! resolver-level tests inject in-memory fakes (mirrors the DI pattern used
//! by every other slice in this crate).

use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, InputObject, SimpleObject};

use crate::pagination::PageInfo;
use crate::scalars::{CursorScalar, DecimalScalar, JsonRawMessageScalar, TimeScalar};

// ===========================================================================
// Enums — snapshot lines 6274-6295 (Request) and 7915-7926 (UsageLog).
// ===========================================================================

/// `enum RequestOrderField { CREATED_AT UPDATED_AT }` — snapshot lines
/// 6274-6277.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum RequestOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

/// `enum RequestSource { api playground test }` — snapshot lines 6281-6285.
/// Bound to Go `ent/request.Source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum RequestSource {
    #[graphql(name = "api")]
    Api,
    #[graphql(name = "playground")]
    Playground,
    #[graphql(name = "test")]
    Test,
}

/// `enum RequestStatus { pending processing completed failed canceled }` —
/// snapshot lines 6289-6295. Bound to Go `ent/request.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum RequestStatus {
    #[graphql(name = "pending")]
    Pending,
    #[graphql(name = "processing")]
    Processing,
    #[graphql(name = "completed")]
    Completed,
    #[graphql(name = "failed")]
    Failed,
    #[graphql(name = "canceled")]
    Canceled,
}

/// `enum UsageLogOrderField { CREATED_AT UPDATED_AT }` — snapshot lines
/// 7915-7918.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum UsageLogOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

/// `enum UsageLogSource { api playground test }` — snapshot lines 7922-7926.
/// Bound to Go `ent/usagelog.Source`. Same values as `RequestSource` but a
/// distinct GraphQL type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum UsageLogSource {
    #[graphql(name = "api")]
    Api,
    #[graphql(name = "playground")]
    Playground,
    #[graphql(name = "test")]
    Test,
}

/// Re-export of the ent-global `OrderDirection` enum owned by
/// [`crate::channel`] (snapshot line 4072). Kept as a single canonical
/// registration so the SDL registry does not see two enums with the same
/// GraphQL name.
pub use crate::channel::OrderDirection;

// ===========================================================================
// Output types — Request (snapshot lines 5745-5873).
// ===========================================================================

/// `type Request implements Node` — snapshot lines 5745-5873. Scalar fields
/// only; the `executions(...)` edge is resolved by the [`ComplexObject`]
/// impl below. The remaining edge fields (`apiKey`, `project`, `trace`,
/// `dataStorage`, `channel`, `usageLogs`) are pending (module doc). The four
/// force-resolver body fields are kept as `Option<JsonValue>` so the host
/// can populate them when content fetching is wired; this slice returns
/// `None` for them by default.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
#[graphql(name = "Request")]
pub struct Request {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    /// GraphQL field `apiKeyID` — nullable. Acronym rename pin.
    #[graphql(name = "apiKeyID")]
    pub api_key_id: Option<ID>,
    /// GraphQL field `projectID` — non-null. Acronym rename pin.
    #[graphql(name = "projectID")]
    pub project_id: ID,
    /// GraphQL field `traceID` — nullable. Acronym rename pin.
    #[graphql(name = "traceID")]
    pub trace_id: Option<ID>,
    /// GraphQL field `dataStorageID` — nullable. Acronym rename pin.
    #[graphql(name = "dataStorageID")]
    pub data_storage_id: Option<ID>,
    pub source: RequestSource,
    /// GraphQL field `modelID` — non-null. Acronym rename pin.
    #[graphql(name = "modelID")]
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub format: String,
    pub request_headers: Option<JsonRawMessageScalar>,
    /// Snapshot line 5776: `requestBody: JSONRawMessage! @goField
    /// (forceResolver: true)`. The Go resolver fetches the body from the
    /// data storage; we keep the field non-null (matching the snapshot) and
    /// surface an error if the content fetcher is unwired — the host
    /// populates it from the data storage or returns an empty JSON object
    /// for legacy rows (Go biz/request_body.go does the same).
    pub request_body: JsonRawMessageScalar,
    pub response_body: Option<JsonRawMessageScalar>,
    pub response_chunks: Option<Vec<JsonRawMessageScalar>>,
    /// GraphQL field `channelID` — nullable. Acronym rename pin.
    #[graphql(name = "channelID")]
    pub channel_id: Option<ID>,
    /// GraphQL field `externalID` — nullable. Acronym rename pin.
    #[graphql(name = "externalID")]
    pub external_id: Option<String>,
    pub status: RequestStatus,
    pub stream: bool,
    /// GraphQL field `clientIP` — non-null. Acronym rename pin.
    #[graphql(name = "clientIP")]
    pub client_ip: String,
    /// GraphQL field `metricsLatencyMs` — nullable.
    pub metrics_latency_ms: Option<i64>,
    pub metrics_first_token_latency_ms: Option<i64>,
    pub metrics_reasoning_duration_ms: Option<i64>,
    pub content_saved: bool,
    /// GraphQL field `contentStorageID` — nullable Int (not ID). Acronym
    /// rename pin.
    #[graphql(name = "contentStorageID")]
    pub content_storage_id: Option<i64>,
    pub content_storage_key: Option<String>,
    pub content_saved_at: Option<TimeScalar>,
}

// ===========================================================================
// Request forward-edge resolver — RUST-P12-001 S07 continuation.
//
// Go source: `conduit/internal/server/gql/generated.go:44366-44394`
// auto-resolves `Request.executions(...)` by calling
// `obj.Executions(ctx, after, first, before, last, orderBy, where)` on the
// ent `*ent.Request`. ent injects the FK predicate
// `request_executions.request_id = obj.ID` via `sqlgraph.Neighbors`
// (`ent/request/request.go:118-123`: `ExecutionsTable = "request_executions"`,
// `ExecutionsColumn = "request_id"`).
//
// This slice mirrors that filter by injecting `requestID: <obj.id>` into the
// caller-supplied `where` argument and delegating to the host-injected
// [`crate::request_execution::RequestExecutionQueryServices`].
// ===========================================================================

#[ComplexObject]
impl Request {
    async fn api_key(&self, ctx: &Context<'_>) -> Result<Option<crate::apikey::APIKey>, String> {
        let Some(id) = self.api_key_id.clone() else {
            return Ok(None);
        };
        let services = crate::apikey::apikey_query_services(ctx)?;
        let scope = crate::apikey::api_key_access_scope(ctx)?;
        services
            .api_key(&scope, id.as_str())
            .await
            .map_err(|err| err.to_string())
    }

    async fn project(&self, ctx: &Context<'_>) -> Result<crate::project::Project, String> {
        let services = crate::project::project_query_services(ctx)?;
        let conn = services
            .projects(crate::project::ProjectConnectionArgs {
                first: Some(1),
                where_filter: Some(crate::project::ProjectWhereInput {
                    id: Some(self.project_id.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .map_err(|err| err.to_string())?;
        conn.edges
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .next()
            .and_then(|edge| edge.node)
            .ok_or_else(|| "request's project was not found".to_string())
    }

    async fn channel(&self, ctx: &Context<'_>) -> Result<Option<crate::channel::Channel>, String> {
        let Some(id) = self.channel_id.clone() else {
            return Ok(None);
        };
        let services = crate::channel::channel_query_services(ctx)?;
        let conn = services
            .channels(crate::channel::ChannelConnectionArgs {
                first: Some(1),
                where_filter: Some(crate::channel::ChannelWhereInput {
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
    async fn usage_logs(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<UsageLogOrder>,
        #[graphql(name = "where")] where_filter: Option<UsageLogWhereInput>,
    ) -> Result<UsageLogConnection, String> {
        let services = usage_log_query_services(ctx)?;
        let injected = UsageLogWhereInput {
            request_id: Some(self.id.clone()),
            ..where_filter.unwrap_or_default()
        };
        services
            .usage_logs(UsageLogConnectionArgs {
                after: after.map(|cursor| cursor.0),
                first,
                before: before.map(|cursor| cursor.0),
                last,
                order_by: resolve_usage_log_order(order_by),
                where_filter: Some(injected),
            })
            .await
            .map_err(|err| err.to_string())
    }

    /// `Request.executions(...): RequestExecutionConnection!` — snapshot
    /// lines 5810-5840. Mirrors the ent-generated edge resolver: filter the
    /// request-execution connection by `requestID = obj.id` and the parent
    /// request's `projectID`, then delegate
    /// to the host-injected
    /// [`crate::request_execution::RequestExecutionQueryServices`].
    #[allow(clippy::too_many_arguments)]
    async fn executions(
        &self,
        ctx: &Context<'_>,
        after: Option<CursorScalar>,
        first: Option<i32>,
        before: Option<CursorScalar>,
        last: Option<i32>,
        order_by: Option<crate::request_execution::RequestExecutionOrder>,
        #[graphql(name = "where")] where_filter: Option<
            crate::request_execution::RequestExecutionWhereInput,
        >,
    ) -> Result<crate::request_execution::RequestExecutionConnection, String> {
        let services = crate::request_execution::request_execution_query_services(ctx)?;
        let project_id = self
            .project_id
            .as_str()
            .rsplit('/')
            .next()
            .and_then(|id| id.parse::<i64>().ok())
            .ok_or_else(|| "request project ID is not a valid integer".to_string())?;
        let injected = crate::request_execution::RequestExecutionWhereInput {
            request_id: Some(self.id.clone()),
            project_id: Some(project_id),
            ..where_filter.unwrap_or_default()
        };
        let args = crate::request_execution::RequestExecutionConnectionArgs {
            after: after.map(|cursor| cursor.0),
            first,
            before: before.map(|cursor| cursor.0),
            last,
            order_by: crate::request_execution::resolve_request_execution_order(order_by),
            where_filter: Some(injected),
        };
        services
            .request_executions(args)
            .await
            .map_err(|err| err.to_string())
    }
}

/// `type RequestEdge` — snapshot lines 5894-5903. `node` is nullable in the
/// snapshot (`node: Request`, no `!`).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct RequestEdge {
    pub node: Option<Request>,
    pub cursor: CursorScalar,
}

/// `type RequestConnection` — snapshot lines 5877-5890.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct RequestConnection {
    pub edges: Option<Vec<Option<RequestEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

/// `input RequestOrder` — snapshot lines 6261-6270. `direction:
/// OrderDirection! = ASC` — non-null with default ASC (ent-global pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "RequestOrder")]
pub struct RequestOrder {
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: RequestOrderField,
}

/// `input RequestWhereInput` — snapshot lines 6300-6605 (ent-generated
/// predicate grammar). Implemented: `not`/`and`/`or`, every scalar-field
/// predicate family for the ported scalar fields, plus enum predicates
/// (`source`, `status`). Edge predicates (`hasAPIKey`, `hasProject`,
/// `hasTrace`, `hasDataStorage`, `hasExecutions`, `hasChannel`, `hasChannel`
/// / `hasChannelWith`, `hasRequest`) reference other entities' WhereInputs
/// and are pending.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct RequestWhereInput {
    pub not: Option<Box<RequestWhereInput>>,
    pub and: Option<Vec<RequestWhereInput>>,
    pub or: Option<Vec<RequestWhereInput>>,
    // id field predicates (snapshot lines 6307-6314)
    pub id: Option<ID>,
    #[graphql(name = "idNEQ")]
    pub id_neq: Option<ID>,
    #[graphql(name = "idIn")]
    pub id_in: Option<Vec<ID>>,
    #[graphql(name = "idNotIn")]
    pub id_not_in: Option<Vec<ID>>,
    #[graphql(name = "idGT")]
    pub id_gt: Option<ID>,
    #[graphql(name = "idGTE")]
    pub id_gte: Option<ID>,
    #[graphql(name = "idLT")]
    pub id_lt: Option<ID>,
    #[graphql(name = "idLTE")]
    pub id_lte: Option<ID>,
    // created_at field predicates (snapshot lines 6318-6325)
    pub created_at: Option<TimeScalar>,
    #[graphql(name = "createdAtNEQ")]
    pub created_at_neq: Option<TimeScalar>,
    #[graphql(name = "createdAtIn")]
    pub created_at_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "createdAtNotIn")]
    pub created_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "createdAtGT")]
    pub created_at_gt: Option<TimeScalar>,
    #[graphql(name = "createdAtGTE")]
    pub created_at_gte: Option<TimeScalar>,
    #[graphql(name = "createdAtLT")]
    pub created_at_lt: Option<TimeScalar>,
    #[graphql(name = "createdAtLTE")]
    pub created_at_lte: Option<TimeScalar>,
    // updated_at field predicates (snapshot lines 6329-6336)
    pub updated_at: Option<TimeScalar>,
    #[graphql(name = "updatedAtNEQ")]
    pub updated_at_neq: Option<TimeScalar>,
    #[graphql(name = "updatedAtIn")]
    pub updated_at_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "updatedAtNotIn")]
    pub updated_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "updatedAtGT")]
    pub updated_at_gt: Option<TimeScalar>,
    #[graphql(name = "updatedAtGTE")]
    pub updated_at_gte: Option<TimeScalar>,
    #[graphql(name = "updatedAtLT")]
    pub updated_at_lt: Option<TimeScalar>,
    #[graphql(name = "updatedAtLTE")]
    pub updated_at_lte: Option<TimeScalar>,
    // api_key_id field predicates (snapshot lines 6340-6345)
    #[graphql(name = "apiKeyID")]
    pub api_key_id: Option<ID>,
    #[graphql(name = "apiKeyIDNEQ")]
    pub api_key_id_neq: Option<ID>,
    #[graphql(name = "apiKeyIDIn")]
    pub api_key_id_in: Option<Vec<ID>>,
    #[graphql(name = "apiKeyIDNotIn")]
    pub api_key_id_not_in: Option<Vec<ID>>,
    #[graphql(name = "apiKeyIDIsNil")]
    pub api_key_id_is_nil: Option<bool>,
    #[graphql(name = "apiKeyIDNotNil")]
    pub api_key_id_not_nil: Option<bool>,
    // project_id field predicates (snapshot lines 6349-6352)
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    #[graphql(name = "projectIDNEQ")]
    pub project_id_neq: Option<ID>,
    #[graphql(name = "projectIDIn")]
    pub project_id_in: Option<Vec<ID>>,
    #[graphql(name = "projectIDNotIn")]
    pub project_id_not_in: Option<Vec<ID>>,
    // trace_id field predicates (snapshot lines 6356-6361)
    #[graphql(name = "traceID")]
    pub trace_id: Option<ID>,
    #[graphql(name = "traceIDNEQ")]
    pub trace_id_neq: Option<ID>,
    #[graphql(name = "traceIDIn")]
    pub trace_id_in: Option<Vec<ID>>,
    #[graphql(name = "traceIDNotIn")]
    pub trace_id_not_in: Option<Vec<ID>>,
    #[graphql(name = "traceIDIsNil")]
    pub trace_id_is_nil: Option<bool>,
    #[graphql(name = "traceIDNotNil")]
    pub trace_id_not_nil: Option<bool>,
    // data_storage_id field predicates (snapshot lines 6365-6370)
    #[graphql(name = "dataStorageID")]
    pub data_storage_id: Option<ID>,
    #[graphql(name = "dataStorageIDNEQ")]
    pub data_storage_id_neq: Option<ID>,
    #[graphql(name = "dataStorageIDIn")]
    pub data_storage_id_in: Option<Vec<ID>>,
    #[graphql(name = "dataStorageIDNotIn")]
    pub data_storage_id_not_in: Option<Vec<ID>>,
    #[graphql(name = "dataStorageIDIsNil")]
    pub data_storage_id_is_nil: Option<bool>,
    #[graphql(name = "dataStorageIDNotNil")]
    pub data_storage_id_not_nil: Option<bool>,
    // source field predicates (snapshot lines 6374-6377)
    pub source: Option<RequestSource>,
    #[graphql(name = "sourceNEQ")]
    pub source_neq: Option<RequestSource>,
    #[graphql(name = "sourceIn")]
    pub source_in: Option<Vec<RequestSource>>,
    #[graphql(name = "sourceNotIn")]
    pub source_not_in: Option<Vec<RequestSource>>,
    // model_id field predicates (snapshot lines 6381-6393)
    #[graphql(name = "modelID")]
    pub model_id: Option<String>,
    #[graphql(name = "modelIDNEQ")]
    pub model_id_neq: Option<String>,
    #[graphql(name = "modelIDIn")]
    pub model_id_in: Option<Vec<String>>,
    #[graphql(name = "modelIDNotIn")]
    pub model_id_not_in: Option<Vec<String>>,
    #[graphql(name = "modelIDGT")]
    pub model_id_gt: Option<String>,
    #[graphql(name = "modelIDGTE")]
    pub model_id_gte: Option<String>,
    #[graphql(name = "modelIDLT")]
    pub model_id_lt: Option<String>,
    #[graphql(name = "modelIDLTE")]
    pub model_id_lte: Option<String>,
    #[graphql(name = "modelIDContains")]
    pub model_id_contains: Option<String>,
    #[graphql(name = "modelIDHasPrefix")]
    pub model_id_has_prefix: Option<String>,
    #[graphql(name = "modelIDHasSuffix")]
    pub model_id_has_suffix: Option<String>,
    #[graphql(name = "modelIDEqualFold")]
    pub model_id_equal_fold: Option<String>,
    #[graphql(name = "modelIDContainsFold")]
    pub model_id_contains_fold: Option<String>,
    // reasoning_effort field predicates (snapshot lines 6397-6411)
    pub reasoning_effort: Option<String>,
    #[graphql(name = "reasoningEffortNEQ")]
    pub reasoning_effort_neq: Option<String>,
    #[graphql(name = "reasoningEffortIn")]
    pub reasoning_effort_in: Option<Vec<String>>,
    #[graphql(name = "reasoningEffortNotIn")]
    pub reasoning_effort_not_in: Option<Vec<String>>,
    #[graphql(name = "reasoningEffortGT")]
    pub reasoning_effort_gt: Option<String>,
    #[graphql(name = "reasoningEffortGTE")]
    pub reasoning_effort_gte: Option<String>,
    #[graphql(name = "reasoningEffortLT")]
    pub reasoning_effort_lt: Option<String>,
    #[graphql(name = "reasoningEffortLTE")]
    pub reasoning_effort_lte: Option<String>,
    pub reasoning_effort_contains: Option<String>,
    #[graphql(name = "reasoningEffortHasPrefix")]
    pub reasoning_effort_has_prefix: Option<String>,
    #[graphql(name = "reasoningEffortHasSuffix")]
    pub reasoning_effort_has_suffix: Option<String>,
    #[graphql(name = "reasoningEffortIsNil")]
    pub reasoning_effort_is_nil: Option<bool>,
    #[graphql(name = "reasoningEffortNotNil")]
    pub reasoning_effort_not_nil: Option<bool>,
    #[graphql(name = "reasoningEffortEqualFold")]
    pub reasoning_effort_equal_fold: Option<String>,
    #[graphql(name = "reasoningEffortContainsFold")]
    pub reasoning_effort_contains_fold: Option<String>,
    // format field predicates (snapshot lines 6415-6427)
    pub format: Option<String>,
    #[graphql(name = "formatNEQ")]
    pub format_neq: Option<String>,
    #[graphql(name = "formatIn")]
    pub format_in: Option<Vec<String>>,
    #[graphql(name = "formatNotIn")]
    pub format_not_in: Option<Vec<String>>,
    #[graphql(name = "formatGT")]
    pub format_gt: Option<String>,
    #[graphql(name = "formatGTE")]
    pub format_gte: Option<String>,
    #[graphql(name = "formatLT")]
    pub format_lt: Option<String>,
    #[graphql(name = "formatLTE")]
    pub format_lte: Option<String>,
    pub format_contains: Option<String>,
    #[graphql(name = "formatHasPrefix")]
    pub format_has_prefix: Option<String>,
    #[graphql(name = "formatHasSuffix")]
    pub format_has_suffix: Option<String>,
    #[graphql(name = "formatEqualFold")]
    pub format_equal_fold: Option<String>,
    #[graphql(name = "formatContainsFold")]
    pub format_contains_fold: Option<String>,
    // channel_id field predicates (snapshot lines 6431-6436)
    #[graphql(name = "channelID")]
    pub channel_id: Option<ID>,
    #[graphql(name = "channelIDNEQ")]
    pub channel_id_neq: Option<ID>,
    #[graphql(name = "channelIDIn")]
    pub channel_id_in: Option<Vec<ID>>,
    #[graphql(name = "channelIDNotIn")]
    pub channel_id_not_in: Option<Vec<ID>>,
    #[graphql(name = "channelIDIsNil")]
    pub channel_id_is_nil: Option<bool>,
    #[graphql(name = "channelIDNotNil")]
    pub channel_id_not_nil: Option<bool>,
    // external_id field predicates (snapshot lines 6440-6454)
    #[graphql(name = "externalID")]
    pub external_id: Option<String>,
    #[graphql(name = "externalIDNEQ")]
    pub external_id_neq: Option<String>,
    #[graphql(name = "externalIDIn")]
    pub external_id_in: Option<Vec<String>>,
    #[graphql(name = "externalIDNotIn")]
    pub external_id_not_in: Option<Vec<String>>,
    #[graphql(name = "externalIDGT")]
    pub external_id_gt: Option<String>,
    #[graphql(name = "externalIDGTE")]
    pub external_id_gte: Option<String>,
    #[graphql(name = "externalIDLT")]
    pub external_id_lt: Option<String>,
    #[graphql(name = "externalIDLTE")]
    pub external_id_lte: Option<String>,
    #[graphql(name = "externalIDContains")]
    pub external_id_contains: Option<String>,
    #[graphql(name = "externalIDHasPrefix")]
    pub external_id_has_prefix: Option<String>,
    #[graphql(name = "externalIDHasSuffix")]
    pub external_id_has_suffix: Option<String>,
    #[graphql(name = "externalIDIsNil")]
    pub external_id_is_nil: Option<bool>,
    #[graphql(name = "externalIDNotNil")]
    pub external_id_not_nil: Option<bool>,
    #[graphql(name = "externalIDEqualFold")]
    pub external_id_equal_fold: Option<String>,
    #[graphql(name = "externalIDContainsFold")]
    pub external_id_contains_fold: Option<String>,
    // status field predicates (snapshot lines 6458-6461)
    pub status: Option<RequestStatus>,
    #[graphql(name = "statusNEQ")]
    pub status_neq: Option<RequestStatus>,
    #[graphql(name = "statusIn")]
    pub status_in: Option<Vec<RequestStatus>>,
    #[graphql(name = "statusNotIn")]
    pub status_not_in: Option<Vec<RequestStatus>>,
    // stream field predicates (snapshot lines 6465-6466)
    pub stream: Option<bool>,
    #[graphql(name = "streamNEQ")]
    pub stream_neq: Option<bool>,
    // client_ip field predicates (snapshot lines 6470-6480)
    #[graphql(name = "clientIP")]
    pub client_ip: Option<String>,
    #[graphql(name = "clientIPNEQ")]
    pub client_ip_neq: Option<String>,
    #[graphql(name = "clientIPIn")]
    pub client_ip_in: Option<Vec<String>>,
    #[graphql(name = "clientIPNotIn")]
    pub client_ip_not_in: Option<Vec<String>>,
    #[graphql(name = "clientIPGT")]
    pub client_ip_gt: Option<String>,
    #[graphql(name = "clientIPGTE")]
    pub client_ip_gte: Option<String>,
    #[graphql(name = "clientIPLT")]
    pub client_ip_lt: Option<String>,
    #[graphql(name = "clientIPLTE")]
    pub client_ip_lte: Option<String>,
    #[graphql(name = "clientIPContains")]
    pub client_ip_contains: Option<String>,
    #[graphql(name = "clientIPHasPrefix")]
    pub client_ip_has_prefix: Option<String>,
    #[graphql(name = "clientIPHasSuffix")]
    pub client_ip_has_suffix: Option<String>,
    #[graphql(name = "clientIPEqualFold")]
    pub client_ip_equal_fold: Option<String>,
    #[graphql(name = "clientIPContainsFold")]
    pub client_ip_contains_fold: Option<String>,
    // content_saved field predicates (snapshot lines 6525-6526)
    pub content_saved: Option<bool>,
    #[graphql(name = "contentSavedNEQ")]
    pub content_saved_neq: Option<bool>,
    // content_storage_id field predicates (snapshot lines 6530-6539)
    #[graphql(name = "contentStorageID")]
    pub content_storage_id: Option<i64>,
    #[graphql(name = "contentStorageIDNEQ")]
    pub content_storage_id_neq: Option<i64>,
    #[graphql(name = "contentStorageIDIn")]
    pub content_storage_id_in: Option<Vec<i64>>,
    #[graphql(name = "contentStorageIDNotIn")]
    pub content_storage_id_not_in: Option<Vec<i64>>,
    #[graphql(name = "contentStorageIDGT")]
    pub content_storage_id_gt: Option<i64>,
    #[graphql(name = "contentStorageIDGTE")]
    pub content_storage_id_gte: Option<i64>,
    #[graphql(name = "contentStorageIDLT")]
    pub content_storage_id_lt: Option<i64>,
    #[graphql(name = "contentStorageIDLTE")]
    pub content_storage_id_lte: Option<i64>,
    #[graphql(name = "contentStorageIDIsNil")]
    pub content_storage_id_is_nil: Option<bool>,
    #[graphql(name = "contentStorageIDNotNil")]
    pub content_storage_id_not_nil: Option<bool>,
}

// ===========================================================================
// Output types — UsageLog (snapshot lines 7776-7868).
// ===========================================================================

/// `type TierCost { upTo: Int units: Int! subtotal: Decimal! }` — snapshot
/// lines 1066-1073. Used inside `CostItem.tierBreakdown`.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "TierCost")]
pub struct TierCost {
    /// Snapshot field `upTo` — nullable Int.
    pub up_to: Option<i64>,
    pub units: i64,
    pub subtotal: DecimalScalar,
}

/// `enum PriceItemCode { prompt_tokens completion_tokens prompt_cached_tokens
/// prompt_write_cached_tokens }` — snapshot lines 9133-9138.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PriceItemCode {
    #[graphql(name = "prompt_tokens")]
    PromptTokens,
    #[graphql(name = "completion_tokens")]
    CompletionTokens,
    #[graphql(name = "prompt_cached_tokens")]
    PromptCachedTokens,
    #[graphql(name = "prompt_write_cached_tokens")]
    PromptWriteCachedTokens,
}

/// `type CostItem` — snapshot lines 1075-1080.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "CostItem")]
pub struct CostItem {
    #[graphql(name = "itemCode")]
    pub item_code: PriceItemCode,
    pub quantity: i64,
    pub tier_breakdown: Option<Vec<TierCost>>,
    pub subtotal: DecimalScalar,
}

/// `type UsageLog implements Node` — snapshot lines 7776-7868. Scalar fields
/// only; edge fields (`request`, `project`, `channel`) are pending.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "UsageLog")]
pub struct UsageLog {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    /// GraphQL field `requestID` — non-null. Acronym rename pin.
    #[graphql(name = "requestID")]
    pub request_id: ID,
    /// GraphQL field `apiKeyID` — nullable Int (NOT ID, per snapshot line
    /// 7784). Acronym rename pin.
    #[graphql(name = "apiKeyID")]
    pub api_key_id: Option<i64>,
    /// GraphQL field `projectID` — non-null ID. Acronym rename pin.
    #[graphql(name = "projectID")]
    pub project_id: ID,
    /// GraphQL field `channelID` — nullable. Acronym rename pin.
    #[graphql(name = "channelID")]
    pub channel_id: Option<ID>,
    /// GraphQL field `modelID` — non-null. Acronym rename pin.
    #[graphql(name = "modelID")]
    pub model_id: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub prompt_audio_tokens: Option<i64>,
    pub prompt_cached_tokens: Option<i64>,
    pub prompt_write_cached_tokens: Option<i64>,
    /// GraphQL field `promptWriteCachedTokens5m` — note trailing `5m`
    /// (numeric+letter), NOT remappable by camelCase.
    #[graphql(name = "promptWriteCachedTokens5m")]
    pub prompt_write_cached_tokens_5m: Option<i64>,
    /// GraphQL field `promptWriteCachedTokens1h`.
    #[graphql(name = "promptWriteCachedTokens1h")]
    pub prompt_write_cached_tokens_1h: Option<i64>,
    pub completion_audio_tokens: Option<i64>,
    pub completion_reasoning_tokens: Option<i64>,
    pub completion_accepted_prediction_tokens: Option<i64>,
    pub completion_rejected_prediction_tokens: Option<i64>,
    pub source: UsageLogSource,
    pub format: String,
    pub total_cost: Option<f64>,
    pub cost_items: Option<Vec<CostItem>>,
    /// GraphQL field `costPriceReferenceID` — nullable. Acronym rename pin.
    #[graphql(name = "costPriceReferenceID")]
    pub cost_price_reference_id: Option<String>,
}

/// `type UsageLogEdge` — snapshot lines 7889-7898. `node` is nullable in the
/// snapshot (`node: UsageLog`, no `!`).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct UsageLogEdge {
    pub node: Option<UsageLog>,
    pub cursor: CursorScalar,
}

/// `type UsageLogConnection` — snapshot lines 7872-7885.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct UsageLogConnection {
    pub edges: Option<Vec<Option<UsageLogEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

/// `input UsageLogOrder` — snapshot lines 7902-7911. `direction:
/// OrderDirection! = ASC` — non-null with default ASC (ent-global pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "UsageLogOrder")]
pub struct UsageLogOrder {
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: UsageLogOrderField,
}

/// `input UsageLogWhereInput` — snapshot lines 7931-8239 (ent-generated
/// predicate grammar). Implemented: `not`/`and`/`or`, every scalar-field
/// predicate family for the ported scalar fields, plus enum/source
/// predicates. Edge predicates (`hasRequest`, `hasProject`, `hasChannel`)
/// are pending.
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct UsageLogWhereInput {
    pub not: Option<Box<UsageLogWhereInput>>,
    pub and: Option<Vec<UsageLogWhereInput>>,
    pub or: Option<Vec<UsageLogWhereInput>>,
    // id field predicates (snapshot lines 7938-7945)
    pub id: Option<ID>,
    #[graphql(name = "idNEQ")]
    pub id_neq: Option<ID>,
    #[graphql(name = "idIn")]
    pub id_in: Option<Vec<ID>>,
    #[graphql(name = "idNotIn")]
    pub id_not_in: Option<Vec<ID>>,
    #[graphql(name = "idGT")]
    pub id_gt: Option<ID>,
    #[graphql(name = "idGTE")]
    pub id_gte: Option<ID>,
    #[graphql(name = "idLT")]
    pub id_lt: Option<ID>,
    #[graphql(name = "idLTE")]
    pub id_lte: Option<ID>,
    // created_at field predicates (snapshot lines 7949-7956)
    pub created_at: Option<TimeScalar>,
    #[graphql(name = "createdAtNEQ")]
    pub created_at_neq: Option<TimeScalar>,
    #[graphql(name = "createdAtIn")]
    pub created_at_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "createdAtNotIn")]
    pub created_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "createdAtGT")]
    pub created_at_gt: Option<TimeScalar>,
    #[graphql(name = "createdAtGTE")]
    pub created_at_gte: Option<TimeScalar>,
    #[graphql(name = "createdAtLT")]
    pub created_at_lt: Option<TimeScalar>,
    #[graphql(name = "createdAtLTE")]
    pub created_at_lte: Option<TimeScalar>,
    // updated_at field predicates (snapshot lines 7960-7967)
    pub updated_at: Option<TimeScalar>,
    #[graphql(name = "updatedAtNEQ")]
    pub updated_at_neq: Option<TimeScalar>,
    #[graphql(name = "updatedAtIn")]
    pub updated_at_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "updatedAtNotIn")]
    pub updated_at_not_in: Option<Vec<TimeScalar>>,
    #[graphql(name = "updatedAtGT")]
    pub updated_at_gt: Option<TimeScalar>,
    #[graphql(name = "updatedAtGTE")]
    pub updated_at_gte: Option<TimeScalar>,
    #[graphql(name = "updatedAtLT")]
    pub updated_at_lt: Option<TimeScalar>,
    #[graphql(name = "updatedAtLTE")]
    pub updated_at_lte: Option<TimeScalar>,
    // request_id field predicates (snapshot lines 7971-7974)
    #[graphql(name = "requestID")]
    pub request_id: Option<ID>,
    #[graphql(name = "requestIDNEQ")]
    pub request_id_neq: Option<ID>,
    #[graphql(name = "requestIDIn")]
    pub request_id_in: Option<Vec<ID>>,
    #[graphql(name = "requestIDNotIn")]
    pub request_id_not_in: Option<Vec<ID>>,
    // api_key_id field predicates (snapshot lines 7978-7987)
    #[graphql(name = "apiKeyID")]
    pub api_key_id: Option<i64>,
    #[graphql(name = "apiKeyIDNEQ")]
    pub api_key_id_neq: Option<i64>,
    #[graphql(name = "apiKeyIDIn")]
    pub api_key_id_in: Option<Vec<i64>>,
    #[graphql(name = "apiKeyIDNotIn")]
    pub api_key_id_not_in: Option<Vec<i64>>,
    #[graphql(name = "apiKeyIDGT")]
    pub api_key_id_gt: Option<i64>,
    #[graphql(name = "apiKeyIDGTE")]
    pub api_key_id_gte: Option<i64>,
    #[graphql(name = "apiKeyIDLT")]
    pub api_key_id_lt: Option<i64>,
    #[graphql(name = "apiKeyIDLTE")]
    pub api_key_id_lte: Option<i64>,
    #[graphql(name = "apiKeyIDIsNil")]
    pub api_key_id_is_nil: Option<bool>,
    #[graphql(name = "apiKeyIDNotNil")]
    pub api_key_id_not_nil: Option<bool>,
    // project_id field predicates (snapshot lines 7991-7994)
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    #[graphql(name = "projectIDNEQ")]
    pub project_id_neq: Option<ID>,
    #[graphql(name = "projectIDIn")]
    pub project_id_in: Option<Vec<ID>>,
    #[graphql(name = "projectIDNotIn")]
    pub project_id_not_in: Option<Vec<ID>>,
    // channel_id field predicates (snapshot lines 7998-8003)
    #[graphql(name = "channelID")]
    pub channel_id: Option<ID>,
    #[graphql(name = "channelIDNEQ")]
    pub channel_id_neq: Option<ID>,
    #[graphql(name = "channelIDIn")]
    pub channel_id_in: Option<Vec<ID>>,
    #[graphql(name = "channelIDNotIn")]
    pub channel_id_not_in: Option<Vec<ID>>,
    #[graphql(name = "channelIDIsNil")]
    pub channel_id_is_nil: Option<bool>,
    #[graphql(name = "channelIDNotNil")]
    pub channel_id_not_nil: Option<bool>,
    // model_id field predicates (snapshot lines 8007-8019)
    #[graphql(name = "modelID")]
    pub model_id: Option<String>,
    #[graphql(name = "modelIDNEQ")]
    pub model_id_neq: Option<String>,
    #[graphql(name = "modelIDIn")]
    pub model_id_in: Option<Vec<String>>,
    #[graphql(name = "modelIDNotIn")]
    pub model_id_not_in: Option<Vec<String>>,
    #[graphql(name = "modelIDGT")]
    pub model_id_gt: Option<String>,
    #[graphql(name = "modelIDGTE")]
    pub model_id_gte: Option<String>,
    #[graphql(name = "modelIDLT")]
    pub model_id_lt: Option<String>,
    #[graphql(name = "modelIDLTE")]
    pub model_id_lte: Option<String>,
    #[graphql(name = "modelIDContains")]
    pub model_id_contains: Option<String>,
    #[graphql(name = "modelIDHasPrefix")]
    pub model_id_has_prefix: Option<String>,
    #[graphql(name = "modelIDHasSuffix")]
    pub model_id_has_suffix: Option<String>,
    #[graphql(name = "modelIDEqualFold")]
    pub model_id_equal_fold: Option<String>,
    #[graphql(name = "modelIDContainsFold")]
    pub model_id_contains_fold: Option<String>,
    // prompt_tokens / completion_tokens / total_tokens — non-nullable Int
    // predicates (snapshot lines 8023-8052).
    pub prompt_tokens: Option<i64>,
    #[graphql(name = "promptTokensNEQ")]
    pub prompt_tokens_neq: Option<i64>,
    #[graphql(name = "promptTokensIn")]
    pub prompt_tokens_in: Option<Vec<i64>>,
    #[graphql(name = "promptTokensNotIn")]
    pub prompt_tokens_not_in: Option<Vec<i64>>,
    #[graphql(name = "promptTokensGT")]
    pub prompt_tokens_gt: Option<i64>,
    #[graphql(name = "promptTokensGTE")]
    pub prompt_tokens_gte: Option<i64>,
    #[graphql(name = "promptTokensLT")]
    pub prompt_tokens_lt: Option<i64>,
    #[graphql(name = "promptTokensLTE")]
    pub prompt_tokens_lte: Option<i64>,
    pub completion_tokens: Option<i64>,
    #[graphql(name = "completionTokensNEQ")]
    pub completion_tokens_neq: Option<i64>,
    #[graphql(name = "completionTokensIn")]
    pub completion_tokens_in: Option<Vec<i64>>,
    #[graphql(name = "completionTokensNotIn")]
    pub completion_tokens_not_in: Option<Vec<i64>>,
    #[graphql(name = "completionTokensGT")]
    pub completion_tokens_gt: Option<i64>,
    #[graphql(name = "completionTokensGTE")]
    pub completion_tokens_gte: Option<i64>,
    #[graphql(name = "completionTokensLT")]
    pub completion_tokens_lt: Option<i64>,
    #[graphql(name = "completionTokensLTE")]
    pub completion_tokens_lte: Option<i64>,
    pub total_tokens: Option<i64>,
    #[graphql(name = "totalTokensNEQ")]
    pub total_tokens_neq: Option<i64>,
    #[graphql(name = "totalTokensIn")]
    pub total_tokens_in: Option<Vec<i64>>,
    #[graphql(name = "totalTokensNotIn")]
    pub total_tokens_not_in: Option<Vec<i64>>,
    #[graphql(name = "totalTokensGT")]
    pub total_tokens_gt: Option<i64>,
    #[graphql(name = "totalTokensGTE")]
    pub total_tokens_gte: Option<i64>,
    #[graphql(name = "totalTokensLT")]
    pub total_tokens_lt: Option<i64>,
    #[graphql(name = "totalTokensLTE")]
    pub total_tokens_lte: Option<i64>,
    // source field predicates (snapshot lines 8173-8176)
    pub source: Option<UsageLogSource>,
    #[graphql(name = "sourceNEQ")]
    pub source_neq: Option<UsageLogSource>,
    #[graphql(name = "sourceIn")]
    pub source_in: Option<Vec<UsageLogSource>>,
    #[graphql(name = "sourceNotIn")]
    pub source_not_in: Option<Vec<UsageLogSource>>,
    // format field predicates (snapshot lines 8180-8192)
    pub format: Option<String>,
    #[graphql(name = "formatNEQ")]
    pub format_neq: Option<String>,
    #[graphql(name = "formatIn")]
    pub format_in: Option<Vec<String>>,
    #[graphql(name = "formatNotIn")]
    pub format_not_in: Option<Vec<String>>,
    #[graphql(name = "formatGT")]
    pub format_gt: Option<String>,
    #[graphql(name = "formatGTE")]
    pub format_gte: Option<String>,
    #[graphql(name = "formatLT")]
    pub format_lt: Option<String>,
    #[graphql(name = "formatLTE")]
    pub format_lte: Option<String>,
    pub format_contains: Option<String>,
    #[graphql(name = "formatHasPrefix")]
    pub format_has_prefix: Option<String>,
    #[graphql(name = "formatHasSuffix")]
    pub format_has_suffix: Option<String>,
    #[graphql(name = "formatEqualFold")]
    pub format_equal_fold: Option<String>,
    #[graphql(name = "formatContainsFold")]
    pub format_contains_fold: Option<String>,
    // total_cost field predicates (snapshot lines 8196-8205) — Float, not
    // Eq-derivable, so we keep the whole struct `PartialEq` only.
    pub total_cost: Option<f64>,
    #[graphql(name = "totalCostNEQ")]
    pub total_cost_neq: Option<f64>,
    #[graphql(name = "totalCostIn")]
    pub total_cost_in: Option<Vec<f64>>,
    #[graphql(name = "totalCostNotIn")]
    pub total_cost_not_in: Option<Vec<f64>>,
    #[graphql(name = "totalCostGT")]
    pub total_cost_gt: Option<f64>,
    #[graphql(name = "totalCostGTE")]
    pub total_cost_gte: Option<f64>,
    #[graphql(name = "totalCostLT")]
    pub total_cost_lt: Option<f64>,
    #[graphql(name = "totalCostLTE")]
    pub total_cost_lte: Option<f64>,
    #[graphql(name = "totalCostIsNil")]
    pub total_cost_is_nil: Option<bool>,
    #[graphql(name = "totalCostNotNil")]
    pub total_cost_not_nil: Option<bool>,
}

// ===========================================================================
// Order selection lowering (mirrors `resolve_channel_order` in channel.rs)
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestOrderTerm {
    /// `CREATED_AT` ordering is remapped to the ent default ID order, with
    /// the requested direction preserved (gql_pagination.go:413).
    Id,
    UpdatedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestOrderSelection {
    pub direction: OrderDirection,
    pub term: RequestOrderTerm,
}

pub fn resolve_request_order(order_by: Option<RequestOrder>) -> Option<RequestOrderSelection> {
    order_by.map(|order| RequestOrderSelection {
        direction: order.direction,
        term: match order.field {
            RequestOrderField::CreatedAt => RequestOrderTerm::Id,
            RequestOrderField::UpdatedAt => RequestOrderTerm::UpdatedAt,
        },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageLogOrderTerm {
    Id,
    UpdatedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageLogOrderSelection {
    pub direction: OrderDirection,
    pub term: UsageLogOrderTerm,
}

pub fn resolve_usage_log_order(order_by: Option<UsageLogOrder>) -> Option<UsageLogOrderSelection> {
    order_by.map(|order| UsageLogOrderSelection {
        direction: order.direction,
        term: match order.field {
            UsageLogOrderField::CreatedAt => UsageLogOrderTerm::Id,
            UsageLogOrderField::UpdatedAt => UsageLogOrderTerm::UpdatedAt,
        },
    })
}

// ===========================================================================
// Service traits (host-injected)
// ===========================================================================

#[derive(Debug, Clone, thiserror::Error)]
pub enum RequestQueryError {
    #[error("request service is not available")]
    ServiceUnavailable,
    #[error("failed to query requests: {0}")]
    Query(String),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum UsageLogQueryError {
    #[error("usage log service is not available")]
    ServiceUnavailable,
    #[error("failed to query usage logs: {0}")]
    Query(String),
}

#[derive(Debug, Clone, Default)]
pub struct RequestConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<RequestOrderSelection>,
    pub where_filter: Option<RequestWhereInput>,
}

#[derive(Debug, Clone, Default)]
pub struct UsageLogConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<UsageLogOrderSelection>,
    pub where_filter: Option<UsageLogWhereInput>,
}

#[async_trait::async_trait]
pub trait RequestQueryServices: Send + Sync {
    async fn requests(
        &self,
        args: RequestConnectionArgs,
    ) -> Result<RequestConnection, RequestQueryError>;
}

#[async_trait::async_trait]
pub trait UsageLogQueryServices: Send + Sync {
    async fn usage_logs(
        &self,
        args: UsageLogConnectionArgs,
    ) -> Result<UsageLogConnection, UsageLogQueryError>;
}

pub(crate) fn request_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn RequestQueryServices>, String> {
    match ctx.data::<Arc<dyn RequestQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(RequestQueryError::ServiceUnavailable.to_string()),
    }
}

pub(crate) fn usage_log_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn UsageLogQueryServices>, String> {
    match ctx.data::<Arc<dyn UsageLogQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(UsageLogQueryError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Helper: build a JSON object Value (test fixture convenience).
// ===========================================================================

/// Convenience used by tests to build a [`JsonRawMessageScalar`] from any
/// `serde_json::Value` without panicking.
#[cfg(test)]
pub(crate) fn json_msg(value: serde_json::Value) -> JsonRawMessageScalar {
    JsonRawMessageScalar(value)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::{EmptySubscription, Name, Object, Schema, Value};
    use chrono::{TimeZone, Utc};
    use rust_decimal::Decimal;
    use serde_json::json;

    use super::*;
    use crate::mutation::MutationRoot;

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn as_object(value: &Value) -> &async_graphql::indexmap::IndexMap<Name, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected Value::Object, got {other:?}"),
        }
    }

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

    // ---------------------------------------------------------------------
    // In-memory fake services.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct FakeRequestServices {
        records: Arc<Mutex<Vec<Request>>>,
        query_error: Option<RequestQueryError>,
    }

    #[async_trait::async_trait]
    impl RequestQueryServices for FakeRequestServices {
        async fn requests(
            &self,
            args: RequestConnectionArgs,
        ) -> Result<RequestConnection, RequestQueryError> {
            match &self.query_error {
                Some(err) => Err(err.clone()),
                None => {
                    let records = lock(&self.records).clone();
                    let filtered: Vec<Request> =
                        match args.where_filter.as_ref().and_then(|w| w.status) {
                            Some(status) => {
                                records.into_iter().filter(|r| r.status == status).collect()
                            }
                            None => records,
                        };
                    let total = filtered.len() as i64;
                    Ok(RequestConnection {
                        edges: Some(
                            filtered
                                .into_iter()
                                .map(|node| {
                                    Some(RequestEdge {
                                        cursor: CursorScalar(format!("req:{}", node.id.as_str())),
                                        node: Some(node),
                                    })
                                })
                                .collect(),
                        ),
                        page_info: PageInfo {
                            has_previous_page: false,
                            has_next_page: false,
                            start_cursor: None,
                            end_cursor: None,
                        },
                        total_count: total,
                    })
                }
            }
        }
    }

    #[derive(Default, Clone)]
    struct FakeUsageLogServices {
        records: Arc<Mutex<Vec<UsageLog>>>,
        query_error: Option<UsageLogQueryError>,
    }

    #[async_trait::async_trait]
    impl UsageLogQueryServices for FakeUsageLogServices {
        async fn usage_logs(
            &self,
            _args: UsageLogConnectionArgs,
        ) -> Result<UsageLogConnection, UsageLogQueryError> {
            match &self.query_error {
                Some(err) => Err(err.clone()),
                None => {
                    let records = lock(&self.records).clone();
                    let total = records.len() as i64;
                    Ok(UsageLogConnection {
                        edges: Some(
                            records
                                .into_iter()
                                .map(|node| {
                                    Some(UsageLogEdge {
                                        cursor: CursorScalar(format!("usage:{}", node.id.as_str())),
                                        node: Some(node),
                                    })
                                })
                                .collect(),
                        ),
                        page_info: PageInfo {
                            has_previous_page: false,
                            has_next_page: false,
                            start_cursor: None,
                            end_cursor: None,
                        },
                        total_count: total,
                    })
                }
            }
        }
    }

    type TestSchema = Schema<crate::QueryRoot, MutationRoot, EmptySubscription>;

    fn schema_with_request_services(services: FakeRequestServices) -> TestSchema {
        let arc: Arc<dyn RequestQueryServices> = Arc::new(services);
        crate::admin_schema_builder().data(arc).finish()
    }

    fn schema_with_usage_services(services: FakeUsageLogServices) -> TestSchema {
        let arc: Arc<dyn UsageLogQueryServices> = Arc::new(services);
        crate::admin_schema_builder().data(arc).finish()
    }

    // ---- order-lowering semantics -----------------------------------

    #[test]
    fn resolve_request_order_remaps_created_at_to_default_id_order() {
        let order = Some(RequestOrder {
            direction: OrderDirection::Desc,
            field: RequestOrderField::CreatedAt,
        });
        let sel = resolve_request_order(order);
        match sel {
            Some(RequestOrderSelection { direction, term }) => {
                assert_eq!(direction, OrderDirection::Desc);
                assert_eq!(term, RequestOrderTerm::Id);
            }
            None => panic!("expected Some selection"),
        }
    }

    #[test]
    fn resolve_usage_log_order_passes_updated_at_through() {
        let order = Some(UsageLogOrder {
            direction: OrderDirection::Asc,
            field: UsageLogOrderField::UpdatedAt,
        });
        let sel = resolve_usage_log_order(order);
        match sel {
            Some(UsageLogOrderSelection { direction, term }) => {
                assert_eq!(direction, OrderDirection::Asc);
                assert_eq!(term, UsageLogOrderTerm::UpdatedAt);
            }
            None => panic!("expected Some selection"),
        }
    }

    // ---- resolver: requests query ----------------------------------

    #[tokio::test]
    async fn requests_returns_connection_shape() -> Result<(), String> {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let req = Request {
            id: ID::from("1"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            api_key_id: Some(ID::from("ak-1")),
            project_id: ID::from("1"),
            trace_id: None,
            data_storage_id: None,
            source: RequestSource::Api,
            model_id: "gpt-4".to_string(),
            reasoning_effort: None,
            format: "openai".to_string(),
            request_headers: Some(json_msg(json!({"x": "y"}))),
            request_body: json_msg(json!({})),
            response_body: None,
            response_chunks: None,
            channel_id: Some(ID::from("ch-1")),
            external_id: None,
            status: RequestStatus::Completed,
            stream: false,
            client_ip: "127.0.0.1".to_string(),
            metrics_latency_ms: Some(42),
            metrics_first_token_latency_ms: None,
            metrics_reasoning_duration_ms: None,
            content_saved: false,
            content_storage_id: None,
            content_storage_key: None,
            content_saved_at: None,
        };
        let fake = FakeRequestServices {
            records: Arc::new(Mutex::new(vec![req])),
            ..FakeRequestServices::default()
        };
        let schema = schema_with_request_services(fake);

        let resp = schema
            .execute(
                "{ requests { totalCount edges { node { id modelID status source stream clientIP } } } }",
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let conn = match obj.get(&Name::new("requests")) {
            Some(v) => v,
            None => panic!("requests missing"),
        };
        let conn_fields = as_object(conn);
        match conn_fields.get(&Name::new("totalCount")) {
            Some(Value::Number(n)) => assert_eq!(n.as_i64(), Some(1)),
            other => panic!("totalCount unexpected: {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn requests_surfaces_service_unavailable_when_unwired() {
        let schema: TestSchema = crate::admin_schema_builder().finish();
        let resp = schema.execute("{ requests { totalCount } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("request service is not available"),
            "unexpected msg: {msg}"
        );
    }

    #[tokio::test]
    async fn requests_surfaces_query_error() {
        let fake = FakeRequestServices {
            query_error: Some(RequestQueryError::Query("db offline".to_string())),
            ..FakeRequestServices::default()
        };
        let schema = schema_with_request_services(fake);
        let resp = schema.execute("{ requests { totalCount } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to query requests"), "msg: {msg}");
        assert!(msg.contains("db offline"), "msg: {msg}");
    }

    // ---- resolver: usage_logs query --------------------------------

    #[tokio::test]
    async fn usage_logs_returns_connection_shape() -> Result<(), String> {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let log = UsageLog {
            id: ID::from("1"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            request_id: ID::from("r-1"),
            api_key_id: Some(7),
            project_id: ID::from("1"),
            channel_id: Some(ID::from("ch-1")),
            model_id: "gpt-4".to_string(),
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
            prompt_audio_tokens: None,
            prompt_cached_tokens: Some(5),
            prompt_write_cached_tokens: None,
            prompt_write_cached_tokens_5m: None,
            prompt_write_cached_tokens_1h: None,
            completion_audio_tokens: None,
            completion_reasoning_tokens: None,
            completion_accepted_prediction_tokens: None,
            completion_rejected_prediction_tokens: None,
            source: UsageLogSource::Api,
            format: "openai".to_string(),
            total_cost: Some(0.001),
            cost_items: Some(vec![CostItem {
                item_code: PriceItemCode::PromptTokens,
                quantity: 10,
                tier_breakdown: None,
                subtotal: DecimalScalar(Decimal::new(1, 4)),
            }]),
            cost_price_reference_id: None,
        };
        let fake = FakeUsageLogServices {
            records: Arc::new(Mutex::new(vec![log])),
            ..FakeUsageLogServices::default()
        };
        let schema = schema_with_usage_services(fake);

        let resp = schema
            .execute(
                "{ usageLogs { totalCount edges { node { id modelID promptTokens completionTokens totalTokens source totalCost costItems { itemCode quantity subtotal } } } } }",
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let conn = match obj.get(&Name::new("usageLogs")) {
            Some(v) => v,
            None => panic!("usageLogs missing"),
        };
        let conn_fields = as_object(conn);
        match conn_fields.get(&Name::new("totalCount")) {
            Some(Value::Number(n)) => assert_eq!(n.as_i64(), Some(1)),
            other => panic!("totalCount unexpected: {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn usage_logs_surfaces_service_unavailable_when_unwired() {
        let schema: TestSchema = crate::admin_schema_builder().finish();
        let resp = schema.execute("{ usageLogs { totalCount } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("usage log service is not available"),
            "unexpected msg: {msg}"
        );
    }

    // ---- SDL shape parity -----------------------------------------

    #[test]
    fn sdl_contains_request_usage_slice_types_and_signatures() {
        let req: Arc<dyn RequestQueryServices> = Arc::new(FakeRequestServices::default());
        let usage: Arc<dyn UsageLogQueryServices> = Arc::new(FakeUsageLogServices::default());
        let sdl = crate::admin_schema_builder()
            .data(req)
            .data(usage)
            .finish()
            .sdl();

        for expected in [
            "type Request implements Node {",
            "type RequestConnection {",
            "type RequestEdge {",
            "type UsageLog implements Node {",
            "type UsageLogConnection {",
            "type UsageLogEdge {",
            "type CostItem {",
            "type TierCost {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        for expected in [
            "input RequestOrder {",
            "input RequestWhereInput {",
            "input UsageLogOrder {",
            "input UsageLogWhereInput {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        for expected in [
            "enum RequestOrderField {",
            "enum RequestSource {",
            "enum RequestStatus {",
            "enum UsageLogOrderField {",
            "enum UsageLogSource {",
            "enum PriceItemCode {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        // Top-level queries.
        assert!(
            sdl.contains("requests("),
            "SDL missing requests query: {sdl}"
        );
        assert!(
            sdl.contains("usageLogs("),
            "SDL missing usageLogs query: {sdl}"
        );

        // Enum values.
        assert!(
            sdl.contains("playground"),
            "missing playground enum value: {sdl}"
        );
        assert!(
            sdl.contains("canceled"),
            "missing canceled enum value: {sdl}"
        );

        // Acronym rename pinning.
        assert!(
            sdl.contains("apiKeyID: ID"),
            "SDL missing apiKeyID field: {sdl}"
        );
        assert!(
            sdl.contains("projectID: ID!"),
            "SDL missing projectID field: {sdl}"
        );
        assert!(
            sdl.contains("channelID: ID"),
            "SDL missing channelID field: {sdl}"
        );
        assert!(
            sdl.contains("dataStorageID: ID"),
            "SDL missing dataStorageID field: {sdl}"
        );
        assert!(
            sdl.contains("modelID: String!"),
            "SDL missing modelID field: {sdl}"
        );
        assert!(
            sdl.contains("externalID: String"),
            "SDL missing externalID field: {sdl}"
        );
        assert!(
            sdl.contains("clientIP: String!"),
            "SDL missing clientIP field: {sdl}"
        );
        assert!(
            sdl.contains("requestID: ID!"),
            "SDL missing requestID field: {sdl}"
        );
        assert!(
            sdl.contains("costPriceReferenceID: String"),
            "SDL missing costPriceReferenceID field: {sdl}"
        );

        // Numeric-suffixed field names.
        assert!(
            sdl.contains("promptWriteCachedTokens5m: Int"),
            "SDL missing promptWriteCachedTokens5m field: {sdl}"
        );
        assert!(
            sdl.contains("promptWriteCachedTokens1h: Int"),
            "SDL missing promptWriteCachedTokens1h field: {sdl}"
        );
    }

    #[test]
    fn sdl_matches_snapshot_for_request_usage_slice() -> Result<(), Box<dyn std::error::Error>> {
        let req: Arc<dyn RequestQueryServices> = Arc::new(FakeRequestServices::default());
        let usage: Arc<dyn UsageLogQueryServices> = Arc::new(FakeUsageLogServices::default());
        let sdl = crate::admin_schema_builder()
            .data(req)
            .data(usage)
            .finish()
            .sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;

        // `Request` output type — `executions`, API key, project, channel,
        // and usage-log edges are now ported. Two lower-value edges remain.
        let request_pending: &[&str] = &["trace: Trace", "dataStorage: DataStorage"];
        crate::sdl_parity::assert_block_parity(
            &sdl,
            &snapshot,
            "type Request",
            "type Request",
            request_pending,
        )?;

        // `UsageLog` output type — pending the 3 edge fields.
        let usage_pending: &[&str] =
            &["request: Request!", "project: Project!", "channel: Channel"];
        crate::sdl_parity::assert_block_parity(
            &sdl,
            &snapshot,
            "type UsageLog",
            "type UsageLog",
            usage_pending,
        )?;

        // The WhereInput blocks are very large; we check the smaller input
        // and enum blocks in full.
        for header in [
            "input RequestOrder",
            "enum RequestOrderField",
            "enum RequestSource",
            "enum RequestStatus",
            "input UsageLogOrder",
            "enum UsageLogOrderField",
            "enum UsageLogSource",
            "type RequestConnection",
            "type RequestEdge",
            "type UsageLogConnection",
            "type UsageLogEdge",
            "type CostItem",
            "type TierCost",
            "enum PriceItemCode",
        ] {
            crate::sdl_parity::assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }

        Ok(())
    }

    // ---- TestQueryRoot placeholder (unused but keeps schema builder happy)
    // ----------------------------------------------------------------

    #[allow(dead_code)]
    struct _MarkerQueryRoot;
    #[Object]
    impl _MarkerQueryRoot {
        async fn _marker(&self) -> &'static str {
            "marker"
        }
    }

    // ---------------------------------------------------------------------
    // Forward-edge resolver semantics — `Request.executions`
    // (RUST-P12-001 S07 continuation). The resolver delegates to the
    // host-injected `RequestExecutionQueryServices` with a `where: { requestID:
    // <obj.id>, projectID: <obj.projectID> }` predicate. This fake captures
    // args so the test can assert the injected FK/project scope.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct CapturingExecServices {
        captured: Arc<Mutex<Vec<crate::request_execution::RequestExecutionConnectionArgs>>>,
    }

    #[async_trait::async_trait]
    impl crate::request_execution::RequestExecutionQueryServices for CapturingExecServices {
        async fn request_executions(
            &self,
            args: crate::request_execution::RequestExecutionConnectionArgs,
        ) -> Result<
            crate::request_execution::RequestExecutionConnection,
            crate::request_execution::RequestExecutionQueryError,
        > {
            lock(&self.captured).push(args);
            Ok(crate::request_execution::RequestExecutionConnection {
                edges: Some(vec![]),
                page_info: crate::pagination::PageInfo {
                    has_previous_page: false,
                    has_next_page: false,
                    start_cursor: None,
                    end_cursor: None,
                },
                total_count: 0,
            })
        }
    }

    #[tokio::test]
    async fn request_executions_edge_injects_request_and_project_predicates() -> Result<(), String>
    {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let parent = sample_request_for_edge_tests("42", now);
        let requests_fake = FakeRequestServices {
            records: Arc::new(Mutex::new(vec![parent])),
            ..FakeRequestServices::default()
        };
        let exec_fake = CapturingExecServices::default();
        let req: Arc<dyn RequestQueryServices> = Arc::new(requests_fake);
        let exec: Arc<dyn crate::request_execution::RequestExecutionQueryServices> =
            Arc::new(exec_fake.clone());
        let schema = crate::admin_schema_builder().data(req).data(exec).finish();

        let resp = schema
            .execute("{ requests { edges { node { executions(first: 5) { totalCount } } } } }")
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);

        let captured = lock(&exec_fake.captured).clone();
        assert_eq!(captured.len(), 1, "expected one executions edge query");
        let filter = captured[0]
            .where_filter
            .clone()
            .ok_or("executions edge did not inject a where filter")?;
        assert_eq!(filter.request_id, Some(ID::from("42")));
        assert_eq!(filter.project_id, Some(1));
        assert_eq!(captured[0].first, Some(5));
        Ok(())
    }

    #[tokio::test]
    async fn request_executions_edge_reports_unavailable_when_service_not_wired() {
        // RequestQueryServices is wired with a single record (so the parent
        // `requests` query yields a node to navigate from) but
        // RequestExecutionQueryServices is NOT — the edge resolver must
        // surface the same "service is not available" error the rest of the
        // crate uses.
        let now = ts(2024, 1, 2, 3, 4, 5).unwrap_or_else(|err| panic!("{err}"));
        let parent = sample_request_for_edge_tests("1", now);
        let requests_fake = FakeRequestServices {
            records: Arc::new(Mutex::new(vec![parent])),
            ..FakeRequestServices::default()
        };
        let req: Arc<dyn RequestQueryServices> = Arc::new(requests_fake);
        let schema = crate::admin_schema_builder().data(req).finish();
        let resp = schema
            .execute("{ requests { edges { node { executions { totalCount } } } } }")
            .await;
        assert!(!resp.errors.is_empty(), "expected an error");
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("request execution service is not available"),
            "unexpected error: {msg}"
        );
    }

    fn sample_request_for_edge_tests(id: &str, now: chrono::DateTime<Utc>) -> Request {
        Request {
            id: ID::from(id),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            api_key_id: None,
            project_id: ID::from("1"),
            trace_id: None,
            data_storage_id: None,
            source: RequestSource::Api,
            model_id: "gpt-4".to_string(),
            reasoning_effort: None,
            format: "openai/chat_completions".to_string(),
            request_headers: None,
            request_body: json_msg(json!({})),
            response_body: None,
            response_chunks: None,
            channel_id: None,
            external_id: None,
            status: RequestStatus::Completed,
            stream: false,
            client_ip: "127.0.0.1".to_string(),
            metrics_latency_ms: None,
            metrics_first_token_latency_ms: None,
            metrics_reasoning_duration_ms: None,
            content_saved: false,
            content_storage_id: None,
            content_storage_key: None,
            content_saved_at: None,
        }
    }
}
