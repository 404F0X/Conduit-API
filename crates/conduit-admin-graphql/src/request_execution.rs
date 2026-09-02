//! RUST-P12-001 S07 (continuation) — RequestExecution GraphQL domain slice.
//!
//! Ports the `RequestExecution` entity declared in
//! `conduit/internal/ent/schema/request_execution.go` and surfaced in the
//! GraphQL snapshot at `tests/contracts/admin_graphql_schema.graphql`
//! (lines 5904-6258) — generated from `conduit/internal/server/gql/ent.graphql`
//! (lines 4564-4917).
//!
//! ## Operations ported this slice
//!
//! Type / connection / edge / order / enum / where-input for the
//! `RequestExecution` entity. The type is reachable from three edge fields
//! (`Request.executions`, `Channel.executions`, `DataStorage.executions`) and
//! has no top-level Query in the snapshot — only edge access. This slice
//! supplies the type together with its connection envelope so that
//! `Request.executions` can be wired in a follow-up edge-resolver slice.
//!
//! ## Pending
//!
//!   - `RequestExecution.request: Request!` / `channel: Channel` /
//!     `dataStorage: DataStorage` — cross-domain edge fields. Pending the
//!     edge-resolver slice that wires each parent type's `<parent>.executions`
//!     connection and the back-reference resolvers on this type.
//!   - `RequestExecution.requestBody / responseBody / responseChunks` — Go
//!     marks these with `@goField(forceResolver: true)` (snapshot lines
//!     4578-4580); the resolver loads the bodies from the data storage. This
//!     slice surfaces them as plain `JsonRawMessageScalar` fields so the host
//!     can populate them when content fetching is wired (mirrors the
//!     `Request` body-field pattern established by `request_usage.rs`).
//!   - `Request.executions(...)` edge resolver — pending the edge-resolver
//!     slice (needs `Request` promoted to `#[graphql(complex)]`).
//!   - `Channel.executions(...)` / `DataStorage.executions(...)` edge
//!     resolvers — pending.
//!
//! ## Service wiring
//!
//! The admin-graphql crate stays free of DB/IO concerns. The host wires a
//! concrete implementation of [`RequestExecutionQueryServices`] into the
//! schema data bag; resolver-level tests inject in-memory fakes.

use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, InputObject, SimpleObject};

use crate::pagination::PageInfo;
use crate::scalars::{CursorScalar, JsonRawMessageScalar, TimeScalar};

// ===========================================================================
// Enums — snapshot lines 5996-6009.
// ===========================================================================

/// `enum RequestExecutionOrderField { CREATED_AT UPDATED_AT }` — snapshot
/// lines 5996-5999.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum RequestExecutionOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

/// `enum RequestExecutionStatus` — snapshot lines 6003-6009. Bound to Go
/// `ent/requestexecution.Status` (schema line 72:
/// `pending|processing|completed|failed|canceled`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum RequestExecutionStatus {
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

/// Re-export of the ent-global `OrderDirection` enum owned by
/// [`crate::channel`] (snapshot line 4072). Single canonical registration so
/// the SDL registry does not see two enums with the same GraphQL name.
pub use crate::channel::OrderDirection;

// ===========================================================================
// Output types — RequestExecution (snapshot lines 5904-5951, schema
// `ent/schema/request_execution.go` Fields()).
// ===========================================================================

/// `type RequestExecution implements Node` — snapshot lines 5904-5951. All
/// scalar fields plus the three force-resolver body fields (kept as plain
/// `JsonRawMessageScalar` so the host can populate them when content
/// fetching is wired, mirroring the `Request` body-field pattern). The
/// `request` (snapshot line 4606) and `channel` (line 4607) back-edges are
/// resolved by the [`ComplexObject`] impl below; `dataStorage` (line 4608)
/// remains pending.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
#[graphql(name = "RequestExecution")]
pub struct RequestExecution {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    /// GraphQL field `projectID` — non-null `Int` (snapshot line 5908; Go
    /// schema `field.Int("project_id")`). Acronym rename pin.
    #[graphql(name = "projectID")]
    pub project_id: i64,
    /// GraphQL field `requestID` — non-null ID. Acronym rename pin.
    #[graphql(name = "requestID")]
    pub request_id: ID,
    /// GraphQL field `channelID` — nullable ID. Acronym rename pin.
    #[graphql(name = "channelID")]
    pub channel_id: Option<ID>,
    /// GraphQL field `dataStorageID` — nullable ID. Acronym rename pin.
    #[graphql(name = "dataStorageID")]
    pub data_storage_id: Option<ID>,
    /// GraphQL field `externalID` — nullable String. Acronym rename pin.
    #[graphql(name = "externalID")]
    pub external_id: Option<String>,
    /// GraphQL field `modelID` — non-null String. Acronym rename pin.
    #[graphql(name = "modelID")]
    pub model_id: String,
    pub format: String,
    /// Snapshot line 5918: `requestBody: JSONRawMessage! @goField
    /// (forceResolver: true)`. Force-resolver in Go; we keep the field
    /// non-null and surface the raw JSON carried by the host (the loader
    /// service is wired in a separate slice).
    pub request_body: JsonRawMessageScalar,
    pub response_body: Option<JsonRawMessageScalar>,
    /// Snapshot line 5920: `responseChunks: [JSONRawMessage!]` (nullable
    /// list of non-null JSON values).
    pub response_chunks: Option<Vec<JsonRawMessageScalar>>,
    pub error_message: Option<String>,
    /// Snapshot line 5924: `responseStatusCode: Int` — nullable Int (HTTP
    /// status from the upstream provider).
    pub response_status_code: Option<i64>,
    pub status: RequestExecutionStatus,
    pub stream: bool,
    /// Snapshot line 5928: nullable Int. Schema field name
    /// `metrics_latency_ms` (Go `field.Int64`).
    pub metrics_latency_ms: Option<i64>,
    pub metrics_first_token_latency_ms: Option<i64>,
    pub metrics_reasoning_duration_ms: Option<i64>,
    pub request_headers: Option<JsonRawMessageScalar>,
    /// Snapshot line 5938: nullable String; actual upstream URL sent to the
    /// provider. Acronym rename pin — async-graphql's default camelCase
    /// would emit `requestUrl`, but the snapshot uses `requestURL`.
    #[graphql(name = "requestURL")]
    pub request_url: Option<String>,
    /// Snapshot line 5942: non-null Boolean (Go `field.Bool` with
    /// `Default(false)`).
    pub pass_through_applied: bool,
}

// ===========================================================================
// RequestExecution back-edge resolvers (RUST-P12-001 S07 continuation).
//
// Go source: `conduit/internal/server/gql/generated.go` lines 45453-45666
// auto-resolves the three back-edges by calling ent's `obj.Request(ctx)` /
// `obj.DataStorage(ctx)` / `ec.resolvers.RequestExecution().Channel(ctx, obj)`
// (the `.channel` field carries `@goField(forceResolver: true)` in
// `ent.graphql:4607`, so it has an explicit resolver in
// `ent.resolvers.go:726-728` that calls `getNilableChannel(ctx, r.client,
// obj.ChannelID)`).
//
// This slice ports `request` (non-null `Request!`) and `channel` (nullable
// `Channel`); `dataStorage` stays pending (lower value — DataStorage is
// rarely navigated from a single execution).
//
// The admin-graphql crate stays free of DB/IO concerns, so each resolver
// delegates to the parent entity's host-injected query service, injecting a
// `where: { id: <fk> }` predicate that mirrors ent's implicit M2O edge
// filter (`request_executions.request_id` / `.channel_id`), and returns the
// first edge's node. The host's connection machinery already paginates, so
// we pass `first: Some(1)` to short-circuit further fetches.
// ===========================================================================

#[ComplexObject]
impl RequestExecution {
    /// `RequestExecution.request: Request!` — snapshot line 4606; Go
    /// `requestExecutionResolver` auto-resolved via `obj.Request(ctx)`
    /// (`generated.go:45453-45467`). FK: `request_executions.request_id`
    /// (`ent/request/request.go:118-123`: `ExecutionsTable` /
    /// `ExecutionsColumn = "request_id"`).
    ///
    /// Non-null in the snapshot: if the host cannot find the parent request
    /// we surface the same Go-style error (the Go path would surface the ent
    /// `NotFound` error).
    async fn request(&self, ctx: &Context<'_>) -> Result<crate::request_usage::Request, String> {
        let services = crate::request_usage::request_query_services(ctx)?;
        let injected = crate::request_usage::RequestWhereInput {
            id: Some(self.request_id.clone()),
            ..Default::default()
        };
        let args = crate::request_usage::RequestConnectionArgs {
            access: crate::request_usage::request_read_access_scope(ctx)?,
            after: None,
            first: Some(1),
            before: None,
            last: None,
            order_by: None,
            where_filter: Some(injected),
        };
        let conn = services
            .requests(args)
            .await
            .map_err(|err| err.to_string())?;
        let edges = conn.edges.unwrap_or_default();
        let edge = edges
            .into_iter()
            .flatten()
            .next()
            .ok_or_else(|| "request execution's parent request was not found".to_string())?;
        edge.node
            .ok_or_else(|| "request execution's parent request was not found".to_string())
    }

    /// `RequestExecution.channel: Channel` — snapshot line 4607; Go
    /// `@goField(forceResolver: true)`, explicit resolver at
    /// `ent.resolvers.go:726-728` calling
    /// `getNilableChannel(ctx, r.client, obj.ChannelID)`. FK:
    /// `request_executions.channel_id` (`ent/channel/channel.go:86-91`:
    /// `ExecutionsTable = "request_executions"`, `ExecutionsColumn =
    /// "channel_id"`).
    ///
    /// Nullable in the snapshot: returns `None` when `channelID` is absent
    /// or when the host has no matching channel.
    async fn channel(&self, ctx: &Context<'_>) -> Result<Option<crate::channel::Channel>, String> {
        let channel_id = match self.channel_id.clone() {
            Some(id) => id,
            None => return Ok(None),
        };
        crate::policy::authorize_current(ctx, conduit_auth::scopes::slug::READ_CHANNELS)
            .map_err(|error| error.to_string())?;
        let services = crate::channel::channel_query_services(ctx)?;
        let injected = crate::channel::ChannelWhereInput {
            id: Some(channel_id),
            ..Default::default()
        };
        let args = crate::channel::ChannelConnectionArgs {
            after: None,
            first: Some(1),
            before: None,
            last: None,
            order_by: None,
            where_filter: Some(injected),
        };
        let conn = services
            .channels(args)
            .await
            .map_err(|err| err.to_string())?;
        let edges = conn.edges.unwrap_or_default();
        let Some(edge) = edges.into_iter().flatten().next() else {
            return Ok(None);
        };
        Ok(edge.node)
    }
}

/// `type RequestExecutionEdge` — snapshot lines 5970-5976. `node` is
/// nullable in the snapshot (`node: RequestExecution`, no `!`).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct RequestExecutionEdge {
    pub node: Option<RequestExecution>,
    pub cursor: CursorScalar,
}

/// `type RequestExecutionConnection` — snapshot lines 5953-5966.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct RequestExecutionConnection {
    pub edges: Option<Vec<Option<RequestExecutionEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

/// `input RequestExecutionOrder` — snapshot lines 5983-5992. `direction:
/// OrderDirection! = ASC` — non-null with default ASC (ent-global pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "RequestExecutionOrder")]
pub struct RequestExecutionOrder {
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: RequestExecutionOrderField,
}

/// `input RequestExecutionWhereInput` — snapshot lines 6014-6258
/// (ent-generated predicate grammar; Go source
/// `internal/ent/requestexecution/where.go`). Implemented: `not`/`and`/`or`,
/// every scalar-field predicate family for the ported scalar fields, plus
/// the `status` enum predicates. Edge predicates (`hasRequest`,
/// `hasRequestWith`, `hasChannel`/`hasChannelWith`, `hasDataStorage`/
/// `hasDataStorageWith`) reference other entities' WhereInputs and are
/// pending the edge-resolver slice.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct RequestExecutionWhereInput {
    pub not: Option<Box<RequestExecutionWhereInput>>,
    pub and: Option<Vec<RequestExecutionWhereInput>>,
    pub or: Option<Vec<RequestExecutionWhereInput>>,
    // id field predicates (snapshot lines 6021-6028)
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
    // created_at field predicates (snapshot lines 6032-6039)
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
    // updated_at field predicates (snapshot lines 6043-6050)
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
    // project_id field predicates (snapshot lines 6054-6061) — Int (not ID)
    #[graphql(name = "projectID")]
    pub project_id: Option<i64>,
    #[graphql(name = "projectIDNEQ")]
    pub project_id_neq: Option<i64>,
    #[graphql(name = "projectIDIn")]
    pub project_id_in: Option<Vec<i64>>,
    #[graphql(name = "projectIDNotIn")]
    pub project_id_not_in: Option<Vec<i64>>,
    #[graphql(name = "projectIDGT")]
    pub project_id_gt: Option<i64>,
    #[graphql(name = "projectIDGTE")]
    pub project_id_gte: Option<i64>,
    #[graphql(name = "projectIDLT")]
    pub project_id_lt: Option<i64>,
    #[graphql(name = "projectIDLTE")]
    pub project_id_lte: Option<i64>,
    // request_id field predicates (snapshot lines 6065-6068) — ID, eq/neq/in/not_in only
    #[graphql(name = "requestID")]
    pub request_id: Option<ID>,
    #[graphql(name = "requestIDNEQ")]
    pub request_id_neq: Option<ID>,
    #[graphql(name = "requestIDIn")]
    pub request_id_in: Option<Vec<ID>>,
    #[graphql(name = "requestIDNotIn")]
    pub request_id_not_in: Option<Vec<ID>>,
    // channel_id field predicates (snapshot lines 6072-6077)
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
    // data_storage_id field predicates (snapshot lines 6081-6086)
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
    // external_id field predicates (snapshot lines 6090-6104)
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
    // model_id field predicates (snapshot lines 6108-6120)
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
    // format field predicates (snapshot lines 6124-6136)
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
    // error_message field predicates (snapshot lines 6140-6154)
    pub error_message: Option<String>,
    #[graphql(name = "errorMessageNEQ")]
    pub error_message_neq: Option<String>,
    #[graphql(name = "errorMessageIn")]
    pub error_message_in: Option<Vec<String>>,
    #[graphql(name = "errorMessageNotIn")]
    pub error_message_not_in: Option<Vec<String>>,
    #[graphql(name = "errorMessageGT")]
    pub error_message_gt: Option<String>,
    #[graphql(name = "errorMessageGTE")]
    pub error_message_gte: Option<String>,
    #[graphql(name = "errorMessageLT")]
    pub error_message_lt: Option<String>,
    #[graphql(name = "errorMessageLTE")]
    pub error_message_lte: Option<String>,
    pub error_message_contains: Option<String>,
    #[graphql(name = "errorMessageHasPrefix")]
    pub error_message_has_prefix: Option<String>,
    #[graphql(name = "errorMessageHasSuffix")]
    pub error_message_has_suffix: Option<String>,
    #[graphql(name = "errorMessageIsNil")]
    pub error_message_is_nil: Option<bool>,
    #[graphql(name = "errorMessageNotNil")]
    pub error_message_not_nil: Option<bool>,
    #[graphql(name = "errorMessageEqualFold")]
    pub error_message_equal_fold: Option<String>,
    #[graphql(name = "errorMessageContainsFold")]
    pub error_message_contains_fold: Option<String>,
    // response_status_code field predicates (snapshot lines 6158-6167)
    pub response_status_code: Option<i64>,
    #[graphql(name = "responseStatusCodeNEQ")]
    pub response_status_code_neq: Option<i64>,
    #[graphql(name = "responseStatusCodeIn")]
    pub response_status_code_in: Option<Vec<i64>>,
    #[graphql(name = "responseStatusCodeNotIn")]
    pub response_status_code_not_in: Option<Vec<i64>>,
    #[graphql(name = "responseStatusCodeGT")]
    pub response_status_code_gt: Option<i64>,
    #[graphql(name = "responseStatusCodeGTE")]
    pub response_status_code_gte: Option<i64>,
    #[graphql(name = "responseStatusCodeLT")]
    pub response_status_code_lt: Option<i64>,
    #[graphql(name = "responseStatusCodeLTE")]
    pub response_status_code_lte: Option<i64>,
    #[graphql(name = "responseStatusCodeIsNil")]
    pub response_status_code_is_nil: Option<bool>,
    #[graphql(name = "responseStatusCodeNotNil")]
    pub response_status_code_not_nil: Option<bool>,
    // status field predicates (snapshot lines 6171-6174)
    pub status: Option<RequestExecutionStatus>,
    #[graphql(name = "statusNEQ")]
    pub status_neq: Option<RequestExecutionStatus>,
    #[graphql(name = "statusIn")]
    pub status_in: Option<Vec<RequestExecutionStatus>>,
    #[graphql(name = "statusNotIn")]
    pub status_not_in: Option<Vec<RequestExecutionStatus>>,
    // stream field predicates (snapshot lines 6178-6179)
    pub stream: Option<bool>,
    #[graphql(name = "streamNEQ")]
    pub stream_neq: Option<bool>,
    // metrics_latency_ms field predicates (snapshot lines 6183-6192)
    pub metrics_latency_ms: Option<i64>,
    #[graphql(name = "metricsLatencyMsNEQ")]
    pub metrics_latency_ms_neq: Option<i64>,
    #[graphql(name = "metricsLatencyMsIn")]
    pub metrics_latency_ms_in: Option<Vec<i64>>,
    #[graphql(name = "metricsLatencyMsNotIn")]
    pub metrics_latency_ms_not_in: Option<Vec<i64>>,
    #[graphql(name = "metricsLatencyMsGT")]
    pub metrics_latency_ms_gt: Option<i64>,
    #[graphql(name = "metricsLatencyMsGTE")]
    pub metrics_latency_ms_gte: Option<i64>,
    #[graphql(name = "metricsLatencyMsLT")]
    pub metrics_latency_ms_lt: Option<i64>,
    #[graphql(name = "metricsLatencyMsLTE")]
    pub metrics_latency_ms_lte: Option<i64>,
    #[graphql(name = "metricsLatencyMsIsNil")]
    pub metrics_latency_ms_is_nil: Option<bool>,
    #[graphql(name = "metricsLatencyMsNotNil")]
    pub metrics_latency_ms_not_nil: Option<bool>,
    // metrics_first_token_latency_ms field predicates (snapshot lines 6196-6205)
    pub metrics_first_token_latency_ms: Option<i64>,
    #[graphql(name = "metricsFirstTokenLatencyMsNEQ")]
    pub metrics_first_token_latency_ms_neq: Option<i64>,
    #[graphql(name = "metricsFirstTokenLatencyMsIn")]
    pub metrics_first_token_latency_ms_in: Option<Vec<i64>>,
    #[graphql(name = "metricsFirstTokenLatencyMsNotIn")]
    pub metrics_first_token_latency_ms_not_in: Option<Vec<i64>>,
    #[graphql(name = "metricsFirstTokenLatencyMsGT")]
    pub metrics_first_token_latency_ms_gt: Option<i64>,
    #[graphql(name = "metricsFirstTokenLatencyMsGTE")]
    pub metrics_first_token_latency_ms_gte: Option<i64>,
    #[graphql(name = "metricsFirstTokenLatencyMsLT")]
    pub metrics_first_token_latency_ms_lt: Option<i64>,
    #[graphql(name = "metricsFirstTokenLatencyMsLTE")]
    pub metrics_first_token_latency_ms_lte: Option<i64>,
    #[graphql(name = "metricsFirstTokenLatencyMsIsNil")]
    pub metrics_first_token_latency_ms_is_nil: Option<bool>,
    #[graphql(name = "metricsFirstTokenLatencyMsNotNil")]
    pub metrics_first_token_latency_ms_not_nil: Option<bool>,
    // metrics_reasoning_duration_ms field predicates (snapshot lines 6209-6218)
    pub metrics_reasoning_duration_ms: Option<i64>,
    #[graphql(name = "metricsReasoningDurationMsNEQ")]
    pub metrics_reasoning_duration_ms_neq: Option<i64>,
    #[graphql(name = "metricsReasoningDurationMsIn")]
    pub metrics_reasoning_duration_ms_in: Option<Vec<i64>>,
    #[graphql(name = "metricsReasoningDurationMsNotIn")]
    pub metrics_reasoning_duration_ms_not_in: Option<Vec<i64>>,
    #[graphql(name = "metricsReasoningDurationMsGT")]
    pub metrics_reasoning_duration_ms_gt: Option<i64>,
    #[graphql(name = "metricsReasoningDurationMsGTE")]
    pub metrics_reasoning_duration_ms_gte: Option<i64>,
    #[graphql(name = "metricsReasoningDurationMsLT")]
    pub metrics_reasoning_duration_ms_lt: Option<i64>,
    #[graphql(name = "metricsReasoningDurationMsLTE")]
    pub metrics_reasoning_duration_ms_lte: Option<i64>,
    #[graphql(name = "metricsReasoningDurationMsIsNil")]
    pub metrics_reasoning_duration_ms_is_nil: Option<bool>,
    #[graphql(name = "metricsReasoningDurationMsNotNil")]
    pub metrics_reasoning_duration_ms_not_nil: Option<bool>,
    // request_url field predicates (snapshot lines 6222-6237)
    pub request_url: Option<String>,
    #[graphql(name = "requestURLNEQ")]
    pub request_url_neq: Option<String>,
    #[graphql(name = "requestURLIn")]
    pub request_url_in: Option<Vec<String>>,
    #[graphql(name = "requestURLNotIn")]
    pub request_url_not_in: Option<Vec<String>>,
    #[graphql(name = "requestURLGT")]
    pub request_url_gt: Option<String>,
    #[graphql(name = "requestURLGTE")]
    pub request_url_gte: Option<String>,
    #[graphql(name = "requestURLLT")]
    pub request_url_lt: Option<String>,
    #[graphql(name = "requestURLLTE")]
    pub request_url_lte: Option<String>,
    pub request_url_contains: Option<String>,
    #[graphql(name = "requestURLHasPrefix")]
    pub request_url_has_prefix: Option<String>,
    #[graphql(name = "requestURLHasSuffix")]
    pub request_url_has_suffix: Option<String>,
    #[graphql(name = "requestURLIsNil")]
    pub request_url_is_nil: Option<bool>,
    #[graphql(name = "requestURLNotNil")]
    pub request_url_not_nil: Option<bool>,
    #[graphql(name = "requestURLEqualFold")]
    pub request_url_equal_fold: Option<String>,
    #[graphql(name = "requestURLContainsFold")]
    pub request_url_contains_fold: Option<String>,
    // pass_through_applied field predicates (snapshot lines 6241-6242)
    pub pass_through_applied: Option<bool>,
    #[graphql(name = "passThroughAppliedNEQ")]
    pub pass_through_applied_neq: Option<bool>,
}

// ===========================================================================
// Order selection lowering (mirrors `resolve_channel_order` in channel.rs).
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestExecutionOrderTerm {
    /// `CREATED_AT` ordering is remapped to the ent default ID order, with
    /// the requested direction preserved (gql_pagination.go:413 — same
    /// convention used by every other ent-generated connection in this
    /// crate).
    Id,
    UpdatedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestExecutionOrderSelection {
    pub direction: OrderDirection,
    pub term: RequestExecutionOrderTerm,
}

pub fn resolve_request_execution_order(
    order_by: Option<RequestExecutionOrder>,
) -> Option<RequestExecutionOrderSelection> {
    order_by.map(|order| RequestExecutionOrderSelection {
        direction: order.direction,
        term: match order.field {
            RequestExecutionOrderField::CreatedAt => RequestExecutionOrderTerm::Id,
            RequestExecutionOrderField::UpdatedAt => RequestExecutionOrderTerm::UpdatedAt,
        },
    })
}

// ===========================================================================
// Service traits (host-injected) — used by the future `Request.executions`
// edge resolver (and Channel/DataStorage equivalents) to delegate
// pagination + filtering to the host (ent in Go).
// ===========================================================================

#[derive(Debug, Clone, thiserror::Error)]
pub enum RequestExecutionQueryError {
    #[error("request execution service is not available")]
    ServiceUnavailable,
    #[error("failed to query request executions: {0}")]
    Query(String),
}

#[derive(Debug, Clone, Default)]
pub struct RequestExecutionConnectionArgs {
    pub access: crate::policy::AdminAccessScope,
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<RequestExecutionOrderSelection>,
    pub where_filter: Option<RequestExecutionWhereInput>,
}

#[async_trait::async_trait]
pub trait RequestExecutionQueryServices: Send + Sync {
    async fn request_executions(
        &self,
        args: RequestExecutionConnectionArgs,
    ) -> Result<RequestExecutionConnection, RequestExecutionQueryError>;
}

/// Resolver-side helper for the future `Request.executions` /
/// `Channel.executions` / `DataStorage.executions` edge resolvers. Exposed
/// to the rest of the crate (and to tests) but currently without a
/// production caller — `#[allow(dead_code)]` keeps the lint quiet until the
/// edge-resolver slice lands.
#[allow(dead_code)]
pub(crate) fn request_execution_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn RequestExecutionQueryServices>, String> {
    match ctx.data::<Arc<dyn RequestExecutionQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(RequestExecutionQueryError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::{EmptySubscription, Name, Object, Schema, Value};
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;
    use crate::mutation::MutationRoot;
    use crate::scalars::JsonRawMessageScalar;

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

    #[cfg(test)]
    pub(crate) fn json_msg(value: serde_json::Value) -> JsonRawMessageScalar {
        JsonRawMessageScalar(value)
    }

    // ---------------------------------------------------------------------
    // In-memory fake service. Exposed via a tiny Query field so the SDL
    // registry picks up every connection / edge / where-input type even
    // before the `Request.executions` edge resolver lands.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct FakeRequestExecutionServices {
        records: Arc<Mutex<Vec<RequestExecution>>>,
        query_error: Option<RequestExecutionQueryError>,
    }

    #[async_trait::async_trait]
    impl RequestExecutionQueryServices for FakeRequestExecutionServices {
        async fn request_executions(
            &self,
            args: RequestExecutionConnectionArgs,
        ) -> Result<RequestExecutionConnection, RequestExecutionQueryError> {
            match &self.query_error {
                Some(err) => Err(err.clone()),
                None => {
                    let records = lock(&self.records).clone();
                    let filtered: Vec<RequestExecution> =
                        match args.where_filter.as_ref().and_then(|w| w.status) {
                            Some(status) => {
                                records.into_iter().filter(|r| r.status == status).collect()
                            }
                            None => records,
                        };
                    let total = filtered.len() as i64;
                    Ok(RequestExecutionConnection {
                        edges: Some(
                            filtered
                                .into_iter()
                                .map(|node| {
                                    Some(RequestExecutionEdge {
                                        cursor: CursorScalar(format!("exec:{}", node.id.as_str())),
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

    /// Test-only Query root that exposes a top-level `requestExecutions`
    /// field so we can exercise the connection resolver end-to-end before
    /// the production `Request.executions` edge resolver lands.
    #[derive(Default)]
    struct TestExecutionsQueryRoot;
    #[Object]
    impl TestExecutionsQueryRoot {
        async fn health(&self) -> &'static str {
            "ok"
        }

        #[allow(clippy::too_many_arguments)]
        async fn request_executions(
            &self,
            ctx: &Context<'_>,
            after: Option<CursorScalar>,
            first: Option<i32>,
            before: Option<CursorScalar>,
            last: Option<i32>,
            order_by: Option<RequestExecutionOrder>,
            #[graphql(name = "where")] where_filter: Option<RequestExecutionWhereInput>,
        ) -> Result<RequestExecutionConnection, String> {
            let services = request_execution_query_services(ctx)?;
            let args = RequestExecutionConnectionArgs {
                access: crate::policy::AdminAccessScope::Global,
                after: after.map(|cursor| cursor.0),
                first,
                before: before.map(|cursor| cursor.0),
                last,
                order_by: resolve_request_execution_order(order_by),
                where_filter,
            };
            services
                .request_executions(args)
                .await
                .map_err(|err| err.to_string())
        }
    }

    type TestSchema = Schema<TestExecutionsQueryRoot, MutationRoot, EmptySubscription>;

    fn test_schema(services: FakeRequestExecutionServices) -> TestSchema {
        let arc: Arc<dyn RequestExecutionQueryServices> = Arc::new(services);
        Schema::build(TestExecutionsQueryRoot, MutationRoot, EmptySubscription)
            .data(arc)
            .finish()
    }

    fn sample_execution(now: chrono::DateTime<Utc>) -> RequestExecution {
        RequestExecution {
            id: ID::from("1"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            project_id: 1,
            request_id: ID::from("r-1"),
            channel_id: Some(ID::from("ch-1")),
            data_storage_id: None,
            external_id: None,
            model_id: "gpt-4".to_string(),
            format: "openai/chat_completions".to_string(),
            request_body: json_msg(json!({"prompt": "hi"})),
            response_body: None,
            response_chunks: None,
            error_message: None,
            response_status_code: Some(200),
            status: RequestExecutionStatus::Completed,
            stream: false,
            metrics_latency_ms: Some(123),
            metrics_first_token_latency_ms: None,
            metrics_reasoning_duration_ms: None,
            request_headers: None,
            request_url: Some("https://api.openai.com/v1/chat/completions".to_string()),
            pass_through_applied: false,
        }
    }

    // ---- order-lowering semantics -----------------------------------

    #[test]
    fn resolve_order_remaps_created_at_to_default_id_order() {
        let order = Some(RequestExecutionOrder {
            direction: OrderDirection::Desc,
            field: RequestExecutionOrderField::CreatedAt,
        });
        match resolve_request_execution_order(order) {
            Some(RequestExecutionOrderSelection { direction, term }) => {
                assert_eq!(direction, OrderDirection::Desc);
                assert_eq!(term, RequestExecutionOrderTerm::Id);
            }
            None => panic!("expected Some selection"),
        }
    }

    #[test]
    fn resolve_order_passes_updated_at_through() {
        let order = Some(RequestExecutionOrder {
            direction: OrderDirection::Asc,
            field: RequestExecutionOrderField::UpdatedAt,
        });
        match resolve_request_execution_order(order) {
            Some(RequestExecutionOrderSelection { direction, term }) => {
                assert_eq!(direction, OrderDirection::Asc);
                assert_eq!(term, RequestExecutionOrderTerm::UpdatedAt);
            }
            None => panic!("expected Some selection"),
        }
    }

    // ---- resolver: requestExecutions query --------------------------

    #[tokio::test]
    async fn request_executions_returns_connection_shape() -> Result<(), String> {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let exec = sample_execution(now);
        let fake = FakeRequestExecutionServices {
            records: Arc::new(Mutex::new(vec![exec])),
            ..FakeRequestExecutionServices::default()
        };
        let schema = test_schema(fake);

        let resp = schema
            .execute(
                "{ requestExecutions { totalCount edges { node { id modelID status stream \
                 requestID channelID projectID responseStatusCode metricsLatencyMs \
                 requestURL passThroughApplied requestBody } } } }",
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let conn = match obj.get(&Name::new("requestExecutions")) {
            Some(v) => v,
            None => panic!("requestExecutions missing"),
        };
        let conn_fields = as_object(conn);
        match conn_fields.get(&Name::new("totalCount")) {
            Some(Value::Number(n)) => assert_eq!(n.as_i64(), Some(1)),
            other => panic!("totalCount unexpected: {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn request_executions_applies_status_where_filter() -> Result<(), String> {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let done = sample_execution(now);
        let mut pending = sample_execution(now);
        pending.id = ID::from("2");
        pending.status = RequestExecutionStatus::Pending;
        let fake = FakeRequestExecutionServices {
            records: Arc::new(Mutex::new(vec![done, pending])),
            ..FakeRequestExecutionServices::default()
        };
        let schema = test_schema(fake);

        let resp = schema
            .execute("{ requestExecutions(where: {status: completed}) { totalCount } }")
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let conn = as_object(match obj.get(&Name::new("requestExecutions")) {
            Some(v) => v,
            None => panic!("requestExecutions missing"),
        });
        match conn.get(&Name::new("totalCount")) {
            Some(Value::Number(n)) => assert_eq!(n.as_i64(), Some(1)),
            other => panic!("totalCount unexpected: {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn request_executions_surfaces_query_error() {
        let fake = FakeRequestExecutionServices {
            query_error: Some(RequestExecutionQueryError::Query("db offline".to_string())),
            ..FakeRequestExecutionServices::default()
        };
        let schema = test_schema(fake);
        let resp = schema.execute("{ requestExecutions { totalCount } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to query request executions"),
            "msg: {msg}"
        );
        assert!(msg.contains("db offline"), "msg: {msg}");
    }

    // ---- SDL shape parity -----------------------------------------

    #[test]
    fn sdl_contains_request_execution_slice_types_and_signatures() {
        let fake: Arc<dyn RequestExecutionQueryServices> =
            Arc::new(FakeRequestExecutionServices::default());
        let sdl = Schema::build(TestExecutionsQueryRoot, MutationRoot, EmptySubscription)
            .data(fake)
            .finish()
            .sdl();

        for expected in [
            "type RequestExecution {",
            "type RequestExecutionConnection {",
            "type RequestExecutionEdge {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        for expected in [
            "input RequestExecutionOrder {",
            "input RequestExecutionWhereInput {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        for expected in [
            "enum RequestExecutionOrderField {",
            "enum RequestExecutionStatus {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        // Enum values.
        for expected in ["pending", "processing", "completed", "failed", "canceled"] {
            assert!(
                sdl.contains(expected),
                "missing status enum value {expected}: {sdl}"
            );
        }

        // Acronym rename pinning.
        for expected in [
            "projectID: Int!",
            "requestID: ID!",
            "channelID: ID",
            "dataStorageID: ID",
            "modelID: String!",
            "externalID: String",
            "requestURL: String",
        ] {
            assert!(
                sdl.contains(expected),
                "SDL missing acronym-renamed field `{expected}`: {sdl}"
            );
        }

        // Boolean flag with camel-cased field.
        assert!(
            sdl.contains("passThroughApplied: Boolean!"),
            "missing passThroughApplied: {sdl}"
        );

        // Where-input acronym fields.
        for expected in [
            "requestIDNEQ",
            "channelIDIsNil",
            "dataStorageIDNotNil",
            "externalIDContainsFold",
            "modelIDHasPrefix",
            "errorMessageEqualFold",
            "responseStatusCodeGTE",
            "metricsLatencyMsLTE",
            "metricsFirstTokenLatencyMsGT",
            "metricsReasoningDurationMsIn",
            "requestURLHasSuffix",
            "passThroughAppliedNEQ",
        ] {
            assert!(
                sdl.contains(expected),
                "SDL missing WhereInput field `{expected}`: {sdl}"
            );
        }
    }

    #[test]
    fn sdl_matches_snapshot_for_request_execution_slice() -> Result<(), Box<dyn std::error::Error>>
    {
        let fake: Arc<dyn RequestExecutionQueryServices> =
            Arc::new(FakeRequestExecutionServices::default());
        let sdl = Schema::build(TestExecutionsQueryRoot, MutationRoot, EmptySubscription)
            .data(fake)
            .finish()
            .sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;

        // `RequestExecution` output type — `request` and `channel` were
        // ported in this slice; `dataStorage` remains pending.
        let pending: &[&str] = &["dataStorage: DataStorage"];
        crate::sdl_parity::assert_block_parity(
            &sdl,
            &snapshot,
            "type RequestExecution",
            "type RequestExecution",
            pending,
        )?;

        // Connection / edge / order / enum blocks are small; check them in full.
        for header in [
            "type RequestExecutionConnection",
            "type RequestExecutionEdge",
            "input RequestExecutionOrder",
            "enum RequestExecutionOrderField",
            "enum RequestExecutionStatus",
        ] {
            crate::sdl_parity::assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }

        Ok(())
    }

    // ---------------------------------------------------------------------
    // Back-edge resolver semantics — `RequestExecution.request` and
    // `RequestExecution.channel` (RUST-P12-001 S07 continuation).
    //
    // The resolver delegates to the host-injected parent query service
    // (`crate::request_usage::RequestQueryServices` /
    // `crate::channel::ChannelQueryServices`) with a `where: { id: <fk> }`
    // predicate. These tests inject minimal in-memory fakes that capture the
    // args and return a single record, then exercise the resolver end-to-end
    // via the test-only `requestExecutions` top-level query.
    // ---------------------------------------------------------------------

    /// Minimal `RequestQueryServices` fake that returns a single hard-coded
    /// `Request` whose ID matches whatever `where.id` predicate was injected.
    #[derive(Default, Clone)]
    struct FakeRequestServices {
        captured: Arc<Mutex<Vec<crate::request_usage::RequestConnectionArgs>>>,
        request: Option<crate::request_usage::Request>,
    }

    #[async_trait::async_trait]
    impl crate::request_usage::RequestQueryServices for FakeRequestServices {
        async fn requests(
            &self,
            args: crate::request_usage::RequestConnectionArgs,
        ) -> Result<crate::request_usage::RequestConnection, crate::request_usage::RequestQueryError>
        {
            lock(&self.captured).push(args.clone());
            let node = self.request.clone();
            Ok(crate::request_usage::RequestConnection {
                edges: Some(vec![node.map(|n| crate::request_usage::RequestEdge {
                    node: Some(n),
                    cursor: CursorScalar("rq".to_string()),
                })]),
                page_info: PageInfo {
                    has_previous_page: false,
                    has_next_page: false,
                    start_cursor: None,
                    end_cursor: None,
                },
                total_count: if self.request.is_some() { 1 } else { 0 },
            })
        }
    }

    /// Minimal `ChannelQueryServices` fake that returns a single hard-coded
    /// `Channel` whose ID matches whatever `where.id` predicate was injected.
    #[derive(Default, Clone)]
    struct FakeChannelServices {
        captured: Arc<Mutex<Vec<crate::channel::ChannelConnectionArgs>>>,
        channel: Option<crate::channel::Channel>,
    }

    #[async_trait::async_trait]
    impl crate::channel::ChannelQueryServices for FakeChannelServices {
        async fn channels(
            &self,
            args: crate::channel::ChannelConnectionArgs,
        ) -> Result<crate::channel::ChannelConnection, crate::channel::ChannelServiceError>
        {
            lock(&self.captured).push(args.clone());
            let node = self.channel.clone();
            Ok(crate::channel::ChannelConnection {
                edges: Some(vec![node.map(|n| crate::channel::ChannelEdge {
                    node: Some(n),
                    cursor: CursorScalar("ch".to_string()),
                })]),
                page_info: PageInfo {
                    has_previous_page: false,
                    has_next_page: false,
                    start_cursor: None,
                    end_cursor: None,
                },
                total_count: if self.channel.is_some() { 1 } else { 0 },
            })
        }
    }

    fn back_edge_schema(
        exec_services: FakeRequestExecutionServices,
        req_services: FakeRequestServices,
        chan_services: FakeChannelServices,
    ) -> TestSchema {
        let exec: Arc<dyn RequestExecutionQueryServices> = Arc::new(exec_services);
        let req: Arc<dyn crate::request_usage::RequestQueryServices> = Arc::new(req_services);
        let chan: Arc<dyn crate::channel::ChannelQueryServices> = Arc::new(chan_services);
        Schema::build(TestExecutionsQueryRoot, MutationRoot, EmptySubscription)
            .data(exec)
            .data(req)
            .data(chan)
            .data(read_requests_context())
            .finish()
    }

    fn read_requests_context() -> conduit_auth::RequestContext {
        let mut context = conduit_auth::RequestContext::new();
        let _ = context.set_principal(
            conduit_auth::Principal::user("request-execution-test")
                .with_scope(conduit_auth::scopes::slug::READ_REQUESTS)
                .with_scope(conduit_auth::scopes::slug::READ_CHANNELS),
        );
        context
    }

    fn sample_request(id: &str) -> crate::request_usage::Request {
        let now = ts(2024, 1, 2, 3, 4, 5).unwrap_or_else(|err| panic!("{err}"));
        crate::request_usage::Request {
            id: ID::from(id),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            api_key_id: None,
            project_id: ID::from("1"),
            trace_id: None,
            data_storage_id: None,
            source: crate::request_usage::RequestSource::Api,
            model_id: "gpt-4".to_string(),
            reasoning_effort: None,
            format: "openai/chat_completions".to_string(),
            request_headers: None,
            request_body: json_msg(json!({"prompt": "hi"})),
            response_body: None,
            response_chunks: None,
            channel_id: None,
            external_id: None,
            status: crate::request_usage::RequestStatus::Completed,
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

    #[tokio::test]
    async fn request_execution_request_back_edge_loads_parent() -> Result<(), String> {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let exec = sample_execution(now);
        let exec_services = FakeRequestExecutionServices {
            records: Arc::new(Mutex::new(vec![exec])),
            ..FakeRequestExecutionServices::default()
        };
        let req_services = FakeRequestServices {
            request: Some(sample_request("r-1")),
            ..FakeRequestServices::default()
        };
        let chan_services = FakeChannelServices::default();
        let schema = back_edge_schema(exec_services, req_services.clone(), chan_services);

        let resp = schema
            .execute("{ requestExecutions { edges { node { request { id modelID } } } } }")
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);

        // The back-edge resolver must have injected `where: { id: "r-1" }`
        // and `first: 1` (mirroring ent's M2O single-parent load).
        let captured = lock(&req_services.captured).clone();
        assert_eq!(captured.len(), 1, "expected one parent-request query");
        let filter = captured[0]
            .where_filter
            .clone()
            .ok_or("back-edge did not inject a where filter")?;
        assert_eq!(filter.id, Some(ID::from("r-1")));
        assert_eq!(captured[0].first, Some(1));
        Ok(())
    }

    #[tokio::test]
    async fn request_execution_request_back_edge_errors_when_parent_missing() -> Result<(), String>
    {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let exec = sample_execution(now);
        let exec_services = FakeRequestExecutionServices {
            records: Arc::new(Mutex::new(vec![exec])),
            ..FakeRequestExecutionServices::default()
        };
        // Empty parent store — no Request matches the injected id.
        let req_services = FakeRequestServices::default();
        let chan_services = FakeChannelServices::default();
        let schema = back_edge_schema(exec_services, req_services, chan_services);

        let resp = schema
            .execute("{ requestExecutions { edges { node { request { id } } } } }")
            .await;
        assert_eq!(resp.errors.len(), 1, "errors: {:?}", resp.errors);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("parent request was not found"),
            "unexpected error: {msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn request_execution_channel_back_edge_loads_parent() -> Result<(), String> {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let exec = sample_execution(now);
        let exec_services = FakeRequestExecutionServices {
            records: Arc::new(Mutex::new(vec![exec])),
            ..FakeRequestExecutionServices::default()
        };
        let req_services = FakeRequestServices::default();
        let chan = crate::channel::Channel {
            id: ID::from("ch-1"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            channel_type: crate::channel::ChannelType::Openai,
            base_url: None,
            website_url: None,
            quota_currency: "USD".to_string(),
            actual_quota_used: None,
            quota_remaining: None,
            name: "alpha".to_string(),
            status: crate::channel::ChannelStatus::Enabled,
            supported_models: vec![],
            manual_models: None,
            auto_sync_supported_models: false,
            auto_sync_model_pattern: None,
            tags: None,
            default_test_model: "gpt-4".to_string(),
            policies: None,
            settings: None,
            ordering_weight: 0,
            error_message: None,
            remark: None,
            endpoints: None,
        };
        let chan_services = FakeChannelServices {
            channel: Some(chan),
            ..FakeChannelServices::default()
        };
        let schema = back_edge_schema(exec_services, req_services, chan_services.clone());

        let resp = schema
            .execute("{ requestExecutions { edges { node { channel { id name } } } } }")
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);

        let captured = lock(&chan_services.captured).clone();
        assert_eq!(captured.len(), 1, "expected one parent-channel query");
        let filter = captured[0]
            .where_filter
            .clone()
            .ok_or("back-edge did not inject a where filter")?;
        assert_eq!(filter.id, Some(ID::from("ch-1")));
        assert_eq!(captured[0].first, Some(1));
        Ok(())
    }

    #[tokio::test]
    async fn request_execution_channel_back_edge_returns_none_when_id_absent() -> Result<(), String>
    {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        // Build an execution with no channel_id.
        let mut exec = sample_execution(now);
        exec.channel_id = None;
        let exec_services = FakeRequestExecutionServices {
            records: Arc::new(Mutex::new(vec![exec])),
            ..FakeRequestExecutionServices::default()
        };
        let req_services = FakeRequestServices::default();
        let chan_services = FakeChannelServices::default();
        let schema = back_edge_schema(exec_services, req_services, chan_services.clone());

        let resp = schema
            .execute("{ requestExecutions { edges { node { channel { id } } } } }")
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        // No channel query should have been issued (short-circuit on null FK).
        let captured = lock(&chan_services.captured).clone();
        assert!(
            captured.is_empty(),
            "unexpected channel queries: {captured:?}"
        );
        Ok(())
    }
}
