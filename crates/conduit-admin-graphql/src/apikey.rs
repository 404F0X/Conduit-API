//! RUST-P12-001 S07 — APIKey domain CRUD GraphQL slice.
//!
//! Bounded scope (this file): the `apiKeys` connection query plus the
//! `createAPIKey` / `updateAPIKey` / `updateAPIKeyStatus` / `rotateAPIKey`
//! mutations and every GraphQL type/input they reference. All shapes are
//! copied field-for-field from the captured contract snapshot
//! `tests/contracts/admin_graphql_schema.graphql`:
//!
//!   - `type APIKey implements Node` (snapshot line 1343) — scalar/self-domain
//!     fields only; the three cross-domain edge fields are pending (below).
//!   - `type APIKeyConnection` / `type APIKeyEdge` (lines 1404-1430).
//!   - `input APIKeyOrder` / `enum APIKeyOrderField` (lines 1434-1450 —
//!     CREATED_AT / UPDATED_AT only; no NAME ordering exists for api keys).
//!   - `enum APIKeyStatus` (1609) / `enum APIKeyType` (1617) — lowercase
//!     values bound to Go `ent/apikey.Status` / `ent/apikey.Type`.
//!   - `input APIKeyWhereInput` (lines 1626-1740, ent-generated) — scalar
//!     predicates + `not`/`and`/`or` + `has<Edge>` booleans.
//!   - `input CreateAPIKeyInput` (lines 2865-2876, ent-generated; `userID`,
//!     `key`, `status` and `profiles` are skipped via `entgql.Skip...` —
//!     `internal/ent/schema/api_key.go:41-77` — and are service/column-set).
//!   - `input UpdateAPIKeyInput` (lines 7302-7310, ent-generated; only
//!     `name` + the scopes set/append/clear family — `type`/`projectID`/
//!     `key`/`status`/`profiles` are update-skipped in the ent schema).
//!   - apikey.graphql support types (snapshot lines 382-473): `APIKeyProfiles`,
//!     `APIKeyProfile`, `APIKeyQuota`, `APIKeyQuotaPeriod`,
//!     `APIKeyQuotaPastDuration`, `APIKeyQuotaCalendarDuration` plus the
//!     enums `ChannelTagsMatchMode` (398), `APIKeyQuotaPeriodType` (427),
//!     `APIKeyQuotaPastDurationUnit` (438), `APIKeyQuotaCalendarDurationUnit`
//!     (448). `APIKeyProfile.modelMappings` reuses the shared
//!     `crate::channel::ModelMapping` output type (same GraphQL type).
//!
//! Naming gotcha: async-graphql pascal-cases Rust type idents (`APIKey` →
//! `ApiKey`), so EVERY GraphQL type name in this file is pinned with
//! `#[graphql(name = "...")]` — the proven precedent is
//! `crates/conduit-openapi-graphql/src/model.rs`.
//!
//! Go reference implementations:
//!   - Query.apiKeys — `internal/server/gql/ent.resolvers.go:295`. Unlike
//!     `Query.channels`/`Query.models`, this resolver FIRST enforces
//!     `validatePaginationArgs(first, last)` (pagination.go: first/last
//!     required, positive, ≤ maxPaginationLimit = 1000), THEN remaps a
//!     `CREATED_AT` ordering to `ent.DefaultAPIKeyOrder` (= order by ID,
//!     gql_pagination.go:413) and delegates to ent `Paginate`.
//!   - Mutation.createAPIKey — `conduit.resolvers.go:395` →
//!     `biz.APIKeyService.CreateAPIKey` (`biz/api_key.go:309`): resolve the
//!     context user → reject an explicit `noauth` type ("reserved") →
//!     generate the key (`GenerateAPIKey`, api_key.go:168: prefix + "-" +
//!     64 hex chars) → per-project LIVE-name duplicate check (an ARCHIVED
//!     key still occupies its name — archiving is a status, not a soft
//!     delete) → create. Column defaults (schema/api_key.go): type `user`,
//!     status `enabled`, scopes `[read_channels, write_requests]`, profiles
//!     `&objects.APIKeyProfiles{}` (an EMPTY STRUCT, not NULL). Scopes:
//!     `service_account` gets the input scopes (or `[]`); `user` type
//!     always takes the column default (input scopes are ignored).
//!   - Mutation.updateAPIKey — `conduit.resolvers.go:400` → `UpdateAPIKey`
//!     (biz/api_key.go:396): get → `user` type rejects any scope mutation
//!     (length-checked, so empty lists pass) → `noauth` type rejects any
//!     update → the rename duplicate probe runs only when the name really
//!     changes and excludes self → SetNillableName + service_account-only
//!     scopes set/append/clear (clear runs last and wins).
//!   - Mutation.updateAPIKeyStatus — `conduit.resolvers.go:405` →
//!     `UpdateAPIKeyStatus` (biz/api_key.go:477): get → `noauth` rejected →
//!     SetStatus. NO transition restriction (archived can be re-enabled).
//!   - Mutation.rotateAPIKey — `conduit.resolvers.go:415` → `RotateAPIKey`
//!     (biz/api_key.go:818): get → `noauth` rejected → generate a new key
//!     value; ONLY `key` changes — status/name/scopes/profiles preserved.
//!   - Mutation.updateAPIKeyProfiles — `conduit.resolvers.go:410` →
//!     `UpdateAPIKeyProfiles` (biz/api_key.go:503): get → `noauth` rejected →
//!     validate profile names (non-empty, case-insensitive unique) → validate
//!     active profile exists in the list → validate filters/quota →
//!     SetProfiles → invalidate cache. Input tree:
//!     `UpdateAPIKeyProfilesInput` / `APIKeyProfileInput` / `APIKeyQuotaInput`
//!     / `APIKeyQuotaPeriodInput` / `APIKeyQuotaPastDurationInput` /
//!     `APIKeyQuotaCalendarDurationInput` (snapshot lines 9470-9511). The
//!     `cost` field uses the `DecimalInput` scalar (snapshot line 9), backed
//!     by `crate::scalars::DecimalInputScalar` (maps to the same Go
//!     `objects.Decimal` type as `Decimal`, per gqlgen.yml:96-98).
//!   - Mutation.bulkDisableAPIKeys / bulkEnableAPIKeys / bulkArchiveAPIKeys —
//!     `conduit.resolvers.go:420/432/444` → `BulkDisableAPIKeys` /
//!     `BulkEnableAPIKeys` / `BulkArchiveAPIKeys` (biz/api_key.go:802/807/812)
//!     → shared private `bulkUpdateAPIKeyStatus` helper (biz/api_key.go:751):
//!     empty ids is a no-op; every id must resolve; NO id may be `noauth`-type
//!     ("noauth type API key cannot be bulk <action>d"); bulk SetStatus.
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//! These belong to other task slices. They are deliberately absent here (not
//! silently dropped — tracked for follow-up):
//!
//!   - `APIKey.user: User` (Go force-resolver), `APIKey.project: Project!`,
//!     `APIKey.requests(...): RequestConnection!` — cross-domain edge fields
//!     into the user / project / request domains.
//!   - `APIKeyWhereInput.hasUserWith / hasProjectWith / hasRequestsWith` —
//!     they reference other entities' `*WhereInput` types not ported yet.
//!   - Remaining APIKey operations: the `apiKeyQuotaUsages` query (+
//!     `APIKeyProfileQuotaUsage` / `APIKeyQuotaUsage` / `APIKeyQuotaWindow`
//!     output types), and the APIKeyProfileTemplate domain
//!     (`LoadApiKeyProfileTemplateInput`, ...).
//!   - `input UpdateAPIKeyScopesInput` (snapshot line 378) is declared but
//!     referenced by NO operation in the snapshot; it stays with the slice
//!     that ports the owning surface.
//!   - `Project.apiKeys` / `User.apiKeys` connections and `Request.apiKey` —
//!     other domains' edge fields.
//!   - There is NO `apiKey(id: ID!)` single-object query in the snapshot;
//!     single-object lookup goes through the global `node`/`nodes` Relay
//!     queries (a separate slice). `type APIKey implements Node` is declared
//!     via the shared `Node` interface enum in `crate::channel`.

use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, InputObject, SimpleObject};

use crate::channel::{ModelMapping, ModelMappingInput, OrderDirection};
use crate::pagination::PageInfo;
use crate::scalars::{CursorScalar, DecimalInputScalar, DecimalScalar, TimeScalar};

// ---------------------------------------------------------------------------
// Enums (snapshot-exact value spellings; lowercase values are pinned because
// the default SCREAMING_SNAKE renaming would mangle them, and every type name
// is pinned because async-graphql pascal-cases `APIKey*` idents to `ApiKey*`)
// ---------------------------------------------------------------------------

/// `enum APIKeyStatus { enabled disabled archived }` — snapshot line 1609,
/// bound to Go `ent/apikey.Status` (default `enabled`, schema/api_key.go:63).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "APIKeyStatus")]
pub enum APIKeyStatus {
    #[graphql(name = "enabled")]
    Enabled,
    #[graphql(name = "disabled")]
    Disabled,
    #[graphql(name = "archived")]
    Archived,
}

/// `enum APIKeyType { user service_account noauth }` — snapshot line 1617,
/// bound to Go `ent/apikey.Type` (default `user`, schema/api_key.go:57).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "APIKeyType")]
pub enum APIKeyType {
    #[graphql(name = "user")]
    User,
    #[graphql(name = "service_account")]
    ServiceAccount,
    #[graphql(name = "noauth")]
    Noauth,
}

/// `enum APIKeyOrderField { CREATED_AT UPDATED_AT }` — snapshot lines
/// 1447-1450 (two values only; api keys have no NAME/STATUS ordering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "APIKeyOrderField")]
pub enum APIKeyOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
}

/// `enum ChannelTagsMatchMode { any all none }` — snapshot lines 398-402,
/// bound to Go `objects.ChannelTagsMatchMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ChannelTagsMatchMode {
    #[graphql(name = "any")]
    Any,
    #[graphql(name = "all")]
    All,
    #[graphql(name = "none")]
    None,
}

/// `enum APIKeyQuotaPeriodType { all_time past_duration calendar_duration }`
/// — snapshot lines 427-431.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "APIKeyQuotaPeriodType")]
pub enum APIKeyQuotaPeriodType {
    #[graphql(name = "all_time")]
    AllTime,
    #[graphql(name = "past_duration")]
    PastDuration,
    #[graphql(name = "calendar_duration")]
    CalendarDuration,
}

/// `enum APIKeyQuotaPastDurationUnit { minute hour day }` — snapshot lines
/// 438-442.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "APIKeyQuotaPastDurationUnit")]
pub enum APIKeyQuotaPastDurationUnit {
    #[graphql(name = "minute")]
    Minute,
    #[graphql(name = "hour")]
    Hour,
    #[graphql(name = "day")]
    Day,
}

/// `enum APIKeyQuotaCalendarDurationUnit { day month }` — snapshot lines
/// 448-451.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "APIKeyQuotaCalendarDurationUnit")]
pub enum APIKeyQuotaCalendarDurationUnit {
    #[graphql(name = "day")]
    Day,
    #[graphql(name = "month")]
    Month,
}

// ---------------------------------------------------------------------------
// Output object types — apikey.graphql profiles subtree (snapshot 382-473).
// Go binds these to `objects.APIKeyProfiles` et al via `@goModel`; only the
// OUTPUT side is in scope here — the input twins belong to the pending
// `updateAPIKeyProfiles` slice (module doc).
// ---------------------------------------------------------------------------

/// `type APIKeyQuotaPastDuration { value: Int! unit:
/// APIKeyQuotaPastDurationUnit! }` — snapshot lines 466-469.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject)]
#[graphql(name = "APIKeyQuotaPastDuration")]
pub struct APIKeyQuotaPastDuration {
    pub value: i64,
    pub unit: APIKeyQuotaPastDurationUnit,
}

/// `type APIKeyQuotaCalendarDuration { unit:
/// APIKeyQuotaCalendarDurationUnit! }` — snapshot lines 471-473.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject)]
#[graphql(name = "APIKeyQuotaCalendarDuration")]
pub struct APIKeyQuotaCalendarDuration {
    pub unit: APIKeyQuotaCalendarDurationUnit,
}

/// `type APIKeyQuotaPeriod` — snapshot lines 460-464.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject)]
#[graphql(name = "APIKeyQuotaPeriod")]
pub struct APIKeyQuotaPeriod {
    // `type` is a Rust keyword; the GraphQL field name is pinned.
    #[graphql(name = "type")]
    pub period_type: APIKeyQuotaPeriodType,
    pub past_duration: Option<APIKeyQuotaPastDuration>,
    pub calendar_duration: Option<APIKeyQuotaCalendarDuration>,
}

/// `type APIKeyQuota { requests: Int totalTokens: Int cost: Decimal
/// period: APIKeyQuotaPeriod! }` — snapshot lines 453-458. `cost` uses the
/// crate's `Decimal` scalar shim (Go `objects.Decimal`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "APIKeyQuota")]
pub struct APIKeyQuota {
    pub requests: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost: Option<DecimalScalar>,
    pub period: APIKeyQuotaPeriod,
}

/// `type APIKeyProfile` — snapshot lines 387-396. `modelMappings` reuses the
/// shared `ModelMapping` output type declared in `crate::channel` (the
/// snapshot has exactly one `ModelMapping` GraphQL type).
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyProfile")]
pub struct APIKeyProfile {
    pub name: String,
    pub model_mappings: Option<Vec<ModelMapping>>,
    // All-caps acronym tags: default camelCase would emit `channelIds` /
    // `modelIds`.
    #[graphql(name = "channelIDs")]
    pub channel_ids: Option<Vec<i64>>,
    pub channel_tags: Option<Vec<String>>,
    pub channel_tags_match_mode: Option<ChannelTagsMatchMode>,
    #[graphql(name = "modelIDs")]
    pub model_ids: Option<Vec<String>>,
    pub valid_from: Option<TimeScalar>,
    pub valid_until: Option<TimeScalar>,
    pub quota: Option<APIKeyQuota>,
    pub load_balance_strategy: Option<String>,
    pub max_concurrent_requests: Option<i64>,
}

/// `type APIKeyProfiles { activeProfile: String! profiles: [APIKeyProfile!] }`
/// — snapshot lines 382-385. The ent column default is the EMPTY STRUCT
/// `&objects.APIKeyProfiles{}` (schema/api_key.go:71-77), so a freshly
/// created key carries `{ activeProfile: "", profiles: null }`, not `null`.
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyProfiles")]
pub struct APIKeyProfiles {
    pub active_profile: String,
    pub profiles: Option<Vec<APIKeyProfile>>,
}

/// `type APIKey implements Node` — snapshot lines 1343-1400, scalar and
/// apikey-domain fields only. The `user`/`project`/`requests` cross-domain
/// edge fields are pending (module doc).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "APIKey", complex)]
pub struct APIKey {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    // All-caps acronym tags: default camelCase would emit `userId` /
    // `projectId`. `userID` is nullable — ent `user_id` is Optional.
    #[graphql(name = "userID")]
    pub user_id: Option<ID>,
    #[graphql(name = "projectID")]
    pub project_id: ID,
    pub key: String,
    pub name: String,
    #[graphql(name = "type")]
    pub key_type: APIKeyType,
    pub status: APIKeyStatus,
    pub scopes: Option<Vec<String>>,
    pub profiles: Option<APIKeyProfiles>,
}

#[ComplexObject]
impl APIKey {
    /// Resolve the creator shown by the retained API-key UI. API keys created
    /// without a user (for example service-owned/imported rows) return `null`.
    async fn user(&self, ctx: &Context<'_>) -> Result<Option<crate::user::User>, String> {
        let Some(user_id) = self.user_id.clone() else {
            return Ok(None);
        };

        let services = crate::user::user_query_services(ctx)?;
        let connection = services
            .users(crate::user::UserConnectionArgs {
                first: Some(1),
                where_filter: Some(crate::user::UserWhereInput {
                    id: Some(user_id),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .map_err(|err| err.to_string())?;

        Ok(connection
            .edges
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .find_map(|edge| edge.node))
    }
}

/// `type APIKeyEdge { node: APIKey cursor: Cursor! }` — snapshot line 1421.
/// `node` is nullable in the contract (ent emits nullable edge nodes).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyEdge")]
pub struct APIKeyEdge {
    pub node: Option<APIKey>,
    pub cursor: CursorScalar,
}

/// `type APIKeyConnection` — snapshot line 1404. `edges` is a nullable list
/// of nullable edges (`[APIKeyEdge]`), exactly as ent generates it.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "APIKeyConnection")]
pub struct APIKeyConnection {
    pub edges: Option<Vec<Option<APIKeyEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

// ---------------------------------------------------------------------------
// Input object types
// ---------------------------------------------------------------------------

/// `input CreateAPIKeyInput` — snapshot lines 2865-2876 (ent-generated).
///
/// Go parity (`internal/ent/schema/api_key.go:41-77`): `userID`, `key`,
/// `status` and `profiles` are skipped via `entgql.Skip(MutationCreateInput |
/// MutationUpdateInput)` — they are server-set (creator context, generated
/// prefix-`-`-hex value, default `enabled`, default empty profiles struct).
/// `type` and `scopes` are accepted but `user`-type keys ignore the supplied
/// scopes in favor of the column default (`biz/api_key.go:309`).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "CreateAPIKeyInput")]
pub struct CreateAPIKeyInput {
    pub name: String,
    /// API Key type (nullable on input; service defaults to `user`).
    #[graphql(name = "type")]
    pub key_type: Option<APIKeyType>,
    /// Caller-supplied scopes; honored only for `service_account` keys.
    pub scopes: Option<Vec<String>>,
    #[graphql(name = "projectID")]
    pub project_id: ID,
    /// Optional initial policy, persisted in the same INSERT as the key.
    pub profiles: Option<UpdateAPIKeyProfilesInput>,
}

/// `input UpdateAPIKeyInput` — snapshot lines 7302-7310 (ent-generated). Only
/// `name` + the scopes set/append/clear family is mutable through this input
/// (the schema skips `type`/`projectID`/`key`/`status`/`profiles`).
///
/// Go parity (`biz/api_key.go:396`):
///   - `user`-type keys REJECT any scope mutation that touches scopes
///     (length-checked, so empty `scopes` + no append + no clear passes).
///   - `noauth`-type keys reject every update.
///   - The rename duplicate probe runs only when the name really changes and
///     excludes self.
///   - `service_account` scope mutations apply as: set → `SetScopes`;
///     append → `AppendScopes`; clear → `ClearScopes` (clear runs last and
///     wins, matching the Go else-if chain order).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateAPIKeyInput")]
pub struct UpdateAPIKeyInput {
    pub name: Option<String>,
    pub scopes: Option<Vec<String>>,
    pub append_scopes: Option<Vec<String>>,
    pub clear_scopes: Option<bool>,
}

/// `input APIKeyQuotaPastDurationInput { value: Int! unit:
/// APIKeyQuotaPastDurationUnit! }` — snapshot lines 9504-9507 (conduit.graphql
/// lines 428-431). Mirrors the OUTPUT `APIKeyQuotaPastDuration` shape exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "APIKeyQuotaPastDurationInput")]
pub struct APIKeyQuotaPastDurationInput {
    pub value: i64,
    pub unit: APIKeyQuotaPastDurationUnit,
}

/// `input APIKeyQuotaCalendarDurationInput { unit:
/// APIKeyQuotaCalendarDurationUnit! }` — snapshot lines 9509-9511
/// (conduit.graphql lines 439-441).
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "APIKeyQuotaCalendarDurationInput")]
pub struct APIKeyQuotaCalendarDurationInput {
    pub unit: APIKeyQuotaCalendarDurationUnit,
}

/// `input APIKeyQuotaPeriodInput { type: APIKeyQuotaPeriodType! pastDuration:
/// APIKeyQuotaPastDurationInput calendarDuration:
/// APIKeyQuotaCalendarDurationInput }` — snapshot lines 9497-9501
/// (conduit.graphql lines 416-420). `type` is a Rust keyword; the GraphQL name
/// is pinned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "APIKeyQuotaPeriodInput")]
pub struct APIKeyQuotaPeriodInput {
    #[graphql(name = "type")]
    pub period_type: APIKeyQuotaPeriodType,
    #[graphql(name = "pastDuration")]
    pub past_duration: Option<APIKeyQuotaPastDurationInput>,
    #[graphql(name = "calendarDuration")]
    pub calendar_duration: Option<APIKeyQuotaCalendarDurationInput>,
}

/// `input APIKeyQuotaInput { requests: Int totalTokens: Int cost: DecimalInput
/// period: APIKeyQuotaPeriodInput! }` — snapshot lines 9491-9495
/// (conduit.graphql lines 409-414). `cost` uses the `DecimalInput` scalar
/// (snapshot line 9), which maps to the same Go type as `Decimal`
/// (gqlgen.yml:96-98).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "APIKeyQuotaInput")]
pub struct APIKeyQuotaInput {
    pub requests: Option<i64>,
    #[graphql(name = "totalTokens")]
    pub total_tokens: Option<i64>,
    pub cost: Option<DecimalInputScalar>,
    pub period: APIKeyQuotaPeriodInput,
}

/// `input APIKeyProfileInput` — snapshot lines 9476-9484 (conduit.graphql
/// lines 338-347). Field names mirror the OUTPUT `APIKeyProfile` exactly —
/// the all-caps acronym tags (`channelIDs`, `modelIDs`) are pinned.
///
/// Go parity (`internal/objects/apikey.go:14-24`): `modelMappings`,
/// `channelIDs`, `channelTags`, `channelTagsMatchMode`, `modelIDs`, `quota`,
/// `loadBalanceStrategy` are all optional; `name` is required.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "APIKeyProfileInput")]
pub struct APIKeyProfileInput {
    pub name: String,
    #[graphql(name = "modelMappings")]
    pub model_mappings: Option<Vec<ModelMappingInput>>,
    #[graphql(name = "channelIDs")]
    pub channel_ids: Option<Vec<i64>>,
    #[graphql(name = "channelTags")]
    pub channel_tags: Option<Vec<String>>,
    #[graphql(name = "channelTagsMatchMode")]
    pub channel_tags_match_mode: Option<ChannelTagsMatchMode>,
    #[graphql(name = "modelIDs")]
    pub model_ids: Option<Vec<String>>,
    pub valid_from: Option<TimeScalar>,
    pub valid_until: Option<TimeScalar>,
    pub quota: Option<APIKeyQuotaInput>,
    #[graphql(name = "loadBalanceStrategy")]
    pub load_balance_strategy: Option<String>,
    pub max_concurrent_requests: Option<i64>,
}

/// `input UpdateAPIKeyProfilesInput { activeProfile: String! profiles:
/// [APIKeyProfileInput!] }` — snapshot lines 9470-9473 (conduit.graphql
/// lines 333-336). `profiles` is nullable (Go `[]APIKeyProfile` slice).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateAPIKeyProfilesInput")]
pub struct UpdateAPIKeyProfilesInput {
    #[graphql(name = "activeProfile")]
    pub active_profile: String,
    pub profiles: Option<Vec<APIKeyProfileInput>>,
}

/// `input APIKeyOrder { direction: OrderDirection! = ASC field:
/// APIKeyOrderField! }` — snapshot lines 1434-1443.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "APIKeyOrder")]
pub struct APIKeyOrder {
    /// Defaults to ASC when omitted, matching the ent-generated contract.
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: APIKeyOrderField,
}

/// `input APIKeyWhereInput` — snapshot lines 1626-1740 (ent-generated predicate
/// grammar). Implemented: `not`/`and`/`or`, every scalar-field predicate family,
/// and the three `has<Edge>: Boolean` existence predicates. The three
/// `has<Edge>With: [<Other>WhereInput!]` fields are pending (they reference
/// other entities' WhereInputs — see module doc).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
#[graphql(name = "APIKeyWhereInput")]
pub struct APIKeyWhereInput {
    pub not: Option<Box<APIKeyWhereInput>>,
    pub and: Option<Vec<APIKeyWhereInput>>,
    pub or: Option<Vec<APIKeyWhereInput>>,
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
    // user_id field predicates (all-caps acronym: every name pinned)
    #[graphql(name = "userID")]
    pub user_id: Option<ID>,
    #[graphql(name = "userIDNEQ")]
    pub user_id_neq: Option<ID>,
    #[graphql(name = "userIDIn")]
    pub user_id_in: Option<Vec<ID>>,
    #[graphql(name = "userIDNotIn")]
    pub user_id_not_in: Option<Vec<ID>>,
    #[graphql(name = "userIDIsNil")]
    pub user_id_is_nil: Option<bool>,
    #[graphql(name = "userIDNotNil")]
    pub user_id_not_nil: Option<bool>,
    // project_id field predicates (all-caps acronym: every name pinned)
    #[graphql(name = "projectID")]
    pub project_id: Option<ID>,
    #[graphql(name = "projectIDNEQ")]
    pub project_id_neq: Option<ID>,
    #[graphql(name = "projectIDIn")]
    pub project_id_in: Option<Vec<ID>>,
    #[graphql(name = "projectIDNotIn")]
    pub project_id_not_in: Option<Vec<ID>>,
    // key field predicates
    pub key: Option<String>,
    #[graphql(name = "keyNEQ")]
    pub key_neq: Option<String>,
    pub key_in: Option<Vec<String>>,
    pub key_not_in: Option<Vec<String>>,
    #[graphql(name = "keyGT")]
    pub key_gt: Option<String>,
    #[graphql(name = "keyGTE")]
    pub key_gte: Option<String>,
    #[graphql(name = "keyLT")]
    pub key_lt: Option<String>,
    #[graphql(name = "keyLTE")]
    pub key_lte: Option<String>,
    pub key_contains: Option<String>,
    pub key_has_prefix: Option<String>,
    pub key_has_suffix: Option<String>,
    pub key_equal_fold: Option<String>,
    pub key_contains_fold: Option<String>,
    // name field predicates
    pub name: Option<String>,
    #[graphql(name = "nameNEQ")]
    pub name_neq: Option<String>,
    pub name_in: Option<Vec<String>>,
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
    pub name_has_prefix: Option<String>,
    pub name_has_suffix: Option<String>,
    pub name_equal_fold: Option<String>,
    pub name_contains_fold: Option<String>,
    // type field predicates (`type` is a Rust keyword; GraphQL name pinned)
    #[graphql(name = "type")]
    pub key_type: Option<APIKeyType>,
    #[graphql(name = "typeNEQ")]
    pub type_neq: Option<APIKeyType>,
    #[graphql(name = "typeIn")]
    pub type_in: Option<Vec<APIKeyType>>,
    #[graphql(name = "typeNotIn")]
    pub type_not_in: Option<Vec<APIKeyType>>,
    // status field predicates
    pub status: Option<APIKeyStatus>,
    #[graphql(name = "statusNEQ")]
    pub status_neq: Option<APIKeyStatus>,
    pub status_in: Option<Vec<APIKeyStatus>>,
    pub status_not_in: Option<Vec<APIKeyStatus>>,
    // edge existence predicates (`has<Edge>With` variants pending — they
    // reference other entities' WhereInput types, see module doc)
    pub has_user: Option<bool>,
    pub has_project: Option<bool>,
    pub has_requests: Option<bool>,
}

// ---------------------------------------------------------------------------
// Ordering resolution (Go ent.resolvers.go:295 — CREATED_AT remaps to
// `ent.DefaultAPIKeyOrder` = order by primary key, gql_pagination.go:413)
// ---------------------------------------------------------------------------

/// Internal ordering terms the service layer receives. `Id` is NOT part of the
/// GraphQL `APIKeyOrderField` enum — it is ent's `DefaultAPIKeyOrder` (order by
/// primary key), which the Go resolver substitutes when the client asks for
/// `CREATED_AT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum APIKeyOrderTerm {
    /// ent `DefaultAPIKeyOrder` — ascending/descending by row ID.
    Id,
    UpdatedAt,
}

/// The resolver-lowered ordering selection handed to
/// [`ApiKeyQueryServices::api_keys`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct APIKeyOrderSelection {
    pub direction: OrderDirection,
    pub term: APIKeyOrderTerm,
}

/// Lower the GraphQL `orderBy` argument into a service-level selection,
/// mirroring Go `Query.apiKeys` (ent.resolvers.go:295): a `CREATED_AT` request
/// is remapped to `ent.DefaultAPIKeyOrder` (order by ID) with the requested
/// direction preserved; `UPDATED_AT` maps one-to-one.
pub fn resolve_apikey_order(order_by: Option<APIKeyOrder>) -> Option<APIKeyOrderSelection> {
    order_by.map(|order| APIKeyOrderSelection {
        direction: order.direction,
        term: match order.field {
            APIKeyOrderField::CreatedAt => APIKeyOrderTerm::Id,
            APIKeyOrderField::UpdatedAt => APIKeyOrderTerm::UpdatedAt,
        },
    })
}

// ---------------------------------------------------------------------------
// Service traits (host-injected, mirroring the Go resolver's dependencies:
// `r.client.APIKey` for the connection query and `r.apiKeyService` for the
// CRUD mutations). The host wires real repository / `biz.APIKeyService`
// implementations; tests use the in-memory double defined under `tests`.
// ---------------------------------------------------------------------------

/// Error surface for the api-key services. Messages mirror the Go error
/// strings so frontend error handling stays stable:
///   - duplicate name — `xerrors.DuplicateNameError("API Key", name)`
///     (`internal/pkg/xerrors/graphql.go:104`: "%s name '%s' already exists").
///   - reserved `noauth` create — `"noauth type API key is reserved"`
///     (`biz/api_key.go:318`).
///   - `noauth` / `user`-scope rejections — `biz/api_key.go:407-414`.
///   - not found — wrapped ent `ent: apikey not found`.
///   - wrapped create/update/rotate/query failures — the
///     `fmt.Errorf("failed to ...: %w")` prefixes in `biz/api_key.go`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum APIKeyServiceError {
    #[error("api key service is not available")]
    ServiceUnavailable,
    #[error("API Key name '{0}' already exists")]
    DuplicateName(String),
    #[error("noauth type API key is reserved")]
    NoauthReserved,
    #[error("noauth type API key cannot be updated")]
    NoauthNotUpdatable,
    #[error("noauth type API key status cannot be updated")]
    NoauthStatusNotUpdatable,
    #[error("noauth type API key cannot be rotated")]
    NoauthNotRotatable,
    #[error("user type API key cannot update scopes")]
    UserTypeScopesImmutable,
    #[error("ent: apikey not found")]
    NotFound,
    #[error("failed to create API key: {0}")]
    Create(String),
    #[error("failed to update API key: {0}")]
    Update(String),
    #[error("failed to update API key status: {0}")]
    UpdateStatus(String),
    #[error("failed to rotate API key: {0}")]
    Rotate(String),
    #[error("failed to query API keys: {0}")]
    Query(String),
    #[error("failed to bulk {0} API keys: {1}")]
    BulkUpdate(String, String),
    #[error("noauth type API key profiles cannot be updated")]
    NoauthProfilesNotUpdatable,
    #[error("failed to update API key profiles: {0}")]
    UpdateProfiles(String),
    #[error("profile name cannot be empty")]
    ProfileNameEmpty,
    #[error("duplicate profile name: {0}")]
    DuplicateProfileName(String),
    #[error("active profile '{0}' does not exist in the profiles list")]
    ActiveProfileMissing(String),
}

/// Arguments for the `apiKeys` connection query, passed through from the
/// GraphQL layer verbatim (Go hands them straight to ent's `Paginate`).
#[derive(Debug, Clone, Default)]
pub struct APIKeyConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<APIKeyOrderSelection>,
    pub where_filter: Option<APIKeyWhereInput>,
}

/// Per-request API-key boundary. API keys are Project resources: every list,
/// detail and mutation must stay inside the explicitly selected Project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct APIKeyAccessScope {
    pub project_id: String,
}

pub(crate) fn api_key_access_scope(ctx: &Context<'_>) -> Result<APIKeyAccessScope, String> {
    let project_id = crate::policy::request_context(ctx)
        .and_then(|request| request.project_id.clone())
        .ok_or_else(|| "current project is required; send X-Project-ID".to_string())?;
    Ok(APIKeyAccessScope { project_id })
}

/// Backs `Query.apiKeys` (Go `ent.resolvers.go:295`: `r.client.APIKey.Query()
/// .Paginate(...)`). The host wires the real repository; tests use an
/// in-memory store.
#[async_trait::async_trait]
pub trait ApiKeyQueryServices: Send + Sync {
    async fn api_keys(
        &self,
        scope: &APIKeyAccessScope,
        args: APIKeyConnectionArgs,
    ) -> Result<APIKeyConnection, APIKeyServiceError>;

    async fn api_key(
        &self,
        scope: &APIKeyAccessScope,
        id: &str,
    ) -> Result<Option<APIKey>, APIKeyServiceError>;
}

/// Backs the four CRUD mutations (Go `biz.APIKeyService`). `id` is the raw
/// GraphQL `ID!` scalar string; Go decodes it into `objects.GUID` and passes
/// `.ID` (int) to the service — concrete hosts perform the same decode.
#[async_trait::async_trait]
pub trait ApiKeyMutationServices: Send + Sync {
    /// Mirrors `APIKeyService.CreateAPIKey` (biz/api_key.go:309): resolve the
    /// context user → reject an explicit `noauth` type ("reserved") →
    /// generate the key → per-project LIVE-name duplicate check (an ARCHIVED
    /// key still occupies its name) → create with column defaults
    /// (type `user`, status `enabled`, scopes
    /// `[read_channels, write_requests]`, profiles = empty struct).
    async fn create_api_key(
        &self,
        scope: &APIKeyAccessScope,
        current_user_id: Option<i64>,
        input: CreateAPIKeyInput,
    ) -> Result<APIKey, APIKeyServiceError>;

    /// Mirrors `APIKeyService.UpdateAPIKey` (biz/api_key.go:396): get →
    /// `user`-type rejects any non-empty scope mutation → `noauth`-type
    /// rejects any update → rename duplicate probe (excluding self) →
    /// `SetNillableName` + `service_account`-only scope set/append/clear.
    async fn update_api_key(
        &self,
        scope: &APIKeyAccessScope,
        id: &str,
        input: UpdateAPIKeyInput,
    ) -> Result<APIKey, APIKeyServiceError>;

    /// Mirrors `APIKeyService.UpdateAPIKeyStatus` (biz/api_key.go:477): get →
    /// `noauth`-type rejected → `SetStatus`. NO transition restriction
    /// (archived keys can be re-enabled).
    async fn update_api_key_status(
        &self,
        scope: &APIKeyAccessScope,
        id: &str,
        status: APIKeyStatus,
    ) -> Result<APIKey, APIKeyServiceError>;

    /// Mirrors `APIKeyService.RotateAPIKey` (biz/api_key.go:818): get →
    /// `noauth`-type rejected → generate a new key value; ONLY `key` changes
    /// — status/name/scopes/profiles preserved.
    async fn rotate_api_key(
        &self,
        scope: &APIKeyAccessScope,
        id: &str,
    ) -> Result<APIKey, APIKeyServiceError>;

    /// Mirrors `APIKeyService.UpdateAPIKeyProfiles` (biz/api_key.go:503): get
    /// → `noauth`-type rejected ("noauth type API key profiles cannot be
    /// updated") → validate profile names unique (case-insensitive, non-empty),
    /// validate active profile exists, validate filters/quota → SetProfiles →
    /// invalidate cache.
    async fn update_api_key_profiles(
        &self,
        scope: &APIKeyAccessScope,
        id: &str,
        input: UpdateAPIKeyProfilesInput,
    ) -> Result<APIKey, APIKeyServiceError>;

    /// Mirrors `APIKeyService.BulkDisableAPIKeys` (biz/api_key.go:802) →
    /// `bulkUpdateAPIKeyStatus` (biz/api_key.go:751): empty ids is a no-op;
    /// all ids must exist; NO id may be a `noauth`-type key; bulk SetStatus
    /// (disabled).
    async fn bulk_disable_api_keys(
        &self,
        scope: &APIKeyAccessScope,
        ids: Vec<String>,
    ) -> Result<(), APIKeyServiceError>;

    /// Mirrors `APIKeyService.BulkEnableAPIKeys` (biz/api_key.go:807):
    /// same shape as [`Self::bulk_disable_api_keys`] with status `enabled`.
    async fn bulk_enable_api_keys(
        &self,
        scope: &APIKeyAccessScope,
        ids: Vec<String>,
    ) -> Result<(), APIKeyServiceError>;

    /// Mirrors `APIKeyService.BulkArchiveAPIKeys` (biz/api_key.go:812):
    /// same shape as [`Self::bulk_disable_api_keys`] with status `archived`.
    async fn bulk_archive_api_keys(
        &self,
        scope: &APIKeyAccessScope,
        ids: Vec<String>,
    ) -> Result<(), APIKeyServiceError>;
}

/// Resolves the injected [`ApiKeyQueryServices`] from the async-graphql data
/// bag; absent wiring surfaces the "service is not available" failure mode
/// (same convention as `channel::channel_query_services`).
pub(crate) fn apikey_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ApiKeyQueryServices>, String> {
    match ctx.data::<Arc<dyn ApiKeyQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(APIKeyServiceError::ServiceUnavailable.to_string()),
    }
}

/// Resolves the injected [`ApiKeyMutationServices`] from the data bag.
pub(crate) fn apikey_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ApiKeyMutationServices>, String> {
    match ctx.data::<Arc<dyn ApiKeyMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(APIKeyServiceError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::ID;

    use super::*;
    use crate::pagination::connection_from_offset_page;
    use crate::user::{
        User, UserConnection, UserConnectionArgs, UserEdge, UserQueryServices, UserServiceError,
        UserStatus,
    };
    use crate::{AdminSchema, admin_schema_builder};

    type TestError = Box<dyn std::error::Error>;

    // ---------------------------------------------------------------------
    // In-memory service double. Mirrors the Go `biz.APIKeyService` call
    // sequences without DB/HTTP; the connection query mirrors the thin ent
    // delegation (no predicate lowering — `where` is recorded and passed
    // through, as in Go where ent lowers it).
    // ---------------------------------------------------------------------

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    /// Default user-type scopes (Go column default, schema/api_key.go:63):
    /// `[read_channels, write_requests]`.
    fn default_user_scopes() -> Vec<String> {
        vec!["read_channels".to_owned(), "write_requests".to_owned()]
    }

    fn sample_apikey(id: i64, name: &str, key_type: APIKeyType) -> APIKey {
        APIKey {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            user_id: None,
            project_id: ID::from("1"),
            key: format!("axd-{id:064x}"),
            name: name.to_owned(),
            key_type,
            status: APIKeyStatus::Enabled,
            scopes: Some(default_user_scopes()),
            profiles: None,
        }
    }

    #[derive(Default, Clone)]
    struct InMemoryApiKeyService {
        api_keys: Arc<Mutex<Vec<APIKey>>>,
        users: Arc<Mutex<Vec<User>>>,
        captured_query_args: Arc<Mutex<Vec<APIKeyConnectionArgs>>>,
    }

    #[async_trait::async_trait]
    impl UserQueryServices for InMemoryApiKeyService {
        async fn users(
            &self,
            args: UserConnectionArgs,
        ) -> Result<UserConnection, UserServiceError> {
            let requested_id = args.where_filter.and_then(|where_input| where_input.id);
            let users = lock(&self.users);
            let matching = users
                .iter()
                .filter(|user| {
                    requested_id
                        .as_ref()
                        .is_none_or(|id| user.id.as_str() == id.as_str())
                })
                .cloned()
                .collect::<Vec<_>>();
            let total_count = matching.len() as i64;
            Ok(UserConnection {
                edges: Some(
                    matching
                        .into_iter()
                        .enumerate()
                        .map(|(index, user)| {
                            Some(UserEdge {
                                node: Some(user),
                                cursor: CursorScalar(index.to_string()),
                            })
                        })
                        .collect(),
                ),
                page_info: PageInfo::empty(false, false),
                total_count,
            })
        }

        async fn roles_for_user(
            &self,
            _user_id: &str,
            _args: crate::role::RoleConnectionArgs,
        ) -> Result<crate::role::RoleConnection, UserServiceError> {
            Ok(crate::role::RoleConnection {
                edges: Some(Vec::new()),
                page_info: PageInfo::empty(false, false),
                total_count: 0,
            })
        }

        async fn project_users(
            &self,
            _project_id: &str,
        ) -> Result<Vec<crate::user::UserProject>, UserServiceError> {
            Ok(Vec::new())
        }
    }

    #[async_trait::async_trait]
    impl ApiKeyQueryServices for InMemoryApiKeyService {
        async fn api_keys(
            &self,
            _scope: &APIKeyAccessScope,
            args: APIKeyConnectionArgs,
        ) -> Result<APIKeyConnection, APIKeyServiceError> {
            lock(&self.captured_query_args).push(args.clone());

            let mut nodes: Vec<APIKey> = lock(&self.api_keys).clone();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        APIKeyOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        APIKeyOrderTerm::UpdatedAt => a.updated_at.0.cmp(&b.updated_at.0),
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
            Ok(APIKeyConnection {
                edges: Some(
                    connection
                        .edges
                        .into_iter()
                        .map(|edge| {
                            Some(APIKeyEdge {
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

        async fn api_key(
            &self,
            _scope: &APIKeyAccessScope,
            id: &str,
        ) -> Result<Option<APIKey>, APIKeyServiceError> {
            Ok(lock(&self.api_keys)
                .iter()
                .find(|key| key.id.as_str() == id)
                .cloned())
        }
    }

    #[async_trait::async_trait]
    impl ApiKeyMutationServices for InMemoryApiKeyService {
        async fn create_api_key(
            &self,
            _scope: &APIKeyAccessScope,
            current_user_id: Option<i64>,
            input: CreateAPIKeyInput,
        ) -> Result<APIKey, APIKeyServiceError> {
            // Go biz/api_key.go:316-319: noauth is reserved (rejected on
            // create).
            if matches!(input.key_type, Some(APIKeyType::Noauth)) {
                return Err(APIKeyServiceError::NoauthReserved);
            }

            let mut guard = lock(&self.api_keys);
            // Per-project LIVE-name duplicate check (biz/api_key.go:343-357).
            // An ARCHIVED key still occupies its name (status, not soft
            // delete) — so we check every row regardless of status.
            if guard.iter().any(|existing| {
                existing.name == input.name && existing.project_id == input.project_id
            }) {
                return Err(APIKeyServiceError::DuplicateName(input.name));
            }

            let id = guard.len() as i64 + 1;
            // Column defaults (schema/api_key.go): type `user`, status
            // `enabled`, scopes `[read_channels, write_requests]`. The
            // `user`-type ignores input scopes; `service_account` honors them.
            let (key_type, scopes) = match input.key_type.unwrap_or(APIKeyType::User) {
                APIKeyType::User => (APIKeyType::User, default_user_scopes()),
                APIKeyType::ServiceAccount => {
                    (APIKeyType::ServiceAccount, input.scopes.unwrap_or_default())
                }
                APIKeyType::Noauth => (APIKeyType::Noauth, Vec::new()),
            };

            let created = APIKey {
                id: ID::from(id.to_string()),
                created_at: epoch(),
                updated_at: epoch(),
                user_id: current_user_id.map(|id| ID::from(id.to_string())),
                project_id: input.project_id,
                key: format!("axd-{id:064x}"),
                name: input.name,
                key_type,
                status: APIKeyStatus::Enabled,
                scopes: Some(scopes),
                profiles: None,
            };
            guard.push(created.clone());
            Ok(created)
        }

        async fn update_api_key(
            &self,
            _scope: &APIKeyAccessScope,
            id: &str,
            input: UpdateAPIKeyInput,
        ) -> Result<APIKey, APIKeyServiceError> {
            let mut guard = lock(&self.api_keys);
            let idx = guard
                .iter()
                .position(|k| k.id.as_str() == id)
                .ok_or_else(|| {
                    APIKeyServiceError::Update(APIKeyServiceError::NotFound.to_string())
                })?;

            let existing_type = guard[idx].key_type;
            let existing_name = guard[idx].name.clone();

            // biz/api_key.go:407-411: user-type rejects any scope mutation
            // (length-checked, so empty `scopes` + no append + no clear
            // passes).
            if existing_type == APIKeyType::User
                && (input.scopes.as_deref().is_some_and(|s| !s.is_empty())
                    || input
                        .append_scopes
                        .as_deref()
                        .is_some_and(|s| !s.is_empty())
                    || input.clear_scopes.unwrap_or(false))
            {
                return Err(APIKeyServiceError::UserTypeScopesImmutable);
            }

            // biz/api_key.go:413-415: noauth-type rejects any update.
            if existing_type == APIKeyType::Noauth {
                return Err(APIKeyServiceError::NoauthNotUpdatable);
            }

            // Rename duplicate probe (only when the name really changes and
            // excludes self — biz/api_key.go:421-440).
            if let Some(new_name) = &input.name
                && new_name != &existing_name
                && guard
                    .iter()
                    .any(|other| other.name == *new_name && other.id.as_str() != id)
            {
                return Err(APIKeyServiceError::DuplicateName(new_name.clone()));
            }

            let api_key = &mut guard[idx];

            // Apply fields.
            if let Some(name) = input.name {
                api_key.name = name;
            }
            // service_account-only scope mutations (clear runs last and wins).
            if existing_type == APIKeyType::ServiceAccount {
                if let Some(scopes) = input.scopes {
                    api_key.scopes = Some(scopes);
                }
                if let Some(append) = input.append_scopes {
                    api_key.scopes.get_or_insert_with(Vec::new).extend(append);
                }
                if input.clear_scopes.unwrap_or(false) {
                    api_key.scopes = Some(Vec::new());
                }
            }

            Ok(api_key.clone())
        }

        async fn update_api_key_status(
            &self,
            _scope: &APIKeyAccessScope,
            id: &str,
            status: APIKeyStatus,
        ) -> Result<APIKey, APIKeyServiceError> {
            let mut guard = lock(&self.api_keys);
            let Some(api_key) = guard.iter_mut().find(|k| k.id.as_str() == id) else {
                return Err(APIKeyServiceError::UpdateStatus(
                    APIKeyServiceError::NotFound.to_string(),
                ));
            };
            // biz/api_key.go:485-487: noauth-type rejected.
            if api_key.key_type == APIKeyType::Noauth {
                return Err(APIKeyServiceError::NoauthStatusNotUpdatable);
            }
            // NO transition restriction (archived can be re-enabled).
            api_key.status = status;
            Ok(api_key.clone())
        }

        async fn rotate_api_key(
            &self,
            _scope: &APIKeyAccessScope,
            id: &str,
        ) -> Result<APIKey, APIKeyServiceError> {
            let mut guard = lock(&self.api_keys);
            let Some(api_key) = guard.iter_mut().find(|k| k.id.as_str() == id) else {
                return Err(APIKeyServiceError::Rotate(
                    APIKeyServiceError::NotFound.to_string(),
                ));
            };
            // biz/api_key.go:826-828: noauth-type rejected.
            if api_key.key_type == APIKeyType::Noauth {
                return Err(APIKeyServiceError::NoauthNotRotatable);
            }
            // Only `key` changes — status/name/scopes/profiles preserved.
            let new_key = format!("axd-rotated-{}", api_key.id.as_str());
            api_key.key = new_key;
            Ok(api_key.clone())
        }

        async fn update_api_key_profiles(
            &self,
            _scope: &APIKeyAccessScope,
            id: &str,
            input: UpdateAPIKeyProfilesInput,
        ) -> Result<APIKey, APIKeyServiceError> {
            let mut guard = lock(&self.api_keys);
            let api_key = guard
                .iter_mut()
                .find(|k| k.id.as_str() == id)
                .ok_or_else(|| {
                    APIKeyServiceError::UpdateProfiles(APIKeyServiceError::NotFound.to_string())
                })?;
            // biz/api_key.go:511-513: noauth-type rejected.
            if api_key.key_type == APIKeyType::Noauth {
                return Err(APIKeyServiceError::NoauthProfilesNotUpdatable);
            }

            // biz/api_key.go:516-518 — validateProfileNames: case-insensitive,
            // non-empty, unique.
            let profiles = input.profiles.unwrap_or_default();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for p in &profiles {
                let name_lower = p.name.trim().to_lowercase();
                if name_lower.is_empty() {
                    return Err(APIKeyServiceError::ProfileNameEmpty);
                }
                if !seen.insert(name_lower) {
                    return Err(APIKeyServiceError::DuplicateProfileName(p.name.clone()));
                }
            }

            // biz/api_key.go:521-523 — validateActiveProfile: must match a
            // profile name (exact match, like Go `profile.Name ==
            // activeProfile`).
            if !input.active_profile.is_empty()
                && !profiles.iter().any(|p| p.name == input.active_profile)
            {
                return Err(APIKeyServiceError::ActiveProfileMissing(
                    input.active_profile.clone(),
                ));
            }

            // Lower each input profile to the OUTPUT shape and persist.
            let lowered: Vec<APIKeyProfile> = profiles
                .into_iter()
                .map(|p| APIKeyProfile {
                    name: p.name,
                    model_mappings: p.model_mappings.map(|m| {
                        m.into_iter()
                            .map(|mi| ModelMapping {
                                from: mi.from,
                                to: mi.to,
                            })
                            .collect()
                    }),
                    channel_ids: p.channel_ids,
                    channel_tags: p.channel_tags,
                    channel_tags_match_mode: p.channel_tags_match_mode,
                    model_ids: p.model_ids,
                    valid_from: p.valid_from,
                    valid_until: p.valid_until,
                    quota: p.quota.map(|q| APIKeyQuota {
                        requests: q.requests,
                        total_tokens: q.total_tokens,
                        cost: q.cost.map(|c| DecimalScalar(c.0)),
                        period: APIKeyQuotaPeriod {
                            period_type: q.period.period_type,
                            past_duration: q.period.past_duration.map(|pd| {
                                APIKeyQuotaPastDuration {
                                    value: pd.value,
                                    unit: pd.unit,
                                }
                            }),
                            calendar_duration: q
                                .period
                                .calendar_duration
                                .map(|cd| APIKeyQuotaCalendarDuration { unit: cd.unit }),
                        },
                    }),
                    load_balance_strategy: p.load_balance_strategy,
                    max_concurrent_requests: p.max_concurrent_requests,
                })
                .collect();
            api_key.profiles = Some(APIKeyProfiles {
                active_profile: input.active_profile,
                profiles: if lowered.is_empty() {
                    None
                } else {
                    Some(lowered)
                },
            });
            Ok(api_key.clone())
        }

        async fn bulk_disable_api_keys(
            &self,
            _scope: &APIKeyAccessScope,
            ids: Vec<String>,
        ) -> Result<(), APIKeyServiceError> {
            self.bulk_update_status(&ids, APIKeyStatus::Disabled, "disable")
                .await
        }

        async fn bulk_enable_api_keys(
            &self,
            _scope: &APIKeyAccessScope,
            ids: Vec<String>,
        ) -> Result<(), APIKeyServiceError> {
            self.bulk_update_status(&ids, APIKeyStatus::Enabled, "enable")
                .await
        }

        async fn bulk_archive_api_keys(
            &self,
            _scope: &APIKeyAccessScope,
            ids: Vec<String>,
        ) -> Result<(), APIKeyServiceError> {
            self.bulk_update_status(&ids, APIKeyStatus::Archived, "archive")
                .await
        }
    }

    impl InMemoryApiKeyService {
        /// Mirrors Go `APIKeyService.bulkUpdateAPIKeyStatus`
        /// (biz/api_key.go:751-799): empty ids is a no-op; every id must
        /// resolve; NO id may be `noauth`-type ("noauth type API key cannot be
        /// bulk <action>d"); bulk SetStatus.
        async fn bulk_update_status(
            &self,
            ids: &[String],
            status: APIKeyStatus,
            action: &str,
        ) -> Result<(), APIKeyServiceError> {
            if ids.is_empty() {
                return Ok(());
            }
            let mut guard = lock(&self.api_keys);
            // biz/api_key.go:759-768: count check — every id must exist.
            let found_count = guard
                .iter()
                .filter(|k| ids.iter().any(|i| i == k.id.as_str()))
                .count();
            if found_count != ids.len() {
                return Err(APIKeyServiceError::BulkUpdate(
                    action.to_string(),
                    format!(
                        "expected to find {} API keys, but found {}",
                        ids.len(),
                        found_count
                    ),
                ));
            }
            // biz/api_key.go:770-779: no `noauth`-type allowed.
            let noauth_exists = guard.iter().any(|k| {
                ids.iter().any(|i| i == k.id.as_str()) && k.key_type == APIKeyType::Noauth
            });
            if noauth_exists {
                return Err(APIKeyServiceError::BulkUpdate(
                    action.to_string(),
                    format!("noauth type API key cannot be bulk {action}d"),
                ));
            }
            // biz/api_key.go:788-795: bulk SetStatus.
            for k in guard.iter_mut() {
                if ids.iter().any(|i| i == k.id.as_str()) {
                    k.status = status;
                }
            }
            Ok(())
        }
    }

    fn schema_with(store: &InMemoryApiKeyService) -> AdminSchema {
        let query: Arc<dyn ApiKeyQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn ApiKeyMutationServices> = Arc::new(store.clone());
        let users: Arc<dyn UserQueryServices> = Arc::new(store.clone());
        let mut request = conduit_auth::request_context::RequestContext::new();
        let _ = request.set_principal(conduit_auth::Principal::user("1").with_owner(true));
        let _ = request.set_project_id("gid://conduit/Project/1");
        admin_schema_builder()
            .data(query)
            .data(mutation)
            .data(users)
            .data(request)
            .finish()
    }

    fn bare_schema() -> AdminSchema {
        crate::build_admin_schema()
    }

    // ---------------------------------------------------------------------
    // SDL block-parity helpers (delegate to the shared crate helper).
    // ---------------------------------------------------------------------

    fn assert_block_parity(
        sdl: &str,
        snapshot: &str,
        ours_header: &str,
        snapshot_header: &str,
        pending: &[&str],
    ) -> Result<(), TestError> {
        // Thin wrapper around the shared crate helper; kept to mirror the
        // channel.rs test layout (which has its own private copy).
        crate::sdl_parity::assert_block_parity(sdl, snapshot, ours_header, snapshot_header, pending)
    }

    fn assert_block_parity_with_extensions(
        sdl: &str,
        snapshot: &str,
        header: &str,
        extensions: &[&str],
    ) -> Result<(), TestError> {
        crate::sdl_parity::assert_block_parity_with_extensions(
            sdl,
            snapshot,
            header,
            header,
            &[],
            extensions,
        )
    }

    // ---------------------------------------------------------------------
    // SDL parity: object types
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_apikey_type_matches_snapshot_minus_pending_edges() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        // Cross-domain edge fields are the documented pending set.
        assert_block_parity(
            &sdl,
            &snapshot,
            "type APIKey",
            "type APIKey",
            &["project: Project!", "requests(…): RequestConnection!"],
        )?;
        // The implements clause must match the snapshot's declaration.
        assert!(
            sdl.contains("type APIKey implements Node {"),
            "generated SDL must declare `type APIKey implements Node`"
        );
        assert!(snapshot.contains("type APIKey implements Node {"));
        Ok(())
    }

    #[test]
    fn sdl_apikey_connection_and_edge_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type APIKeyConnection",
            "type APIKeyConnection",
            &[],
        )?;
        assert_block_parity(&sdl, &snapshot, "type APIKeyEdge", "type APIKeyEdge", &[])?;
        Ok(())
    }

    #[test]
    fn sdl_apikey_profiles_subtree_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "type APIKeyProfile",
            &[
                "validFrom: Time",
                "validUntil: Time",
                "maxConcurrentRequests: Int",
            ],
        )?;
        for header in [
            "type APIKeyProfiles",
            "type APIKeyQuota",
            "type APIKeyQuotaPeriod",
            "type APIKeyQuotaPastDuration",
            "type APIKeyQuotaCalendarDuration",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: input types
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_create_apikey_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "input CreateAPIKeyInput",
            &["profiles: UpdateAPIKeyProfilesInput"],
        )
    }

    #[test]
    fn sdl_update_apikey_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input UpdateAPIKeyInput",
            "input UpdateAPIKeyInput",
            &[],
        )
    }

    #[test]
    fn sdl_apikey_order_matches_snapshot_with_asc_default() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input APIKeyOrder",
            "input APIKeyOrder",
            &[],
        )?;
        // The block comparison strips default values; pin the `= ASC` default
        // of APIKeyOrder.direction exactly (snapshot line 1438).
        assert!(
            sdl.contains("direction: OrderDirection! = ASC"),
            "generated SDL must render the ASC default on APIKeyOrder.direction"
        );
        assert!(snapshot.contains("direction: OrderDirection! = ASC"));
        Ok(())
    }

    #[test]
    fn sdl_apikey_where_input_matches_snapshot_minus_pending_edge_filters() -> Result<(), TestError>
    {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        // The three `has<Edge>With` fields reference other entities'
        // WhereInput types (pending slices).
        assert_block_parity(
            &sdl,
            &snapshot,
            "input APIKeyWhereInput",
            "input APIKeyWhereInput",
            &[
                "hasUserWith: [UserWhereInput!]",
                "hasProjectWith: [ProjectWhereInput!]",
                "hasRequestsWith: [RequestWhereInput!]",
            ],
        )
    }

    // ---------------------------------------------------------------------
    // SDL parity: enums
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_apikey_enums_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        for header in [
            "enum APIKeyStatus",
            "enum APIKeyType",
            "enum APIKeyOrderField",
            "enum ChannelTagsMatchMode",
            "enum APIKeyQuotaPeriodType",
            "enum APIKeyQuotaPastDurationUnit",
            "enum APIKeyQuotaCalendarDurationUnit",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: root operation signatures
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_apikeys_query_and_crud_mutations_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;

        // Query.apiKeys — async-graphql renders arguments inline.
        assert!(
            sdl.contains(
                "apiKeys(after: Cursor, first: Int, before: Cursor, last: Int, \
                 orderBy: APIKeyOrder, where: APIKeyWhereInput): APIKeyConnection!"
            ),
            "generated SDL missing the apiKeys connection signature: {sdl}"
        );
        for token in [
            "after: Cursor",
            "first: Int",
            "before: Cursor",
            "last: Int",
            "orderBy: APIKeyOrder",
            "where: APIKeyWhereInput",
            "): APIKeyConnection!",
        ] {
            assert!(
                snapshot.contains(token),
                "snapshot missing apiKeys arg token `{token}`"
            );
        }

        // Mutations — flat one-line signatures in both dialects
        // (snapshot type Mutation, lines 819-823).
        for signature in [
            "createAPIKey(input: CreateAPIKeyInput!): APIKey!",
            "updateAPIKey(id: ID!, input: UpdateAPIKeyInput!): APIKey!",
            "updateAPIKeyStatus(id: ID!, status: APIKeyStatus!): APIKey!",
            "rotateAPIKey(id: ID!): APIKey!",
        ] {
            assert!(
                sdl.contains(signature),
                "generated SDL missing `{signature}`"
            );
            assert!(
                snapshot.contains(signature),
                "snapshot missing `{signature}`"
            );
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Ordering lowering (Go ent.resolvers.go:295)
    // ---------------------------------------------------------------------

    #[test]
    fn resolve_apikey_order_remaps_created_at_to_default_id_order() {
        let selection = resolve_apikey_order(Some(APIKeyOrder {
            direction: OrderDirection::Desc,
            field: APIKeyOrderField::CreatedAt,
        }));
        assert_eq!(
            selection,
            Some(APIKeyOrderSelection {
                direction: OrderDirection::Desc,
                term: APIKeyOrderTerm::Id,
            })
        );
    }

    #[test]
    fn resolve_apikey_order_maps_updated_at_one_to_one() {
        let selection = resolve_apikey_order(Some(APIKeyOrder {
            direction: OrderDirection::Asc,
            field: APIKeyOrderField::UpdatedAt,
        }));
        assert_eq!(selection.map(|s| s.term), Some(APIKeyOrderTerm::UpdatedAt));
        assert_eq!(resolve_apikey_order(None), None);
    }

    // ---------------------------------------------------------------------
    // Resolver: createAPIKey
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn create_apikey_user_type_uses_column_default_scopes() -> Result<(), TestError> {
        let store = InMemoryApiKeyService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createAPIKey(input: { name: "k1", projectID: "1" }) {
                        id name type status scopes
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let created = &data["createAPIKey"];
        assert_eq!(created["name"], "k1");
        // Column defaults: type `user`, status `enabled`, scopes
        // `[read_channels, write_requests]`.
        assert_eq!(created["type"], "user");
        assert_eq!(created["status"], "enabled");
        assert_eq!(created["scopes"][0], "read_channels");
        assert_eq!(created["scopes"][1], "write_requests");
        Ok(())
    }

    #[tokio::test]
    async fn create_apikey_resolves_frontend_user_shape() -> Result<(), TestError> {
        let store = InMemoryApiKeyService::default();
        lock(&store.users).push(User {
            id: ID::from("7"),
            created_at: epoch(),
            updated_at: epoch(),
            email: "owner@example.com".to_owned(),
            status: UserStatus::Activated,
            prefer_language: "en".to_owned(),
            first_name: "Admin".to_owned(),
            last_name: "User".to_owned(),
            avatar: None,
            is_owner: true,
            scopes: Some(Vec::new()),
        });
        let schema = schema_with(&store);

        let request = async_graphql::Request::new(
            r#"mutation {
                createAPIKey(input: { name: "frontend-key", projectID: "1" }) {
                    id createdAt updatedAt
                    user { id firstName lastName }
                    key name type status scopes
                }
            }"#,
        )
        .data(crate::me::CurrentUser { user_id: 7 });
        let resp = schema.execute(request).await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["createAPIKey"]["user"]["id"], "7");
        assert_eq!(data["createAPIKey"]["user"]["firstName"], "Admin");
        assert_eq!(data["createAPIKey"]["user"]["lastName"], "User");
        Ok(())
    }

    #[tokio::test]
    async fn create_apikey_service_account_honors_input_scopes() -> Result<(), TestError> {
        let store = InMemoryApiKeyService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createAPIKey(input: {
                        name: "svc",
                        type: service_account,
                        scopes: ["write:channels"],
                        projectID: "1"
                    }) { type scopes }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["createAPIKey"]["type"], "service_account");
        assert_eq!(data["createAPIKey"]["scopes"][0], "write:channels");
        Ok(())
    }

    #[tokio::test]
    async fn create_apikey_noauth_type_is_rejected_as_reserved() {
        let store = InMemoryApiKeyService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createAPIKey(input: { name: "x", type: noauth, projectID: "1" }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("noauth type API key is reserved"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn create_apikey_archived_key_occupies_name() {
        // biz/api_key.go:343-357: the duplicate-name probe considers every
        // existing key in the project regardless of status — archiving is a
        // status, not a soft delete.
        let store = InMemoryApiKeyService::default();
        let mut archived = sample_apikey(1, "reuse-me", APIKeyType::User);
        archived.status = APIKeyStatus::Archived;
        lock(&store.api_keys).push(archived);
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createAPIKey(input: { name: "reuse-me", projectID: "1" }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("API Key name 'reuse-me' already exists"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn create_apikey_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema
            .execute(
                r#"mutation {
                    createAPIKey(input: { name: "x", projectID: "1" }) { id }
                }"#,
            )
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("api key service is not available"),
            "unexpected error: {msg}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: updateAPIKey
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn update_apikey_renames_when_name_changes() -> Result<(), TestError> {
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(7, "old", APIKeyType::User));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKey(id: "7", input: { name: "new" }) { id name }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updateAPIKey"]["name"], "new");
        Ok(())
    }

    #[tokio::test]
    async fn update_apikey_user_type_rejects_scope_set() {
        // biz/api_key.go:407-411: user-type rejects any non-empty scope
        // mutation.
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(1, "u", APIKeyType::User));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKey(id: "1", input: { scopes: ["admin"] }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("user type API key cannot update scopes"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn update_apikey_noauth_type_rejects_any_update() {
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(2, "n", APIKeyType::Noauth));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKey(id: "2", input: { name: "renamed" }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("noauth type API key cannot be updated"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn update_apikey_service_account_applies_clear_last() -> Result<(), TestError> {
        // biz/api_key.go:444-456 (service_account scope mutation chain):
        // SetScopes → AppendScopes → ClearScopes (clear runs last and wins).
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(3, "svc", APIKeyType::ServiceAccount));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKey(id: "3", input: {
                        scopes: ["a"],
                        appendScopes: ["b"],
                        clearScopes: true
                    }) { scopes }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        // ClearScopes wins — empty list, but not null.
        assert_eq!(data["updateAPIKey"]["scopes"], serde_json::json!([]));
        Ok(())
    }

    #[tokio::test]
    async fn update_apikey_missing_id_surfaces_wrapped_not_found() {
        let store = InMemoryApiKeyService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateAPIKey(id: "404", input: { name: "x" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to update API key: ent: apikey not found"),
            "unexpected error: {msg}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: updateAPIKeyStatus
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn update_apikey_status_no_transition_restriction() -> Result<(), TestError> {
        // biz/api_key.go:477-494: NO transition restriction — an archived key
        // can be re-enabled.
        let store = InMemoryApiKeyService::default();
        let mut archived = sample_apikey(4, "arc", APIKeyType::User);
        archived.status = APIKeyStatus::Archived;
        lock(&store.api_keys).push(archived);
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKeyStatus(id: "4", status: enabled) { id status }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updateAPIKeyStatus"]["status"], "enabled");
        Ok(())
    }

    #[tokio::test]
    async fn update_apikey_status_noauth_rejected() {
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(5, "n", APIKeyType::Noauth));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKeyStatus(id: "5", status: disabled) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("noauth type API key status cannot be updated"),
            "unexpected error: {msg}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: rotateAPIKey
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn rotate_apikey_preserves_status_name_scopes() -> Result<(), TestError> {
        // biz/api_key.go:818-854: ONLY `key` changes — status / name /
        // scopes / profiles preserved.
        let store = InMemoryApiKeyService::default();
        let mut seeded = sample_apikey(6, "rot", APIKeyType::ServiceAccount);
        seeded.status = APIKeyStatus::Disabled;
        seeded.scopes = Some(vec!["custom".to_owned()]);
        let original_key = seeded.key.clone();
        lock(&store.api_keys).push(seeded);
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    rotateAPIKey(id: "6") { id key name status scopes }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let rotated = &data["rotateAPIKey"];
        // Key changed.
        assert_ne!(rotated["key"], original_key);
        // Everything else preserved.
        assert_eq!(rotated["name"], "rot");
        assert_eq!(rotated["status"], "disabled");
        assert_eq!(rotated["scopes"][0], "custom");
        Ok(())
    }

    #[tokio::test]
    async fn rotate_apikey_noauth_rejected() {
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(8, "n", APIKeyType::Noauth));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { rotateAPIKey(id: "8") { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("noauth type API key cannot be rotated"),
            "unexpected error: {msg}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: apiKeys connection query
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn apikeys_returns_connection_with_total_count() -> Result<(), TestError> {
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(1, "a", APIKeyType::User));
        lock(&store.api_keys).push(sample_apikey(2, "b", APIKeyType::User));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    apiKeys {
                        totalCount
                        edges { cursor node { id name } }
                        pageInfo { hasNextPage hasPreviousPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let connection = &data["apiKeys"];
        assert_eq!(connection["totalCount"], 2);
        assert_eq!(connection["edges"][0]["node"]["name"], "a");
        assert_eq!(connection["edges"][1]["node"]["id"], "2");
        assert_eq!(connection["pageInfo"]["hasNextPage"], false);
        Ok(())
    }

    #[tokio::test]
    async fn apikeys_created_at_order_remaps_to_default_id_term() -> Result<(), TestError> {
        // Go Query.apiKeys (ent.resolvers.go:295): CREATED_AT is replaced by
        // ent.DefaultAPIKeyOrder (ID) with direction preserved.
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(1, "a", APIKeyType::User));
        lock(&store.api_keys).push(sample_apikey(2, "b", APIKeyType::User));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    apiKeys(orderBy: { field: CREATED_AT, direction: DESC }) {
                        edges { node { id } }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&store.captured_query_args).clone();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].order_by,
            Some(APIKeyOrderSelection {
                direction: OrderDirection::Desc,
                term: APIKeyOrderTerm::Id,
            })
        );
        // Desc-by-ID ordering is observable in the page.
        let data = resp.data.into_json()?;
        assert_eq!(data["apiKeys"]["edges"][0]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn apikeys_passes_where_filter_through_to_service() -> Result<(), TestError> {
        let store = InMemoryApiKeyService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    apiKeys(where: {
                        nameContainsFold: "prod",
                        statusIn: [enabled, disabled],
                        typeIn: [user]
                    }) { totalCount }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&store.captured_query_args).clone();
        let filter = captured[0]
            .where_filter
            .clone()
            .ok_or("where filter not captured")?;
        assert_eq!(filter.name_contains_fold.as_deref(), Some("prod"));
        assert_eq!(
            filter.status_in,
            Some(vec![APIKeyStatus::Enabled, APIKeyStatus::Disabled])
        );
        assert_eq!(filter.type_in, Some(vec![APIKeyType::User]));
        Ok(())
    }

    #[tokio::test]
    async fn apikeys_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema.execute(r#"{ apiKeys { totalCount } }"#).await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("api key service is not available"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver: updateAPIKeyProfiles (biz/api_key.go:503)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn update_apikey_profiles_rejects_noauth_type() {
        // biz/api_key.go:511-513: noauth-type profiles cannot be updated.
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(1, "n", APIKeyType::Noauth));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKeyProfiles(id: "1", input: { activeProfile: "" }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("noauth type API key profiles cannot be updated"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn update_apikey_profiles_rejects_duplicate_profile_names() -> Result<(), TestError> {
        // biz/api_key.go:516-518 → validateProfileNames: case-insensitive
        // duplicate detection.
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(2, "k", APIKeyType::User));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKeyProfiles(id: "2", input: {
                        activeProfile: "Prod",
                        profiles: [
                            { name: "Prod" },
                            { name: "prod" }
                        ]
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("duplicate profile name: prod"),
            "unexpected error: {msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_apikey_profiles_rejects_missing_active_profile() -> Result<(), TestError> {
        // biz/api_key.go:521-523 → validateActiveProfile.
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(3, "k", APIKeyType::User));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKeyProfiles(id: "3", input: {
                        activeProfile: "Missing",
                        profiles: [{ name: "Real" }]
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("active profile 'Missing' does not exist"),
            "unexpected error: {msg}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn update_apikey_profiles_persists_full_input_tree() -> Result<(), TestError> {
        // biz/api_key.go:534-538 → SetProfiles; the input tree round-trips
        // through the resolver and is observable on the APIKey.profiles field.
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(4, "k", APIKeyType::User));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKeyProfiles(id: "4", input: {
                        activeProfile: "p1",
                        profiles: [{
                            name: "p1",
                            modelMappings: [{ from: "a", to: "b" }],
                            channelIDs: [1, 2],
                            channelTags: ["tag1"],
                            channelTagsMatchMode: all,
                            modelIDs: ["m1"],
                            quota: {
                                requests: 100,
                                totalTokens: 200,
                                cost: "1.50",
                                period: { type: past_duration, pastDuration: { value: 1, unit: hour } }
                            },
                            loadBalanceStrategy: "round-robin"
                        }]
                    }) {
                        profiles {
                            activeProfile
                            profiles {
                                name
                                channelIDs
                                modelIDs
                                channelTagsMatchMode
                                loadBalanceStrategy
                                quota { requests totalTokens cost period { type pastDuration { value unit } } }
                                modelMappings { from to }
                            }
                        }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let profiles = &data["updateAPIKeyProfiles"]["profiles"];
        assert_eq!(profiles["activeProfile"], "p1");
        let p1 = &profiles["profiles"][0];
        assert_eq!(p1["name"], "p1");
        assert_eq!(p1["channelIDs"][0], 1);
        assert_eq!(p1["modelIDs"][0], "m1");
        assert_eq!(p1["channelTagsMatchMode"], "all");
        assert_eq!(p1["loadBalanceStrategy"], "round-robin");
        assert_eq!(p1["quota"]["requests"], 100);
        assert_eq!(p1["quota"]["totalTokens"], 200);
        assert_eq!(p1["quota"]["cost"], "1.50");
        assert_eq!(p1["quota"]["period"]["type"], "past_duration");
        assert_eq!(p1["quota"]["period"]["pastDuration"]["value"], 1);
        assert_eq!(p1["quota"]["period"]["pastDuration"]["unit"], "hour");
        assert_eq!(p1["modelMappings"][0]["from"], "a");
        assert_eq!(p1["modelMappings"][0]["to"], "b");
        Ok(())
    }

    #[tokio::test]
    async fn update_apikey_profiles_missing_id_surfaces_not_found() {
        let store = InMemoryApiKeyService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateAPIKeyProfiles(id: "404", input: { activeProfile: "" }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to update API key profiles: ent: apikey not found"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver: bulkDisableAPIKeys / bulkEnableAPIKeys / bulkArchiveAPIKeys
    // (biz/api_key.go:751, 802, 807, 812)
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn bulk_disable_apikeys_noop_on_empty_ids() -> Result<(), TestError> {
        // biz/api_key.go:752-754: empty ids returns immediately with no work.
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(1, "a", APIKeyType::User));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { bulkDisableAPIKeys(ids: []) }"#)
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["bulkDisableAPIKeys"], true);
        // Status unchanged.
        assert_eq!(lock(&store.api_keys)[0].status, APIKeyStatus::Enabled);
        Ok(())
    }

    #[tokio::test]
    async fn bulk_disable_apikeys_updates_status() -> Result<(), TestError> {
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(1, "a", APIKeyType::User));
        lock(&store.api_keys).push(sample_apikey(2, "b", APIKeyType::ServiceAccount));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { bulkDisableAPIKeys(ids: ["1", "2"]) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["bulkDisableAPIKeys"], true);
        assert_eq!(lock(&store.api_keys)[0].status, APIKeyStatus::Disabled);
        assert_eq!(lock(&store.api_keys)[1].status, APIKeyStatus::Disabled);
        Ok(())
    }

    #[tokio::test]
    async fn bulk_enable_apikeys_re_enables_archived() -> Result<(), TestError> {
        // biz/api_key.go:751: NO transition restriction — bulk enable can
        // re-enable archived keys (mirrors the single-status update).
        let store = InMemoryApiKeyService::default();
        let mut a = sample_apikey(1, "a", APIKeyType::User);
        a.status = APIKeyStatus::Archived;
        lock(&store.api_keys).push(a);
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { bulkEnableAPIKeys(ids: ["1"]) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(lock(&store.api_keys)[0].status, APIKeyStatus::Enabled);
        Ok(())
    }

    #[tokio::test]
    async fn bulk_archive_apikeys_rejects_noauth() {
        // biz/api_key.go:770-779: NO id may be a `noauth`-type key.
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(1, "u", APIKeyType::User));
        lock(&store.api_keys).push(sample_apikey(2, "n", APIKeyType::Noauth));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { bulkArchiveAPIKeys(ids: ["1", "2"]) }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("noauth type API key cannot be bulk archived"),
            "unexpected error: {msg}"
        );
        // No partial mutation: the user key is NOT archived.
        assert_eq!(lock(&store.api_keys)[0].status, APIKeyStatus::Enabled);
    }

    #[tokio::test]
    async fn bulk_disable_apikeys_rejects_missing_ids() {
        // biz/api_key.go:759-768: every id must resolve.
        let store = InMemoryApiKeyService::default();
        lock(&store.api_keys).push(sample_apikey(1, "a", APIKeyType::User));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { bulkDisableAPIKeys(ids: ["1", "999"]) }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("expected to find 2 API keys, but found 1"),
            "unexpected error: {msg}"
        );
    }

    // -----------------------------------------------------------------
    // SDL parity: new mutation signatures + new input types
    // -----------------------------------------------------------------

    #[test]
    fn sdl_apikey_profiles_and_bulk_mutations_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;

        for signature in [
            "updateAPIKeyProfiles(id: ID!, input: UpdateAPIKeyProfilesInput!): APIKey!",
            "bulkDisableAPIKeys(ids: [ID!]!): Boolean!",
            "bulkEnableAPIKeys(ids: [ID!]!): Boolean!",
            "bulkArchiveAPIKeys(ids: [ID!]!): Boolean!",
        ] {
            assert!(
                sdl.contains(signature),
                "generated SDL missing `{signature}`\n--- SDL tail ---\n{}",
                sdl
            );
            assert!(
                snapshot.contains(signature),
                "snapshot missing `{signature}`"
            );
        }
        Ok(())
    }

    #[test]
    fn sdl_apikey_profiles_input_tree_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "input APIKeyProfileInput",
            &[
                "validFrom: Time",
                "validUntil: Time",
                "maxConcurrentRequests: Int",
            ],
        )?;
        for header in [
            "input UpdateAPIKeyProfilesInput",
            "input APIKeyQuotaInput",
            "input APIKeyQuotaPeriodInput",
            "input APIKeyQuotaPastDurationInput",
            "input APIKeyQuotaCalendarDurationInput",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }
}
