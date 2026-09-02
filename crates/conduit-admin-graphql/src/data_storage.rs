//! RUST-P12-001 S07 — Data Storage GraphQL slice.
//!
//! Ports the data-storage GraphQL operations declared in
//! `conduit/internal/server/gql/conduit.graphql` (top-level shapes) and
//! `conduit/internal/server/gql/ent.graphql` (ent-generated connection / input
//! types), resolved by `conduit.resolvers.go` and `ent.resolvers.go`.
//!
//! ## Operations ported this slice
//!
//! Query:
//!   - `Query.dataStorages(after, first, before, last, orderBy, where):
//!     DataStorageConnection!` — ent connection query, snapshot lines 5341-5371
//!     (Go resolver: `ent.resolvers.go` `Query.dataStorages`, ent `Paginate`).
//!
//! Mutations:
//!   - `Mutation.createDataStorage(input: CreateDataStorageInput!): DataStorage!`
//!     — Go resolver `conduit.resolvers.go:565` delegates to
//!     `biz.DataStorageService.CreateDataStorage` (`biz/data_storage.go:192`):
//!     duplicate-name check → ent create with `primary=false`,
//!     `status=active` → cache invalidation.
//!   - `Mutation.updateDataStorage(id: ID!, input: UpdateDataStorageInput!):
//!     DataStorage!` — Go resolver `conduit.resolvers.go:570` delegates to
//!     `biz.DataStorageService.UpdateDataStorage` (`biz/data_storage.go:224`):
//!     read existing → duplicate-name check excluding self → merge settings
//!     (sensitive credential fields kept when input omits them) → ent update.
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//!   - `Mutation.deleteDataStorage(...)` — NOT in the snapshot Mutation list
//!     (the Go schema exposes only create/update for DataStorage; deletion is
//!     an internal/admin-only operation). Tracked here in case a future slice
//!     adds it.
//!   - `Request.dataStorage: DataStorage`, `Channel.dataStorageID`, etc. — edge
//!     fields into this domain owned by other slices (Request slice consumes
//!     DataStorage as a typed reference).
//!   - `Mutation.updateDefaultDataStorage` — system-settings slice (declared
//!     `system.graphql:366`).
//!   - The `requests(...)` / `executions(...)` edge fields on `DataStorage`
//!     itself are pending the Request/RequestExecution edge back-reference
//!     slice.
//!
//! ## Service wiring
//!
//! The admin-graphql crate stays free of DB/IO concerns. The host wires a
//! concrete implementation of [`DataStorageServices`] into the schema data bag
//! at build time; resolver-level tests inject an in-memory fake (mirrors the
//! dependency-injection pattern used by `channel::ChannelQueryServices` and
//! `system::SystemSettingsServices`).

use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, InputObject, SimpleObject};

use crate::pagination::PageInfo;
use crate::scalars::{CursorScalar, TimeScalar};

// ===========================================================================
// Enums — snapshot lines 3457-3477 (DataStorageOrderField, Status, Type).
// ===========================================================================

/// `enum DataStorageOrderField { CREATED_AT UPDATED_AT }` — snapshot lines
/// 3457-3460.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum DataStorageOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

/// `enum DataStorageStatus { active archived }` — snapshot lines 3464-3467.
/// Bound to Go `ent/datastorage.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum DataStorageStatus {
    #[graphql(name = "active")]
    Active,
    #[graphql(name = "archived")]
    Archived,
}

/// `enum DataStorageType { database fs s3 gcs webdav }` — snapshot lines
/// 3471-3477. Bound to Go `ent/datastorage.Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum DataStorageType {
    #[graphql(name = "database")]
    Database,
    #[graphql(name = "fs")]
    Fs,
    #[graphql(name = "s3")]
    S3,
    #[graphql(name = "gcs")]
    Gcs,
    #[graphql(name = "webdav")]
    Webdav,
}

/// Re-export of the ent-global `OrderDirection` enum owned by
/// [`crate::channel`] (snapshot line 4072). Kept as a single canonical
/// registration so the SDL registry does not see two enums with the same
/// GraphQL name.
pub use crate::channel::OrderDirection;

// ===========================================================================
// Output object types — mirror Go `objects.DataStorageSettings` /
// `objects.S3` / `objects.GCS` / `objects.WebDAV` (`objects/data_stograge.go`)
// and the ent-generated `type DataStorage` (snapshot lines 3320-3410).
// ===========================================================================

/// `type S3` — snapshot lines 594-599. The GraphQL output type narrows
/// `endpoint`/`region`/`pathStyle` to non-null (snapshot line 596-598) even
/// though Go `objects.S3` (`data_stograge.go:20-29`) keeps them as plain
/// strings; the resolver populates them from the ent JSON column. We model
/// them as non-null to match the snapshot — the host MUST populate them
/// (defaulting to empty string / false when the stored row is missing the
/// value, mirroring Go's behavior for legacy rows).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "S3")]
pub struct S3 {
    /// GraphQL field `bucketName` — `String!`.
    #[graphql(name = "bucketName")]
    pub bucket_name: String,
    /// GraphQL field `endpoint` — `String!` in snapshot.
    pub endpoint: String,
    /// GraphQL field `region` — `String!` in snapshot.
    pub region: String,
    /// GraphQL field `pathStyle` — `Boolean!` in snapshot.
    pub path_style: bool,
}

/// `type GCS { bucketName: String! }` — snapshot lines 601-603.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "GCS")]
pub struct GCS {
    #[graphql(name = "bucketName")]
    pub bucket_name: String,
}

/// `type WebDAV` — snapshot lines 586-592.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "WebDAV")]
pub struct WebDAV {
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Snapshot field name `insecure_skip_tls` — snake_case, NOT camelCase.
    /// The gqlgen schema emits the field name verbatim (the `goTag` on Go
    /// `objects.WebDAV.InsecureSkipTLS` is `json:"insecure_skip_tls"`); we pin
    /// the GraphQL name explicitly to keep async-graphql from rewriting it.
    #[graphql(name = "insecure_skip_tls")]
    pub insecure_skip_tls: Option<bool>,
    pub path: Option<String>,
}

/// `type DataStorageSettings` — snapshot lines 578-584.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "DataStorageSettings")]
pub struct DataStorageSettings {
    pub dsn: Option<String>,
    pub directory: Option<String>,
    pub s3: Option<S3>,
    pub gcs: Option<GCS>,
    pub webdav: Option<WebDAV>,
}

/// `type DataStorage implements Node` — snapshot lines 3320-3410, scalar and
/// self-domain fields only. The `executions(...)` edge is resolved by the
/// [`ComplexObject`] impl below; the `requests(...)` edge remains pending
/// (module doc).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
#[graphql(name = "DataStorage")]
pub struct DataStorage {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    pub name: String,
    pub description: String,
    /// Snapshot line 3335: "data source is primary, only the system database
    /// is the primary, it can not be archived." Go `ent.DataStorage.Primary`
    /// is non-null `bool`.
    pub primary: bool,
    #[graphql(name = "type")]
    pub storage_type: DataStorageType,
    pub settings: DataStorageSettings,
    pub status: DataStorageStatus,
}

// ===========================================================================
// DataStorage forward-edge resolver — RUST-P12-001 S07 continuation.
//
// Go source: `conduit/internal/server/gql/generated.go:24475-24493`
// auto-resolves `DataStorage.executions(...)` by calling
// `obj.Executions(ctx, after, first, before, last, orderBy, where)` on the
// ent `*ent.DataStorage`. ent injects the FK predicate
// `request_executions.data_storage_id = obj.ID` via `sqlgraph.Neighbors`
// (`ent/datastorage/datastorage.go:53-58`: `ExecutionsTable =
// "request_executions"`, `ExecutionsColumn = "data_storage_id"`).
//
// This slice mirrors that filter by injecting `dataStorageID: <obj.id>`
// into the caller-supplied `where` argument and delegating to the
// host-injected [`crate::request_execution::RequestExecutionQueryServices`].
// ===========================================================================

#[ComplexObject]
impl DataStorage {
    /// `DataStorage.executions(...): RequestExecutionConnection!` — snapshot
    /// lines 3380-3410. Mirrors the ent-generated edge resolver: filter the
    /// request-execution connection by `dataStorageID = obj.id`, then
    /// delegate to the host-injected
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
        let injected = crate::request_execution::RequestExecutionWhereInput {
            data_storage_id: Some(self.id.clone()),
            ..where_filter.unwrap_or_default()
        };
        let args = crate::request_execution::RequestExecutionConnectionArgs {
            access: crate::request_usage::request_read_access_scope(ctx)?,
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

/// `type DataStorageEdge` — snapshot lines 3431-3440. `node` is nullable in
/// the snapshot (`node: DataStorage`, no `!`), matching the ent generation
/// pattern used by every other Connection edge in this crate.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct DataStorageEdge {
    pub node: Option<DataStorage>,
    pub cursor: CursorScalar,
}

/// `type DataStorageConnection` — snapshot lines 3414-3427. `edges` is a
/// nullable list of nullable edges (`[DataStorageEdge]`), matching the ent
/// generation pattern used by every other Connection type in this crate.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct DataStorageConnection {
    pub edges: Option<Vec<Option<DataStorageEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

// ===========================================================================
// Input object types
// ===========================================================================

/// `input S3Input` — snapshot lines 564-571.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "S3Input")]
pub struct S3Input {
    #[graphql(name = "bucketName")]
    pub bucket_name: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    pub path_style: Option<bool>,
}

/// `input GCSInput` — snapshot lines 573-576.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "GCSInput")]
pub struct GCSInput {
    #[graphql(name = "bucketName")]
    pub bucket_name: String,
    pub credential: Option<String>,
}

/// `input WebDAVInput` — snapshot lines 556-562. The `insecure_skip_tls`
/// field name is snake_case in the snapshot (the Go struct tag is
/// `json:"insecure_skip_tls"`); we pin it explicitly.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "WebDAVInput")]
pub struct WebDAVInput {
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    #[graphql(name = "insecure_skip_tls")]
    pub insecure_skip_tls: Option<bool>,
    pub path: Option<String>,
}

/// `input DataStorageSettingsInput` — snapshot lines 548-554.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "DataStorageSettingsInput")]
pub struct DataStorageSettingsInput {
    pub dsn: Option<String>,
    pub directory: Option<String>,
    pub s3: Option<S3Input>,
    pub gcs: Option<GCSInput>,
    pub webdav: Option<WebDAVInput>,
}

/// `input DataStorageOrder` — snapshot lines 3444-3453.
///
/// `direction: OrderDirection! = ASC` — non-null with default ASC, matching
/// every other ent `*Order` input. We use `#[graphql(default_with = …)]`
/// rather than `Option` so the SDL emits the `!` and the default value
/// exactly as the snapshot declares (mirrors `channel::ChannelOrder`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "DataStorageOrder")]
pub struct DataStorageOrder {
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: DataStorageOrderField,
}

/// `input CreateDataStorageInput` — snapshot lines 2951-2972. Generated by
/// ent; `status` is nullable so the resolver can default to `active` (Go
/// `biz/data_storage.go:211`).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "CreateDataStorageInput")]
pub struct CreateDataStorageInput {
    pub name: String,
    pub description: String,
    #[graphql(name = "type")]
    pub storage_type: DataStorageType,
    pub settings: DataStorageSettingsInput,
    pub status: Option<DataStorageStatus>,
}

/// `input UpdateDataStorageInput` — snapshot lines 7401-7420. All fields
/// nullable to match the Go partial-merge semantics (None = keep current).
/// Note: `type` is NOT in the update input — the data storage type is
/// immutable post-create (snapshot line 7401-7420 confirms no `type` field).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateDataStorageInput")]
pub struct UpdateDataStorageInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub settings: Option<DataStorageSettingsInput>,
    pub status: Option<DataStorageStatus>,
}

/// `input DataStorageWhereInput` — snapshot lines 3482-3560 (ent-generated
/// predicate grammar). Implemented: `not`/`and`/`or`, scalar predicates for
/// id/createdAt/updatedAt/name/description, type predicates, status
/// predicates, and the `primary` boolean predicate. Edge predicates
/// (`hasRequestsWith`/`hasExecutionsWith`) reference other entities'
/// WhereInputs and are pending.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct DataStorageWhereInput {
    pub not: Option<Box<DataStorageWhereInput>>,
    pub and: Option<Vec<DataStorageWhereInput>>,
    pub or: Option<Vec<DataStorageWhereInput>>,
    // id field predicates (snapshot lines 3489-3496)
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
    // created_at field predicates (snapshot lines 3500-3507)
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
    // updated_at field predicates (snapshot lines 3511-3518)
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
    // name field predicates (snapshot lines 3522-3534)
    pub name: Option<String>,
    #[graphql(name = "nameNEQ")]
    pub name_neq: Option<String>,
    #[graphql(name = "nameIn")]
    pub name_in: Option<Vec<String>>,
    #[graphql(name = "nameNotIn")]
    pub name_not_in: Option<Vec<String>>,
    #[graphql(name = "nameGT")]
    pub name_gt: Option<String>,
    #[graphql(name = "nameGTE")]
    pub name_gte: Option<String>,
    #[graphql(name = "nameLT")]
    pub name_lt: Option<String>,
    #[graphql(name = "nameLTE")]
    pub name_lte: Option<String>,
    pub name_contains: Option<String>,
    #[graphql(name = "nameHasPrefix")]
    pub name_has_prefix: Option<String>,
    #[graphql(name = "nameHasSuffix")]
    pub name_has_suffix: Option<String>,
    #[graphql(name = "nameEqualFold")]
    pub name_equal_fold: Option<String>,
    #[graphql(name = "nameContainsFold")]
    pub name_contains_fold: Option<String>,
    // description field predicates (snapshot lines 3538-3550)
    pub description: Option<String>,
    #[graphql(name = "descriptionNEQ")]
    pub description_neq: Option<String>,
    #[graphql(name = "descriptionIn")]
    pub description_in: Option<Vec<String>>,
    #[graphql(name = "descriptionNotIn")]
    pub description_not_in: Option<Vec<String>>,
    #[graphql(name = "descriptionGT")]
    pub description_gt: Option<String>,
    #[graphql(name = "descriptionGTE")]
    pub description_gte: Option<String>,
    #[graphql(name = "descriptionLT")]
    pub description_lt: Option<String>,
    #[graphql(name = "descriptionLTE")]
    pub description_lte: Option<String>,
    pub description_contains: Option<String>,
    #[graphql(name = "descriptionHasPrefix")]
    pub description_has_prefix: Option<String>,
    #[graphql(name = "descriptionHasSuffix")]
    pub description_has_suffix: Option<String>,
    #[graphql(name = "descriptionEqualFold")]
    pub description_equal_fold: Option<String>,
    #[graphql(name = "descriptionContainsFold")]
    pub description_contains_fold: Option<String>,
    // primary field predicates (snapshot line 3554-3555)
    pub primary: Option<bool>,
    #[graphql(name = "primaryNEQ")]
    pub primary_neq: Option<bool>,
    // type field predicates (snapshot lines 3559-3562)
    #[graphql(name = "type")]
    pub storage_type: Option<DataStorageType>,
    #[graphql(name = "typeNEQ")]
    pub type_neq: Option<DataStorageType>,
    #[graphql(name = "typeIn")]
    pub type_in: Option<Vec<DataStorageType>>,
    #[graphql(name = "typeNotIn")]
    pub type_not_in: Option<Vec<DataStorageType>>,
    // status field predicates (snapshot lines 3566-3569)
    pub status: Option<DataStorageStatus>,
    #[graphql(name = "statusNEQ")]
    pub status_neq: Option<DataStorageStatus>,
    #[graphql(name = "statusIn")]
    pub status_in: Option<Vec<DataStorageStatus>>,
    #[graphql(name = "statusNotIn")]
    pub status_not_in: Option<Vec<DataStorageStatus>>,
}

// ===========================================================================
// Order selection lowering (mirrors `resolve_channel_order` in channel.rs)
// ===========================================================================

/// Internal ordering selection used by the host service. Mirrors the
/// Go resolver behaviour: ent's default DataStorage order is ID; the resolver
/// remaps `CREATED_AT` to ID-order (direction preserved) — same pattern as
/// `ent.resolvers.go` does for every other ent connection query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataStorageOrderTerm {
    /// `CREATED_AT` ordering is remapped to the ent default ID order, with
    /// the requested direction preserved (gql_pagination.go:413).
    Id,
    UpdatedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataStorageOrderSelection {
    pub direction: OrderDirection,
    pub term: DataStorageOrderTerm,
}

/// Lowers the GraphQL `DataStorageOrder` argument onto the internal
/// selection. Mirrors the Go resolver behaviour shared by every ent
/// connection query (`gql_pagination.go:413` — `CREATED_AT` ⟶ default ID
/// order).
pub fn resolve_data_storage_order(
    order_by: Option<DataStorageOrder>,
) -> Option<DataStorageOrderSelection> {
    order_by.map(|order| DataStorageOrderSelection {
        direction: order.direction,
        term: match order.field {
            DataStorageOrderField::CreatedAt => DataStorageOrderTerm::Id,
            DataStorageOrderField::UpdatedAt => DataStorageOrderTerm::UpdatedAt,
        },
    })
}

// ===========================================================================
// Service traits (host-injected, mirroring the Go resolver's dependencies)
// ===========================================================================

/// Error surface for the data-storage services. Messages mirror the Go error
/// strings so frontend error handling stays stable:
///   - duplicate name — `xerrors.DuplicateNameError("data storage", name)`.
///   - not found — ent's `ent: datastorage not found`.
///   - wrapped create/update failures — `fmt.Errorf("failed to ...: %w")`
///     prefixes in `biz/data_storage.go`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum DataStorageServiceError {
    #[error("data storage service is not available")]
    ServiceUnavailable,
    #[error("data storage name '{0}' already exists")]
    DuplicateName(String),
    #[error("ent: datastorage not found")]
    NotFound,
    #[error("failed to create data storage: {0}")]
    Create(String),
    #[error("failed to update data storage: {0}")]
    Update(String),
    #[error("failed to query data storages: {0}")]
    Query(String),
}

/// Arguments for the `dataStorages` connection query, passed through from
/// the GraphQL layer verbatim (Go hands them straight to ent's `Paginate`).
#[derive(Debug, Clone, Default)]
pub struct DataStorageConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<DataStorageOrderSelection>,
    pub where_filter: Option<DataStorageWhereInput>,
}

/// Backs `Query.dataStorages` (Go ent.resolvers.go: `r.client.DataStorage
/// .Query().Paginate(...)`).
#[async_trait::async_trait]
pub trait DataStorageQueryServices: Send + Sync {
    async fn data_storages(
        &self,
        args: DataStorageConnectionArgs,
    ) -> Result<DataStorageConnection, DataStorageServiceError>;
}

/// Backs the two CRUD mutations (Go `biz.DataStorageService`). `id` is the
/// raw GraphQL `ID!` scalar string; Go decodes it into `objects.GUID` and
/// passes `.ID` (int) to the service.
#[async_trait::async_trait]
pub trait DataStorageMutationServices: Send + Sync {
    /// Mirrors `DataStorageService.CreateDataStorage`
    /// (`biz/data_storage.go:192`): reject duplicate names, create with
    /// `primary=false` + `status=active` defaults, then invalidate caches.
    async fn create_data_storage(
        &self,
        input: CreateDataStorageInput,
    ) -> Result<DataStorage, DataStorageServiceError>;

    /// Mirrors `DataStorageService.UpdateDataStorage`
    /// (`biz/data_storage.go:224`): read existing, duplicate-name check
    /// excluding self, merge settings (sensitive credential fields kept when
    /// input omits them), ent update.
    async fn update_data_storage(
        &self,
        id: &str,
        input: UpdateDataStorageInput,
    ) -> Result<DataStorage, DataStorageServiceError>;
}

/// Unified marker trait the host wires when it wants to register both the
/// query and mutation halves with a single `Arc`. Concrete implementations
/// must implement both supertraits. Trait upcasting (`Arc<dyn
/// DataStorageServices>` → `Arc<dyn DataStorageQueryServices>`) lets the
/// resolver helpers split the unified registration back into its halves —
/// trait upcasting has been stable since Rust 1.86 (April 2025) and the
/// workspace pins a newer toolchain.
#[async_trait::async_trait]
pub trait DataStorageServices:
    DataStorageQueryServices + DataStorageMutationServices + Send + Sync
{
}

#[async_trait::async_trait]
impl<T> DataStorageServices for T where
    T: DataStorageQueryServices + DataStorageMutationServices + Send + Sync + ?Sized
{
}

/// Resolves the injected [`DataStorageQueryServices`] from the async-graphql
/// context data bag. If a unified [`DataStorageServices`] was wired, the
/// result is obtained via trait upcasting; otherwise the resolver looks up
/// the query-only trait directly. Returns the Go-equivalent "service
/// unavailable" error message if neither is present.
pub(crate) fn data_storage_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn DataStorageQueryServices>, String> {
    if let Ok(services) = ctx.data::<Arc<dyn DataStorageServices>>() {
        let query: Arc<dyn DataStorageQueryServices> =
            Arc::clone(services) as Arc<dyn DataStorageQueryServices>;
        return Ok(query);
    }
    match ctx.data::<Arc<dyn DataStorageQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(DataStorageServiceError::ServiceUnavailable.to_string()),
    }
}

/// Resolves the injected [`DataStorageMutationServices`] from the context
/// data bag. Mirrors [`data_storage_query_services`] for the mutation half.
pub(crate) fn data_storage_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn DataStorageMutationServices>, String> {
    if let Ok(services) = ctx.data::<Arc<dyn DataStorageServices>>() {
        let mutation: Arc<dyn DataStorageMutationServices> =
            Arc::clone(services) as Arc<dyn DataStorageMutationServices>;
        return Ok(mutation);
    }
    match ctx.data::<Arc<dyn DataStorageMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(DataStorageServiceError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::{EmptySubscription, Name, Object, Schema, Value};
    use chrono::TimeZone;

    use super::*;
    use crate::mutation::MutationRoot;

    /// Mutex-guard helper that never panics on poison.
    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Extract the inner `Value::Object` or panic with a clear message.
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
    ) -> Result<chrono::DateTime<chrono::Utc>, String> {
        match chrono::Utc.with_ymd_and_hms(y, m, d, h, mi, s) {
            chrono::LocalResult::Single(dt) => Ok(dt),
            other => Err(format!("ambiguous/invalid timestamp: {other:?}")),
        }
    }

    // ---------------------------------------------------------------------
    // In-memory fake — implements BOTH services so tests can wire a single
    // concrete.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct FakeDataStorageServices {
        records: Arc<Mutex<Vec<DataStorage>>>,
        query_error: Option<DataStorageServiceError>,
        create_error: Option<DataStorageServiceError>,
        update_error: Option<DataStorageServiceError>,
        create_calls: Arc<Mutex<Vec<CreateDataStorageInput>>>,
        update_calls: Arc<Mutex<Vec<(String, UpdateDataStorageInput)>>>,
    }

    #[async_trait::async_trait]
    impl DataStorageQueryServices for FakeDataStorageServices {
        async fn data_storages(
            &self,
            args: DataStorageConnectionArgs,
        ) -> Result<DataStorageConnection, DataStorageServiceError> {
            match &self.query_error {
                Some(err) => Err(err.clone()),
                None => {
                    let records = lock(&self.records).clone();
                    // Minimal filtering by `name` predicate if provided; the
                    // host does the real work, this is just enough for tests.
                    let filtered: Vec<DataStorage> =
                        match args.where_filter.as_ref().and_then(|w| w.name.clone()) {
                            Some(name) => records.into_iter().filter(|r| r.name == name).collect(),
                            None => records,
                        };
                    let total = filtered.len() as i64;
                    Ok(DataStorageConnection {
                        edges: Some(
                            filtered
                                .into_iter()
                                .map(|node| {
                                    Some(DataStorageEdge {
                                        cursor: CursorScalar(format!(
                                            "datastorage:{}",
                                            node.id.as_str()
                                        )),
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

    #[async_trait::async_trait]
    impl DataStorageMutationServices for FakeDataStorageServices {
        async fn create_data_storage(
            &self,
            input: CreateDataStorageInput,
        ) -> Result<DataStorage, DataStorageServiceError> {
            lock(&self.create_calls).push(input.clone());
            match &self.create_error {
                Some(err) => Err(err.clone()),
                None => {
                    // Go biz/data_storage.go:194-203: duplicate name check.
                    let exists = lock(&self.records).iter().any(|r| r.name == input.name);
                    if exists {
                        return Err(DataStorageServiceError::DuplicateName(input.name.clone()));
                    }
                    let now = chrono::Utc::now();
                    let next_id = (lock(&self.records).len() + 1).to_string();
                    let ds = DataStorage {
                        id: ID::from(next_id),
                        created_at: TimeScalar(now),
                        updated_at: TimeScalar(now),
                        name: input.name,
                        description: input.description,
                        primary: false,
                        storage_type: input.storage_type,
                        settings: map_settings_from_input(input.settings),
                        status: input.status.unwrap_or(DataStorageStatus::Active),
                    };
                    lock(&self.records).push(ds.clone());
                    Ok(ds)
                }
            }
        }

        async fn update_data_storage(
            &self,
            id: &str,
            input: UpdateDataStorageInput,
        ) -> Result<DataStorage, DataStorageServiceError> {
            lock(&self.update_calls).push((id.to_string(), input.clone()));
            match &self.update_error {
                Some(err) => Err(err.clone()),
                None => {
                    let mut records = lock(&self.records);
                    let idx = records.iter().position(|r| r.id.as_str() == id);
                    let idx = match idx {
                        Some(i) => i,
                        None => return Err(DataStorageServiceError::NotFound),
                    };
                    let existing = records[idx].clone();
                    // Go biz/data_storage.go:232-246: duplicate-name check
                    // excluding self.
                    if let Some(ref name) = input.name
                        && name != &existing.name
                        && records
                            .iter()
                            .enumerate()
                            .any(|(i, r)| i != idx && &r.name == name)
                    {
                        return Err(DataStorageServiceError::DuplicateName(name.clone()));
                    }
                    let updated = DataStorage {
                        id: existing.id.clone(),
                        created_at: existing.created_at.clone(),
                        updated_at: TimeScalar(chrono::Utc::now()),
                        name: input.name.unwrap_or(existing.name),
                        description: input.description.unwrap_or(existing.description),
                        primary: existing.primary,
                        storage_type: existing.storage_type,
                        settings: match input.settings {
                            Some(s) => merge_settings(&existing.settings, &s),
                            None => existing.settings,
                        },
                        status: input.status.unwrap_or(existing.status),
                    };
                    records[idx] = updated.clone();
                    Ok(updated)
                }
            }
        }
    }

    /// Mirrors Go `biz.DataStorageService.mergeSettings`
    /// (`biz/data_storage.go:707-809`): each `Some` input field overrides the
    /// existing value; sensitive credential fields (S3 accessKey/secretKey,
    /// GCS credential, WebDAV password) are kept from `existing` when the
    /// input omits them — the snapshot says "Sensitive fields (credentials)
    /// are only updated if explicitly provided" (biz/data_storage.go:249).
    fn merge_settings(
        existing: &DataStorageSettings,
        input: &DataStorageSettingsInput,
    ) -> DataStorageSettings {
        DataStorageSettings {
            dsn: input.dsn.clone().or(existing.dsn.clone()),
            directory: input.directory.clone().or(existing.directory.clone()),
            s3: match (&existing.s3, &input.s3) {
                (Some(prev), Some(next)) => Some(merge_s3(prev, next)),
                (None, Some(next)) => Some(map_s3_from_input(next.clone())),
                (Some(prev), None) => Some(prev.clone()),
                (None, None) => None,
            },
            gcs: match (&existing.gcs, &input.gcs) {
                (Some(prev), Some(next)) => Some(merge_gcs(prev, next)),
                (None, Some(next)) => Some(GCS {
                    bucket_name: next.bucket_name.clone(),
                }),
                (Some(prev), None) => Some(prev.clone()),
                (None, None) => None,
            },
            webdav: match (&existing.webdav, &input.webdav) {
                (Some(prev), Some(next)) => Some(merge_webdav(prev, next)),
                (None, Some(next)) => Some(map_webdav_from_input(next.clone())),
                (Some(prev), None) => Some(prev.clone()),
                (None, None) => None,
            },
        }
    }

    fn merge_s3(existing: &S3, input: &S3Input) -> S3 {
        S3 {
            bucket_name: if input.bucket_name.is_empty() {
                existing.bucket_name.clone()
            } else {
                input.bucket_name.clone()
            },
            // Sensitive accessKey/secretKey are NOT part of the GraphQL
            // output type (snapshot S3 type has no accessKey/secretKey fields)
            // — only the input carries them. The host's persistence layer
            // merges them; the GraphQL output stays unchanged.
            endpoint: input
                .endpoint
                .clone()
                .or_else(|| Some(existing.endpoint.clone()))
                .unwrap_or_default(),
            region: input
                .region
                .clone()
                .or_else(|| Some(existing.region.clone()))
                .unwrap_or_default(),
            path_style: input.path_style.unwrap_or(existing.path_style),
        }
    }

    fn merge_gcs(existing: &GCS, _input: &GCSInput) -> GCS {
        // GCS output type only has bucketName; credential is input-only and
        // is handled by the persistence layer. We preserve the existing
        // bucketName when the input is provided with an empty bucket_name.
        GCS {
            bucket_name: existing.bucket_name.clone(),
        }
    }

    fn merge_webdav(existing: &WebDAV, input: &WebDAVInput) -> WebDAV {
        WebDAV {
            url: if input.url.is_empty() {
                existing.url.clone()
            } else {
                input.url.clone()
            },
            username: input.username.clone().or(existing.username.clone()),
            // password is sensitive — kept from existing when input omits.
            password: input.password.clone().or(existing.password.clone()),
            insecure_skip_tls: input.insecure_skip_tls.or(existing.insecure_skip_tls),
            path: input.path.clone().or(existing.path.clone()),
        }
    }

    fn map_s3_from_input(input: S3Input) -> S3 {
        S3 {
            bucket_name: input.bucket_name,
            endpoint: input.endpoint.unwrap_or_default(),
            region: input.region.unwrap_or_default(),
            path_style: input.path_style.unwrap_or(false),
        }
    }

    fn map_webdav_from_input(input: WebDAVInput) -> WebDAV {
        WebDAV {
            url: input.url,
            username: input.username,
            password: input.password,
            insecure_skip_tls: input.insecure_skip_tls,
            path: input.path,
        }
    }

    fn map_settings_from_input(input: DataStorageSettingsInput) -> DataStorageSettings {
        DataStorageSettings {
            dsn: input.dsn,
            directory: input.directory,
            s3: input.s3.map(map_s3_from_input),
            gcs: input.gcs.map(|g| GCS {
                bucket_name: g.bucket_name,
            }),
            webdav: input.webdav.map(map_webdav_from_input),
        }
    }

    type TestSchema = Schema<crate::QueryRoot, MutationRoot, EmptySubscription>;

    fn schema_with_services(services: FakeDataStorageServices) -> TestSchema {
        let arc: Arc<dyn DataStorageServices> = Arc::new(services);
        crate::admin_schema_builder().data(arc).finish()
    }

    // ---- order-lowering semantics -----------------------------------

    #[test]
    fn resolve_data_storage_order_remaps_created_at_to_default_id_order() {
        let order = Some(DataStorageOrder {
            direction: OrderDirection::Desc,
            field: DataStorageOrderField::CreatedAt,
        });
        let sel = resolve_data_storage_order(order);
        match sel {
            Some(DataStorageOrderSelection { direction, term }) => {
                assert_eq!(direction, OrderDirection::Desc);
                assert_eq!(term, DataStorageOrderTerm::Id);
            }
            None => panic!("expected Some selection"),
        }
    }

    #[test]
    fn resolve_data_storage_order_passes_updated_at_through() {
        let order = Some(DataStorageOrder {
            direction: OrderDirection::Asc,
            field: DataStorageOrderField::UpdatedAt,
        });
        let sel = resolve_data_storage_order(order);
        match sel {
            Some(DataStorageOrderSelection { direction, term }) => {
                assert_eq!(direction, OrderDirection::Asc);
                assert_eq!(term, DataStorageOrderTerm::UpdatedAt);
            }
            None => panic!("expected Some selection"),
        }
    }

    #[test]
    fn resolve_data_storage_order_none_when_argument_absent() {
        assert!(resolve_data_storage_order(None).is_none());
    }

    // ---- resolver: data_storages query ------------------------------

    #[tokio::test]
    async fn data_storages_returns_connection_shape() -> Result<(), String> {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let ds = DataStorage {
            id: ID::from("1"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            name: "primary-db".to_string(),
            description: "system db".to_string(),
            primary: true,
            storage_type: DataStorageType::Database,
            settings: DataStorageSettings {
                dsn: Some("postgresql://db/conduit".to_string()),
                directory: None,
                s3: None,
                gcs: None,
                webdav: None,
            },
            status: DataStorageStatus::Active,
        };
        let fake = FakeDataStorageServices {
            records: Arc::new(Mutex::new(vec![ds])),
            ..FakeDataStorageServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(
                "{ dataStorages { totalCount edges { node { id name type status primary } cursor } pageInfo { hasNextPage } } }",
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let conn = match obj.get(&Name::new("dataStorages")) {
            Some(v) => v,
            None => panic!("dataStorages missing in {obj:?}"),
        };
        let conn_fields = as_object(conn);
        match conn_fields.get(&Name::new("totalCount")) {
            Some(Value::Number(n)) => assert_eq!(n.as_i64(), Some(1)),
            other => panic!("totalCount unexpected: {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn data_storages_surfaces_service_unavailable_when_unwired() {
        let schema: TestSchema = crate::admin_schema_builder().finish();
        let resp = schema.execute("{ dataStorages { totalCount } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("data storage service is not available"),
            "unexpected msg: {msg}"
        );
    }

    // ---- resolver: create_data_storage mutation ---------------------

    #[tokio::test]
    async fn create_data_storage_returns_record_and_forwards_input() {
        let fake = FakeDataStorageServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    createDataStorage(input: {
                        name: "s3-prod",
                        description: "prod s3 bucket",
                        type: "s3",
                        settings: { s3: { bucketName: "prod", endpoint: "https://s3", region: "us-east-1", accessKey: "ak", secretKey: "sk", pathStyle: true } },
                        status: "active"
                    }) { id name type status primary }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let calls = lock(&fake.create_calls).clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "s3-prod");
        // Go biz/data_storage.go:210: create forces primary=false.
        let obj = as_object(&resp.data);
        let ds = match obj.get(&Name::new("createDataStorage")) {
            Some(v) => v,
            None => panic!("createDataStorage missing"),
        };
        let ds_fields = as_object(ds);
        match ds_fields.get(&Name::new("primary")) {
            Some(Value::Boolean(false)) => {}
            other => panic!("primary must be false on create: {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_data_storage_defaults_status_to_active_when_omitted() {
        let fake = FakeDataStorageServices::default();
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(
                r#"mutation {
                    createDataStorage(input: {
                        name: "fs",
                        description: "filesystem",
                        type: "fs",
                        settings: { directory: "/var/data" }
                    }) { status }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let ds = match obj.get(&Name::new("createDataStorage")) {
            Some(v) => v,
            None => panic!("createDataStorage missing"),
        };
        let ds_fields = as_object(ds);
        match ds_fields.get(&Name::new("status")) {
            // Go biz/data_storage.go:211 sets status=active by default.
            Some(Value::Enum(name)) => assert_eq!(name.as_str(), "active"),
            other => panic!("status unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_data_storage_surfaces_duplicate_name_error() {
        let now = chrono::Utc::now();
        let existing = DataStorage {
            id: ID::from("1"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            name: "dup".to_string(),
            description: String::new(),
            primary: false,
            storage_type: DataStorageType::Database,
            settings: DataStorageSettings {
                dsn: None,
                directory: None,
                s3: None,
                gcs: None,
                webdav: None,
            },
            status: DataStorageStatus::Active,
        };
        let fake = FakeDataStorageServices {
            records: Arc::new(Mutex::new(vec![existing])),
            ..FakeDataStorageServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(
                r#"mutation {
                    createDataStorage(input: { name: "dup", description: "", type: "database", settings: {} })
                    { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("data storage name 'dup' already exists"),
            "msg: {msg}"
        );
    }

    // ---- resolver: update_data_storage mutation ---------------------

    #[tokio::test]
    async fn update_data_storage_merges_provided_fields() -> Result<(), String> {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let existing = DataStorage {
            id: ID::from("7"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            name: "old-name".to_string(),
            description: "old-desc".to_string(),
            primary: false,
            storage_type: DataStorageType::S3,
            settings: DataStorageSettings {
                dsn: None,
                directory: None,
                s3: Some(S3 {
                    bucket_name: "old-bucket".to_string(),
                    endpoint: "https://old".to_string(),
                    region: "us-east-1".to_string(),
                    path_style: false,
                }),
                gcs: None,
                webdav: None,
            },
            status: DataStorageStatus::Active,
        };
        let fake = FakeDataStorageServices {
            records: Arc::new(Mutex::new(vec![existing])),
            ..FakeDataStorageServices::default()
        };
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    updateDataStorage(id: "7", input: { description: "new-desc", status: "archived" })
                    { id name description status }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let calls = lock(&fake.update_calls).clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "7");
        // name not provided in input -> preserved.
        let obj = as_object(&resp.data);
        let ds = match obj.get(&Name::new("updateDataStorage")) {
            Some(v) => v,
            None => panic!("updateDataStorage missing"),
        };
        let ds_fields = as_object(ds);
        match ds_fields.get(&Name::new("name")) {
            Some(Value::String(s)) => assert_eq!(s, "old-name"),
            other => panic!("name unexpected: {other:?}"),
        }
        match ds_fields.get(&Name::new("description")) {
            Some(Value::String(s)) => assert_eq!(s, "new-desc"),
            other => panic!("description unexpected: {other:?}"),
        }
        match ds_fields.get(&Name::new("status")) {
            Some(Value::Enum(name)) => assert_eq!(name.as_str(), "archived"),
            other => panic!("status unexpected: {other:?}"),
        }
        Ok(())
    }

    #[tokio::test]
    async fn update_data_storage_surfaces_not_found() {
        let fake = FakeDataStorageServices::default();
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(
                r#"mutation {
                    updateDataStorage(id: "999", input: { name: "x" }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("ent: datastorage not found"), "msg: {msg}");
    }

    #[tokio::test]
    async fn update_data_storage_blocks_duplicate_name_excluding_self() -> Result<(), String> {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let a = DataStorage {
            id: ID::from("1"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            name: "alpha".to_string(),
            description: String::new(),
            primary: false,
            storage_type: DataStorageType::Database,
            settings: DataStorageSettings {
                dsn: None,
                directory: None,
                s3: None,
                gcs: None,
                webdav: None,
            },
            status: DataStorageStatus::Active,
        };
        let b = DataStorage {
            id: ID::from("2"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            name: "beta".to_string(),
            description: String::new(),
            primary: false,
            storage_type: DataStorageType::Database,
            settings: DataStorageSettings {
                dsn: None,
                directory: None,
                s3: None,
                gcs: None,
                webdav: None,
            },
            status: DataStorageStatus::Active,
        };
        let fake = FakeDataStorageServices {
            records: Arc::new(Mutex::new(vec![a, b])),
            ..FakeDataStorageServices::default()
        };
        let schema = schema_with_services(fake);

        // Update id=1 trying to take name "beta" (id=2 owns it) -> error.
        let resp = schema
            .execute(
                r#"mutation {
                    updateDataStorage(id: "1", input: { name: "beta" }) { id }
                }"#,
            )
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("data storage name 'beta' already exists"),
            "msg: {msg}"
        );

        // But updating id=1 to its OWN name is allowed (no-op rename).
        let resp = schema
            .execute(
                r#"mutation {
                    updateDataStorage(id: "1", input: { name: "alpha" }) { id name }
                }"#,
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        Ok(())
    }

    // ---- resolver: error propagation -------------------------------

    #[tokio::test]
    async fn create_data_storage_surfaces_create_error() {
        let fake = FakeDataStorageServices {
            create_error: Some(DataStorageServiceError::Create("db down".to_string())),
            ..FakeDataStorageServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(
                r#"mutation {
                    createDataStorage(input: { name: "x", description: "", type: "fs", settings: {} }) { id }
                }"#,
            )
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to create data storage"), "msg: {msg}");
        assert!(msg.contains("db down"), "msg: {msg}");
    }

    // ---- settings merge semantics ----------------------------------

    #[test]
    fn merge_settings_keeps_existing_when_input_is_none() {
        let existing = DataStorageSettings {
            dsn: Some("postgresql://db/conduit".to_string()),
            directory: Some("/var/data".to_string()),
            s3: None,
            gcs: None,
            webdav: None,
        };
        let input = DataStorageSettingsInput {
            dsn: None,
            directory: None,
            s3: None,
            gcs: None,
            webdav: None,
        };
        let merged = merge_settings(&existing, &input);
        assert_eq!(merged.dsn, existing.dsn);
        assert_eq!(merged.directory, existing.directory);
    }

    #[test]
    fn merge_settings_overrides_provided_dsn() {
        let existing = DataStorageSettings {
            dsn: Some("postgresql://old/conduit".to_string()),
            directory: None,
            s3: None,
            gcs: None,
            webdav: None,
        };
        let input = DataStorageSettingsInput {
            dsn: Some("postgres://new".to_string()),
            directory: None,
            s3: None,
            gcs: None,
            webdav: None,
        };
        let merged = merge_settings(&existing, &input);
        assert_eq!(merged.dsn.as_deref(), Some("postgres://new"));
    }

    // ---- SDL shape parity -----------------------------------------

    #[test]
    fn sdl_contains_data_storage_slice_types_and_signatures() {
        let arc: Arc<dyn DataStorageServices> = Arc::new(FakeDataStorageServices::default());
        let sdl = crate::admin_schema_builder().data(arc).finish().sdl();

        // Output types.
        for expected in [
            "type DataStorage {",
            "type DataStorageConnection {",
            "type DataStorageEdge {",
            "type DataStorageSettings {",
            "type S3 {",
            "type GCS {",
            "type WebDAV {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        // Input types.
        for expected in [
            "input CreateDataStorageInput {",
            "input UpdateDataStorageInput {",
            "input DataStorageSettingsInput {",
            "input S3Input {",
            "input GCSInput {",
            "input WebDAVInput {",
            "input DataStorageOrder {",
            "input DataStorageWhereInput {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        // Enums.
        for expected in [
            "enum DataStorageOrderField {",
            "enum DataStorageStatus {",
            "enum DataStorageType {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        // Queries.
        assert!(
            sdl.contains("dataStorages("),
            "SDL missing dataStorages query: {sdl}"
        );

        // Mutations.
        for expected in [
            "createDataStorage(input: CreateDataStorageInput!): DataStorage!",
            "updateDataStorage(id: ID!, input: UpdateDataStorageInput!): DataStorage!",
        ] {
            assert!(
                sdl.contains(expected),
                "SDL missing mutation {expected}:\n{sdl}"
            );
        }

        // Enum values (snake_case + lowercase — must NOT be SCREAMING_CASE).
        assert!(
            sdl.contains("database"),
            "missing database enum value: {sdl}"
        );
        assert!(
            sdl.contains("archived"),
            "missing archived enum value: {sdl}"
        );
        // Acronym / snake_case field name pinning.
        assert!(
            sdl.contains("insecure_skip_tls: Boolean"),
            "SDL missing insecure_skip_tls field: {sdl}"
        );
        assert!(
            sdl.contains("bucketName: String!"),
            "SDL missing bucketName field: {sdl}"
        );
    }

    #[test]
    fn sdl_matches_snapshot_for_data_storage_slice() -> Result<(), Box<dyn std::error::Error>> {
        let arc: Arc<dyn DataStorageServices> = Arc::new(FakeDataStorageServices::default());
        let sdl = crate::admin_schema_builder().data(arc).finish().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;

        for header in [
            "type DataStorage",
            "type DataStorageConnection",
            "type DataStorageEdge",
            "type DataStorageSettings",
            "type S3",
            "type GCS",
            "type WebDAV",
            "input CreateDataStorageInput",
            "input UpdateDataStorageInput",
            "input DataStorageSettingsInput",
            "input S3Input",
            "input GCSInput",
            "input WebDAVInput",
            "input DataStorageOrder",
            "enum DataStorageOrderField",
            "enum DataStorageStatus",
            "enum DataStorageType",
        ] {
            // `DataStorage` output type — pending `requests` edge field
            // (the `executions` edge was ported in the RUST-P12-001 S07
            // continuation slice — see the [`ComplexObject`] impl above).
            let pending: &[&str] = if header == "type DataStorage" {
                &["requests(…): RequestConnection!"]
            } else {
                &[]
            };
            crate::sdl_parity::assert_block_parity(&sdl, &snapshot, header, header, pending)?;
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
    // Forward-edge resolver semantics — `DataStorage.executions`
    // (RUST-P12-001 S07 continuation). The resolver delegates to the
    // host-injected `RequestExecutionQueryServices` with a `where: {
    // dataStorageID: <obj.id> }` predicate. This fake captures args so the
    // test can assert the injected FK predicate.
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
    async fn data_storage_executions_edge_injects_data_storage_id_predicate() -> Result<(), String>
    {
        let now = ts(2024, 1, 2, 3, 4, 5)?;
        let ds = DataStorage {
            id: ID::from("9"),
            created_at: TimeScalar(now),
            updated_at: TimeScalar(now),
            name: "primary-db".to_string(),
            description: "system db".to_string(),
            primary: true,
            storage_type: DataStorageType::Database,
            settings: DataStorageSettings {
                dsn: None,
                directory: None,
                s3: None,
                gcs: None,
                webdav: None,
            },
            status: DataStorageStatus::Active,
        };
        let ds_fake = FakeDataStorageServices {
            records: Arc::new(Mutex::new(vec![ds])),
            ..FakeDataStorageServices::default()
        };
        let exec_fake = CapturingExecServices::default();
        let ds_arc: Arc<dyn DataStorageServices> = Arc::new(ds_fake);
        let exec_arc: Arc<dyn crate::request_execution::RequestExecutionQueryServices> =
            Arc::new(exec_fake.clone());
        let mut request_context = conduit_auth::RequestContext::new();
        request_context
            .set_principal(conduit_auth::Principal::system())
            .map_err(|error| error.to_string())?;
        request_context
            .set_project_id("41")
            .map_err(|error| error.to_string())?;
        let schema = crate::admin_schema_builder()
            .data(ds_arc)
            .data(exec_arc)
            .data(request_context)
            .finish();

        let resp = schema
            .execute("{ dataStorages { edges { node { executions(first: 3) { totalCount } } } } }")
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);

        let captured = lock(&exec_fake.captured).clone();
        assert_eq!(captured.len(), 1, "expected one executions edge query");
        let filter = captured[0]
            .where_filter
            .clone()
            .ok_or("executions edge did not inject a where filter")?;
        assert_eq!(filter.data_storage_id, Some(ID::from("9")));
        assert_eq!(captured[0].access, crate::policy::AdminAccessScope::Global);
        assert_eq!(captured[0].first, Some(3));
        Ok(())
    }
}
