//! RUST-P12-001 S07 — Model domain CRUD GraphQL slice.
//!
//! Bounded scope (this file): the `models` connection query plus the
//! `createModel` / `updateModel` / `deleteModel` mutations and every GraphQL
//! type/input they reference. All shapes are copied field-for-field from the
//! captured contract snapshot `tests/contracts/admin_graphql_schema.graphql`:
//!
//!   - `type Model implements Node` (snapshot line 3585) — the base block is
//!     complete here (the ent Model schema declares NO edges, so unlike
//!     Channel there are no cross-domain edge fields to defer).
//!   - `type ModelConnection` / `type ModelEdge` (lines 3624-3650).
//!   - `input CreateModelInput` (lines 2977-3008, ent-generated; `status` is
//!     skipped via `entgql.SkipMutationCreateInput`, schema/model.go:50-52).
//!   - `input UpdateModelInput` (lines 7426-7459, ent-generated).
//!   - `input ModelWhereInput` (lines 3694-3843, ent-generated) — complete:
//!     scalar predicates + `not`/`and`/`or`; no edge predicates exist.
//!   - `input ModelOrder` / `enum ModelOrderField` (lines 3654-3671).
//!   - `enum ModelStatus` (3675) / `enum ModelType` (3683).
//!   - model.graphql support types (lines 8893-9081): `ModelCard(Input)` +
//!     reasoning/modalities/cost/limit sub-cards, `ModelSettings(Input)`,
//!     `ModelAssociation(Input)`, `ModelAssociationWhen(Input)`, the six
//!     association payloads (`ChannelModelAssociation(Input)`,
//!     `ChannelRegexAssociation(Input)`, `RegexAssociation(Input)`,
//!     `ModelIDAssociation(Input)`, `ChannelTagsModelAssociation(Input)`,
//!     `ChannelTagsRegexAssociation(Input)`), `ExcludeAssociation(Input)`.
//!   - filter.graphql types (lines 8810-8831): `enum FilterConditionType`,
//!     `type FilterCondition`, `input FilterConditionInput` (recursive tree
//!     referenced by `ModelAssociationWhen.condition`; bound in Go to
//!     `objects.Condition`, internal/objects/condition.go:24).
//!
//! Go reference implementations:
//!   - Query.models         — `internal/server/gql/ent.resolvers.go:371`
//!     (remaps `CREATED_AT` ordering to `ent.DefaultModelOrder` = ID before
//!     delegating to ent `Paginate`, lines 372-374).
//!   - Mutation.createModel — `internal/server/gql/model.resolvers.go:27` →
//!     `biz.ModelService.CreateModel` (`biz/model.go:311`): validate
//!     settings regex patterns → duplicate check on `modelID` only
//!     (`model.ModelID(input.ModelID)`; failure is
//!     `xerrors.DuplicateNameError("model", input.ModelID)`) → create with
//!     ent column defaults (type `chat`, status `disabled`).
//!   - Mutation.updateModel — `model.resolvers.go:37` →
//!     `biz.ModelService.UpdateModel` (`biz/model.go:408`): validate settings
//!     → partial merge via `SetNillable{Developer,ModelID,Type,Name,Group,
//!     Status,Icon}` + conditional Set for modelCard/settings/remark +
//!     `ClearRemark` (runs after `SetRemark`, so clear wins). NOTE: unlike
//!     `ChannelService.UpdateChannel` there is NO duplicate check.
//!   - Mutation.deleteModel — `model.resolvers.go:42` →
//!     `biz.ModelService.DeleteModel` (`biz/model.go:462`); resolver returns
//!     `false, err` on failure and `true` on success.
//!
//! ## Pending (declared by the snapshot but NOT implemented in this slice)
//!
//! These belong to other task slices. They are deliberately absent here (not
//! silently dropped — tracked for follow-up):
//!
//!   - Extended queries (snapshot lines 9104-9111): `fetchModels`,
//!     `queryModels`, `queryModelChannelConnections`,
//!     `queryUnassociatedChannels` — plus their payload types
//!     (`FetchModelsInput`/`FetchModelsPayload`/`ModelIdentify`/
//!     `ModelIdentityWithStatus`/`QueryModelsInput`, snapshot lines 493-518,
//!     and `ModelChannelConnection`/`ChannelModelEntry`/
//!     `UnassociatedChannel`, lines 9083-9098).
//!   - Model mutations outside this CRUD batch (snapshot lines 9115-9122):
//!     `bulkCreateModels`, `updateModelStatus`, `bulkArchiveModels`,
//!     `bulkDisableModels`, `bulkEnableModels`, `bulkDeleteModels`.
//!   - There is NO `model(id: ID!)` single-object query in the snapshot;
//!     single-object lookup goes through the global `node(id: ID!)` /
//!     `nodes(ids: [ID!]!)` Relay queries (a separate slice). `type Model
//!     implements Node` is declared via the shared `Node` interface enum in
//!     `crate::channel`.

use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, InputObject, SimpleObject};

use crate::channel::OrderDirection;
use crate::pagination::PageInfo;
use crate::scalars::{AnyScalar, CursorScalar, TimeScalar};

// ---------------------------------------------------------------------------
// Enums (snapshot-exact value spellings; lowercase/snake_case values are
// pinned explicitly because the default SCREAMING_SNAKE renaming would mangle
// them)
// ---------------------------------------------------------------------------

/// `enum ModelStatus { enabled disabled archived }` — snapshot line 3675,
/// bound to Go `ent/model.Status` (default `disabled`, schema/model.go:50).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ModelStatus {
    #[graphql(name = "enabled")]
    Enabled,
    #[graphql(name = "disabled")]
    Disabled,
    #[graphql(name = "archived")]
    Archived,
}

/// `enum ModelType { chat embedding rerank image_generation video_generation }`
/// — snapshot line 3683, bound to Go `ent/model.Type` (default `chat`,
/// schema/model.go:41).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ModelType {
    #[graphql(name = "chat")]
    Chat,
    #[graphql(name = "embedding")]
    Embedding,
    #[graphql(name = "rerank")]
    Rerank,
    #[graphql(name = "image_generation")]
    ImageGeneration,
    #[graphql(name = "video_generation")]
    VideoGeneration,
}

/// `enum ModelOrderField { CREATED_AT UPDATED_AT NAME }` — snapshot lines
/// 3667-3671.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum ModelOrderField {
    #[graphql(name = "CREATED_AT")]
    CreatedAt,
    #[graphql(name = "UPDATED_AT")]
    UpdatedAt,
    #[graphql(name = "NAME")]
    Name,
}

/// `enum FilterConditionType { condition group }` — snapshot line 8810
/// (filter.graphql), bound to Go `objects.ConditionType` (condition.go:17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum FilterConditionType {
    #[graphql(name = "condition")]
    Condition,
    #[graphql(name = "group")]
    Group,
}

// ---------------------------------------------------------------------------
// Output object types (filter.graphql + model.graphql sections)
// ---------------------------------------------------------------------------

/// `type FilterCondition` — snapshot lines 8815-8822. Recursive condition
/// tree; Go binds it to `objects.Condition` whose `Logic`/`Field`/`Operator`
/// are plain (non-pointer) strings — see the `From` impl for the resulting
/// zero-fill semantics.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct FilterCondition {
    #[graphql(name = "type")]
    pub condition_type: FilterConditionType,
    pub logic: Option<String>,
    pub conditions: Option<Vec<FilterCondition>>,
    pub field: Option<String>,
    pub operator: Option<String>,
    pub value: Option<AnyScalar>,
}

/// `type ModelCardReasoning { supported: Boolean! default: Boolean! }` —
/// snapshot line 8906.
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
pub struct ModelCardReasoning {
    pub supported: bool,
    // `default` is a Rust keyword; the GraphQL field name is pinned.
    #[graphql(name = "default")]
    pub default_: bool,
}

/// `type ModelCardModalities { input: [String!]! output: [String!]! }` —
/// snapshot line 8911.
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
pub struct ModelCardModalities {
    pub input: Vec<String>,
    pub output: Vec<String>,
}

/// `type ModelCardCost` — snapshot lines 8916-8921 (all fields non-null
/// Float; Go `objects.ModelCardCost` uses plain float64).
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
pub struct ModelCardCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// `type ModelCardLimit { context: Int! output: Int! }` — snapshot line 8923.
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
pub struct ModelCardLimit {
    pub context: i64,
    pub output: i64,
}

/// `type ModelCard` — snapshot lines 8893-8904. Bound in Go to
/// `objects.ModelCard` (objects/model.go:26) whose fields are ALL value types
/// (no pointers): the nullable contract fields `vision`/`knowledge`/
/// `releaseDate`/`lastUpdated` therefore always carry a value on the wire
/// (false/"" when unset), never null — the `From` impl mirrors this.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ModelCard {
    pub reasoning: ModelCardReasoning,
    pub tool_call: bool,
    pub temperature: bool,
    pub modalities: ModelCardModalities,
    pub vision: Option<bool>,
    pub cost: ModelCardCost,
    pub limit: ModelCardLimit,
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
}

/// `type ModelAssociationWhen { enabled: Boolean! condition: FilterCondition }`
/// — snapshot line 8947. Go `objects.ModelAssociationWhen` (model.go:74):
/// `Enabled bool` + `Condition *Condition` (pointer → nullable passthrough).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ModelAssociationWhen {
    pub enabled: bool,
    pub condition: Option<FilterCondition>,
}

/// `type ChannelModelAssociation { channelId: Int! modelId: String! }` —
/// snapshot line 8952. Field names are plain camelCase (`channelId`, NOT the
/// all-caps `channelID` spelling used elsewhere) — default renaming is right.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelModelAssociation {
    pub channel_id: i64,
    pub model_id: String,
}

/// `type ChannelRegexAssociation { channelId: Int! pattern: String! }` —
/// snapshot line 8957.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelRegexAssociation {
    pub channel_id: i64,
    pub pattern: String,
}

/// `type ExcludeAssociation` — snapshot lines 8972-8976. Go
/// `objects.ExcludeAssociation` (model.go:79): `ChannelNamePattern` is a
/// plain string (zero-fills to "", always emitted); the two slices are
/// nil-able (null passthrough).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ExcludeAssociation {
    pub channel_name_pattern: Option<String>,
    pub channel_ids: Option<Vec<i64>>,
    pub channel_tags: Option<Vec<String>>,
}

/// `type RegexAssociation { pattern: String! exclude: [ExcludeAssociation!] }`
/// — snapshot line 8962.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct RegexAssociation {
    pub pattern: String,
    pub exclude: Option<Vec<ExcludeAssociation>>,
}

/// `type ModelIDAssociation` — snapshot lines 8967-8970. The GraphQL type
/// name carries the all-caps `ID` acronym, pinned explicitly.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ModelIDAssociation")]
pub struct ModelIdAssociation {
    pub model_id: String,
    pub exclude: Option<Vec<ExcludeAssociation>>,
}

/// `type ChannelTagsModelAssociation { channelTags: [String!]! modelId:
/// String! }` — snapshot line 8978.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelTagsModelAssociation {
    pub channel_tags: Vec<String>,
    pub model_id: String,
}

/// `type ChannelTagsRegexAssociation { channelTags: [String!]! pattern:
/// String! }` — snapshot line 8983.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelTagsRegexAssociation {
    pub channel_tags: Vec<String>,
    pub pattern: String,
}

/// `type ModelAssociation` — snapshot lines 8934-8945. Go
/// `objects.ModelAssociation` (model.go:55): `Type string` / `Priority int` /
/// `Disabled bool` are values; the seven payload fields are pointers
/// (nullable passthrough). Note `modelId` is plain camelCase here.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ModelAssociation {
    // `type` is a Rust keyword; GraphQL name pinned explicitly.
    #[graphql(name = "type")]
    pub association_type: String,
    pub priority: i64,
    pub disabled: bool,
    pub when: Option<ModelAssociationWhen>,
    pub channel_model: Option<ChannelModelAssociation>,
    pub channel_regex: Option<ChannelRegexAssociation>,
    pub regex: Option<RegexAssociation>,
    pub model_id: Option<ModelIdAssociation>,
    pub channel_tags_model: Option<ChannelTagsModelAssociation>,
    pub channel_tags_regex: Option<ChannelTagsRegexAssociation>,
}

/// `type ModelSettings { disableDeveloperSettingsInheritance: Boolean!
/// associations: [ModelAssociation!]! }` — snapshot lines 8929-8932.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ModelSettings {
    pub disable_developer_settings_inheritance: bool,
    pub associations: Vec<ModelAssociation>,
}

/// `type Model implements Node` — snapshot lines 3585-3620. Complete: the
/// ent Model schema declares no edges, so the base block carries only scalar
/// and self-domain fields. `associatedChannelCount` is included because the
/// retained frontend requests it from create/update/list payloads.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
pub struct Model {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    pub developer: String,
    // All-caps acronym tag: default camelCase would emit `modelId`.
    #[graphql(name = "modelID")]
    pub model_id: String,
    #[graphql(name = "type")]
    pub model_type: ModelType,
    pub name: String,
    pub icon: String,
    pub group: String,
    pub model_card: ModelCard,
    pub settings: ModelSettings,
    pub status: ModelStatus,
    pub remark: Option<String>,
    #[graphql(skip)]
    pub associated_channel_count: i64,
}

#[ComplexObject]
impl Model {
    /// The Go schema exposes this as a force-resolver because the value is
    /// derived from live channels plus inherited model associations, not from
    /// a model-table column. Tests and lightweight schemas without the host
    /// service retain the materialized fallback value.
    async fn associated_channel_count(&self, ctx: &Context<'_>) -> Result<i64, String> {
        match ctx.data::<Arc<dyn ModelAssociationCountServices>>() {
            Ok(services) => services
                .associated_channel_count(self)
                .await
                .map_err(|error| error.to_string()),
            Err(_) => Ok(self.associated_channel_count),
        }
    }
}

/// `type ModelEdge { node: Model cursor: Cursor! }` — snapshot line 3641.
/// `node` is nullable in the contract (ent emits nullable edge nodes).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ModelEdge {
    pub node: Option<Model>,
    pub cursor: CursorScalar,
}

/// `type ModelConnection` — snapshot line 3624. `edges` is a nullable list
/// of nullable edges (`[ModelEdge]`), exactly as ent generates it.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
pub struct ModelConnection {
    pub edges: Option<Vec<Option<ModelEdge>>>,
    pub page_info: PageInfo,
    pub total_count: i64,
}

// ---------------------------------------------------------------------------
// Input object types
// ---------------------------------------------------------------------------

/// `input FilterConditionInput` — snapshot lines 8824-8831 (recursive).
#[derive(Debug, Clone, PartialEq, InputObject)]
pub struct FilterConditionInput {
    #[graphql(name = "type")]
    pub condition_type: FilterConditionType,
    pub logic: Option<String>,
    pub conditions: Option<Vec<FilterConditionInput>>,
    pub field: Option<String>,
    pub operator: Option<String>,
    pub value: Option<AnyScalar>,
}

/// `input ModelCardReasoningInput { supported: Boolean default: Boolean }` —
/// snapshot line 9002 (both nullable on input, non-null on output).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct ModelCardReasoningInput {
    pub supported: Option<bool>,
    #[graphql(name = "default")]
    pub default_: Option<bool>,
}

/// `input ModelCardModalitiesInput { input: [String!] output: [String!] }` —
/// snapshot line 9007 (nullable lists on input, non-null on output).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct ModelCardModalitiesInput {
    pub input: Option<Vec<String>>,
    pub output: Option<Vec<String>>,
}

/// `input ModelCardCostInput` — snapshot lines 9012-9017 (all nullable).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct ModelCardCostInput {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

/// `input ModelCardLimitInput { context: Int output: Int }` — snapshot 9019.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct ModelCardLimitInput {
    pub context: Option<i64>,
    pub output: Option<i64>,
}

/// `input ModelCardInput` — snapshot lines 8989-9000 (all nullable).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct ModelCardInput {
    pub reasoning: Option<ModelCardReasoningInput>,
    pub tool_call: Option<bool>,
    pub temperature: Option<bool>,
    pub modalities: Option<ModelCardModalitiesInput>,
    pub vision: Option<bool>,
    pub cost: Option<ModelCardCostInput>,
    pub limit: Option<ModelCardLimitInput>,
    pub knowledge: Option<String>,
    pub release_date: Option<String>,
    pub last_updated: Option<String>,
}

/// `input ModelAssociationWhenInput { enabled: Boolean condition:
/// FilterConditionInput }` — snapshot line 9042.
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct ModelAssociationWhenInput {
    pub enabled: Option<bool>,
    pub condition: Option<FilterConditionInput>,
}

/// `input ChannelModelAssociationInput { channelId: Int! modelId: String! }`
/// — snapshot line 9047.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ChannelModelAssociationInput {
    pub channel_id: i64,
    pub model_id: String,
}

/// `input ChannelRegexAssociationInput { channelId: Int! pattern: String! }`
/// — snapshot line 9052.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ChannelRegexAssociationInput {
    pub channel_id: i64,
    pub pattern: String,
}

/// `input ExcludeAssociationInput` — snapshot lines 9067-9071.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct ExcludeAssociationInput {
    pub channel_name_pattern: Option<String>,
    pub channel_ids: Option<Vec<i64>>,
    pub channel_tags: Option<Vec<String>>,
}

/// `input RegexAssociationInput { pattern: String! exclude:
/// [ExcludeAssociationInput!] }` — snapshot line 9057.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct RegexAssociationInput {
    pub pattern: String,
    pub exclude: Option<Vec<ExcludeAssociationInput>>,
}

/// `input ModelIDAssociationInput` — snapshot lines 9062-9065 (all-caps `ID`
/// acronym in the type name, pinned explicitly).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "ModelIDAssociationInput")]
pub struct ModelIdAssociationInput {
    pub model_id: String,
    pub exclude: Option<Vec<ExcludeAssociationInput>>,
}

/// `input ChannelTagsModelAssociationInput` — snapshot line 9073.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ChannelTagsModelAssociationInput {
    pub channel_tags: Vec<String>,
    pub model_id: String,
}

/// `input ChannelTagsRegexAssociationInput` — snapshot line 9078.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ChannelTagsRegexAssociationInput {
    pub channel_tags: Vec<String>,
    pub pattern: String,
}

/// `input ModelAssociationInput` — snapshot lines 9029-9040 (`type` is
/// non-null; `priority`/`disabled` are nullable on input and zero-fill).
#[derive(Debug, Clone, PartialEq, InputObject)]
pub struct ModelAssociationInput {
    #[graphql(name = "type")]
    pub association_type: String,
    pub priority: Option<i64>,
    pub disabled: Option<bool>,
    pub when: Option<ModelAssociationWhenInput>,
    pub channel_model: Option<ChannelModelAssociationInput>,
    pub channel_regex: Option<ChannelRegexAssociationInput>,
    pub regex: Option<RegexAssociationInput>,
    pub model_id: Option<ModelIdAssociationInput>,
    pub channel_tags_model: Option<ChannelTagsModelAssociationInput>,
    pub channel_tags_regex: Option<ChannelTagsRegexAssociationInput>,
}

/// `input ModelSettingsInput { disableDeveloperSettingsInheritance: Boolean
/// associations: [ModelAssociationInput!]! }` — snapshot line 9024. The
/// associations list is NON-NULL on input (unlike most support inputs).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct ModelSettingsInput {
    pub disable_developer_settings_inheritance: Option<bool>,
    pub associations: Vec<ModelAssociationInput>,
}

/// `input CreateModelInput` — snapshot lines 2977-3008 (ent-generated).
/// `status` is intentionally absent (ent `SkipMutationCreateInput`); the
/// created row takes the column default `disabled`.
#[derive(Debug, Clone, PartialEq, InputObject)]
pub struct CreateModelInput {
    pub developer: String,
    #[graphql(name = "modelID")]
    pub model_id: String,
    /// Nullable on input; ent column default is `chat` (schema/model.go:41).
    #[graphql(name = "type")]
    pub model_type: Option<ModelType>,
    pub name: String,
    pub icon: String,
    pub group: String,
    pub model_card: ModelCardInput,
    pub settings: ModelSettingsInput,
    pub remark: Option<String>,
}

/// `input UpdateModelInput` — snapshot lines 7426-7459 (ent-generated).
///
/// Go parity (biz/model.go:408-447): every field here IS applied by
/// `ModelService.UpdateModel` — `SetNillable*` for the scalars/enums,
/// conditional `Set` for modelCard/settings/remark, and `ClearRemark` runs
/// AFTER `SetRemark` (clear wins when both are provided).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct UpdateModelInput {
    pub developer: Option<String>,
    #[graphql(name = "modelID")]
    pub model_id: Option<String>,
    #[graphql(name = "type")]
    pub model_type: Option<ModelType>,
    pub name: Option<String>,
    pub icon: Option<String>,
    pub group: Option<String>,
    pub model_card: Option<ModelCardInput>,
    pub settings: Option<ModelSettingsInput>,
    pub status: Option<ModelStatus>,
    pub remark: Option<String>,
    pub clear_remark: Option<bool>,
}

/// `input ModelOrder { direction: OrderDirection! = ASC field:
/// ModelOrderField! }` — snapshot lines 3654-3663.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ModelOrder {
    /// Defaults to ASC when omitted, matching the ent-generated contract.
    #[graphql(default_with = "OrderDirection::Asc")]
    pub direction: OrderDirection,
    pub field: ModelOrderField,
}

/// `input ModelWhereInput` — snapshot lines 3694-3843 (ent-generated
/// predicate grammar). Complete: `not`/`and`/`or` plus every scalar-field
/// predicate family. The ent Model schema declares no edges, so there are no
/// `has<Edge>`/`has<Edge>With` predicates to defer.
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct ModelWhereInput {
    pub not: Option<Box<ModelWhereInput>>,
    pub and: Option<Vec<ModelWhereInput>>,
    pub or: Option<Vec<ModelWhereInput>>,
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
    // developer field predicates
    pub developer: Option<String>,
    #[graphql(name = "developerNEQ")]
    pub developer_neq: Option<String>,
    pub developer_in: Option<Vec<String>>,
    pub developer_not_in: Option<Vec<String>>,
    #[graphql(name = "developerGT")]
    pub developer_gt: Option<String>,
    #[graphql(name = "developerGTE")]
    pub developer_gte: Option<String>,
    #[graphql(name = "developerLT")]
    pub developer_lt: Option<String>,
    #[graphql(name = "developerLTE")]
    pub developer_lte: Option<String>,
    pub developer_contains: Option<String>,
    pub developer_has_prefix: Option<String>,
    pub developer_has_suffix: Option<String>,
    pub developer_equal_fold: Option<String>,
    pub developer_contains_fold: Option<String>,
    // model_id field predicates (all-caps acronym: every name pinned)
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
    // type field predicates
    #[graphql(name = "type")]
    pub model_type: Option<ModelType>,
    #[graphql(name = "typeNEQ")]
    pub type_neq: Option<ModelType>,
    pub type_in: Option<Vec<ModelType>>,
    pub type_not_in: Option<Vec<ModelType>>,
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
    // icon field predicates
    pub icon: Option<String>,
    #[graphql(name = "iconNEQ")]
    pub icon_neq: Option<String>,
    pub icon_in: Option<Vec<String>>,
    pub icon_not_in: Option<Vec<String>>,
    #[graphql(name = "iconGT")]
    pub icon_gt: Option<String>,
    #[graphql(name = "iconGTE")]
    pub icon_gte: Option<String>,
    #[graphql(name = "iconLT")]
    pub icon_lt: Option<String>,
    #[graphql(name = "iconLTE")]
    pub icon_lte: Option<String>,
    pub icon_contains: Option<String>,
    pub icon_has_prefix: Option<String>,
    pub icon_has_suffix: Option<String>,
    pub icon_equal_fold: Option<String>,
    pub icon_contains_fold: Option<String>,
    // group field predicates
    pub group: Option<String>,
    #[graphql(name = "groupNEQ")]
    pub group_neq: Option<String>,
    pub group_in: Option<Vec<String>>,
    pub group_not_in: Option<Vec<String>>,
    #[graphql(name = "groupGT")]
    pub group_gt: Option<String>,
    #[graphql(name = "groupGTE")]
    pub group_gte: Option<String>,
    #[graphql(name = "groupLT")]
    pub group_lt: Option<String>,
    #[graphql(name = "groupLTE")]
    pub group_lte: Option<String>,
    pub group_contains: Option<String>,
    pub group_has_prefix: Option<String>,
    pub group_has_suffix: Option<String>,
    pub group_equal_fold: Option<String>,
    pub group_contains_fold: Option<String>,
    // status field predicates
    pub status: Option<ModelStatus>,
    #[graphql(name = "statusNEQ")]
    pub status_neq: Option<ModelStatus>,
    pub status_in: Option<Vec<ModelStatus>>,
    pub status_not_in: Option<Vec<ModelStatus>>,
    // remark field predicates
    pub remark: Option<String>,
    #[graphql(name = "remarkNEQ")]
    pub remark_neq: Option<String>,
    pub remark_in: Option<Vec<String>>,
    pub remark_not_in: Option<Vec<String>>,
    #[graphql(name = "remarkGT")]
    pub remark_gt: Option<String>,
    #[graphql(name = "remarkGTE")]
    pub remark_gte: Option<String>,
    #[graphql(name = "remarkLT")]
    pub remark_lt: Option<String>,
    #[graphql(name = "remarkLTE")]
    pub remark_lte: Option<String>,
    pub remark_contains: Option<String>,
    pub remark_has_prefix: Option<String>,
    pub remark_has_suffix: Option<String>,
    pub remark_is_nil: Option<bool>,
    pub remark_not_nil: Option<bool>,
    pub remark_equal_fold: Option<String>,
    pub remark_contains_fold: Option<String>,
}

// ---------------------------------------------------------------------------
// Input → object conversions.
//
// Go binds the input and output GraphQL types to the SAME `objects.*` Go
// struct via `@goModel` (e.g. `ModelCardInput` and `ModelCard` both map to
// `objects.ModelCard`), so an input value round-trips into the stored object
// unchanged. These `From` impls are the Rust analogue. Zero-fill rules mirror
// gqlgen decoding into the Go struct exactly:
//   - Go VALUE fields (bool/string/int/float/nested struct): a null input
//     zero-fills, and the zero value is always emitted on output (never null)
//     — hence `Some(x.unwrap_or_default())` for nullable contract fields.
//   - Go POINTER/slice fields: null passes through as nil → null on output.
// ---------------------------------------------------------------------------

impl From<FilterConditionInput> for FilterCondition {
    fn from(input: FilterConditionInput) -> Self {
        Self {
            condition_type: input.condition_type,
            // objects.Condition.Logic/Field/Operator are plain strings.
            logic: Some(input.logic.unwrap_or_default()),
            conditions: input
                .conditions
                .map(|v| v.into_iter().map(Into::into).collect()),
            field: Some(input.field.unwrap_or_default()),
            operator: Some(input.operator.unwrap_or_default()),
            // objects.Condition.Value is `any` (nil-able) — passthrough.
            value: input.value,
        }
    }
}

impl From<ModelCardReasoningInput> for ModelCardReasoning {
    fn from(input: ModelCardReasoningInput) -> Self {
        Self {
            supported: input.supported.unwrap_or(false),
            default_: input.default_.unwrap_or(false),
        }
    }
}

impl From<ModelCardModalitiesInput> for ModelCardModalities {
    fn from(value: ModelCardModalitiesInput) -> Self {
        // Output lists are non-null; Go marshals a nil slice as [].
        Self {
            input: value.input.unwrap_or_default(),
            output: value.output.unwrap_or_default(),
        }
    }
}

impl From<ModelCardCostInput> for ModelCardCost {
    fn from(value: ModelCardCostInput) -> Self {
        Self {
            input: value.input.unwrap_or(0.0),
            output: value.output.unwrap_or(0.0),
            cache_read: value.cache_read.unwrap_or(0.0),
            cache_write: value.cache_write.unwrap_or(0.0),
        }
    }
}

impl From<ModelCardLimitInput> for ModelCardLimit {
    fn from(input: ModelCardLimitInput) -> Self {
        Self {
            context: input.context.unwrap_or(0),
            output: input.output.unwrap_or(0),
        }
    }
}

impl From<ModelCardInput> for ModelCard {
    fn from(input: ModelCardInput) -> Self {
        // Every objects.ModelCard field is a VALUE type (objects/model.go:26)
        // — omitted nested cards become their Go zero value, and the nullable
        // contract fields (vision/knowledge/releaseDate/lastUpdated) always
        // carry a zero-filled value on output.
        Self {
            reasoning: input.reasoning.map(Into::into).unwrap_or_default(),
            tool_call: input.tool_call.unwrap_or(false),
            temperature: input.temperature.unwrap_or(false),
            modalities: input.modalities.map(Into::into).unwrap_or_default(),
            vision: Some(input.vision.unwrap_or(false)),
            cost: input.cost.map(Into::into).unwrap_or_default(),
            limit: input.limit.map(Into::into).unwrap_or_default(),
            knowledge: Some(input.knowledge.unwrap_or_default()),
            release_date: Some(input.release_date.unwrap_or_default()),
            last_updated: Some(input.last_updated.unwrap_or_default()),
        }
    }
}

impl From<ModelAssociationWhenInput> for ModelAssociationWhen {
    fn from(input: ModelAssociationWhenInput) -> Self {
        Self {
            enabled: input.enabled.unwrap_or(false),
            condition: input.condition.map(Into::into),
        }
    }
}

impl From<ChannelModelAssociationInput> for ChannelModelAssociation {
    fn from(input: ChannelModelAssociationInput) -> Self {
        Self {
            channel_id: input.channel_id,
            model_id: input.model_id,
        }
    }
}

impl From<ChannelRegexAssociationInput> for ChannelRegexAssociation {
    fn from(input: ChannelRegexAssociationInput) -> Self {
        Self {
            channel_id: input.channel_id,
            pattern: input.pattern,
        }
    }
}

impl From<ExcludeAssociationInput> for ExcludeAssociation {
    fn from(input: ExcludeAssociationInput) -> Self {
        Self {
            // objects.ExcludeAssociation.ChannelNamePattern is a plain string.
            channel_name_pattern: Some(input.channel_name_pattern.unwrap_or_default()),
            channel_ids: input.channel_ids,
            channel_tags: input.channel_tags,
        }
    }
}

impl From<RegexAssociationInput> for RegexAssociation {
    fn from(input: RegexAssociationInput) -> Self {
        Self {
            pattern: input.pattern,
            exclude: input
                .exclude
                .map(|v| v.into_iter().map(Into::into).collect()),
        }
    }
}

impl From<ModelIdAssociationInput> for ModelIdAssociation {
    fn from(input: ModelIdAssociationInput) -> Self {
        Self {
            model_id: input.model_id,
            exclude: input
                .exclude
                .map(|v| v.into_iter().map(Into::into).collect()),
        }
    }
}

impl From<ChannelTagsModelAssociationInput> for ChannelTagsModelAssociation {
    fn from(input: ChannelTagsModelAssociationInput) -> Self {
        Self {
            channel_tags: input.channel_tags,
            model_id: input.model_id,
        }
    }
}

impl From<ChannelTagsRegexAssociationInput> for ChannelTagsRegexAssociation {
    fn from(input: ChannelTagsRegexAssociationInput) -> Self {
        Self {
            channel_tags: input.channel_tags,
            pattern: input.pattern,
        }
    }
}

impl From<ModelAssociationInput> for ModelAssociation {
    fn from(input: ModelAssociationInput) -> Self {
        Self {
            association_type: input.association_type,
            // objects.ModelAssociation.Priority/Disabled are values.
            priority: input.priority.unwrap_or(0),
            disabled: input.disabled.unwrap_or(false),
            // The seven payload fields are Go pointers — passthrough.
            when: input.when.map(Into::into),
            channel_model: input.channel_model.map(Into::into),
            channel_regex: input.channel_regex.map(Into::into),
            regex: input.regex.map(Into::into),
            model_id: input.model_id.map(Into::into),
            channel_tags_model: input.channel_tags_model.map(Into::into),
            channel_tags_regex: input.channel_tags_regex.map(Into::into),
        }
    }
}

impl From<ModelSettingsInput> for ModelSettings {
    fn from(input: ModelSettingsInput) -> Self {
        Self {
            disable_developer_settings_inheritance: input
                .disable_developer_settings_inheritance
                .unwrap_or(false),
            associations: input.associations.into_iter().map(Into::into).collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Ordering resolution (Go ent.resolvers.go:371-379)
// ---------------------------------------------------------------------------

/// Internal ordering terms the service layer receives. `Id` is NOT part of
/// the GraphQL `ModelOrderField` enum — it is ent's `DefaultModelOrder`
/// (order by primary key, gql_pagination.go:2950), which the Go resolver
/// substitutes when the client asks for `CREATED_AT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelOrderTerm {
    /// ent `DefaultModelOrder` — ascending/descending by row ID.
    Id,
    UpdatedAt,
    Name,
}

/// The resolver-lowered ordering selection handed to
/// [`ModelQueryServices::models`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelOrderSelection {
    pub direction: OrderDirection,
    pub term: ModelOrderTerm,
}

/// Lower the GraphQL `orderBy` argument into a service-level selection,
/// mirroring Go `Query.models` (ent.resolvers.go:372-374): a `CREATED_AT`
/// request is remapped to `ent.DefaultModelOrder` (order by ID) with the
/// requested direction preserved; every other field maps one-to-one.
pub fn resolve_model_order(order_by: Option<ModelOrder>) -> Option<ModelOrderSelection> {
    order_by.map(|order| ModelOrderSelection {
        direction: order.direction,
        term: match order.field {
            ModelOrderField::CreatedAt => ModelOrderTerm::Id,
            ModelOrderField::UpdatedAt => ModelOrderTerm::UpdatedAt,
            ModelOrderField::Name => ModelOrderTerm::Name,
        },
    })
}

// ---------------------------------------------------------------------------
// Service traits (host-injected, mirroring the Go resolver's dependencies:
// `r.client.Model` for the connection query and `r.modelService` for the
// CRUD mutations)
// ---------------------------------------------------------------------------

/// Error surface for the model services. Messages mirror the Go error
/// strings so frontend error handling stays stable:
///   - duplicate — `xerrors.DuplicateNameError("model", input.ModelID)`
///     (`internal/pkg/xerrors/graphql.go:104`: "%s name '%s' already exists";
///     note Go passes the MODEL ID as the name argument, biz/model.go:328).
///   - not found — ent's `ent: model not found`.
///   - wrapped create/update/delete failures — the `fmt.Errorf("failed to
///     ...: %w")` prefixes in `biz/model.go`.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ModelServiceError {
    #[error("model service is not available")]
    ServiceUnavailable,
    #[error("model name '{0}' already exists")]
    DuplicateName(String),
    #[error("ent: model not found")]
    NotFound,
    #[error("failed to create model: {0}")]
    Create(String),
    #[error("failed to update model: {0}")]
    Update(String),
    #[error("failed to delete model: {0}")]
    Delete(String),
    #[error("failed to query models: {0}")]
    Query(String),
}

/// Arguments for the `models` connection query, passed through from the
/// GraphQL layer verbatim (Go hands them straight to ent's `Paginate`).
/// Cursor values are the raw `Cursor` scalar strings; `where_filter` is the
/// full predicate input — lowering it into storage predicates is the host's
/// job (ent does this in Go via `WithModelFilter`).
#[derive(Debug, Clone, Default)]
pub struct ModelConnectionArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<ModelOrderSelection>,
    pub where_filter: Option<ModelWhereInput>,
}

/// Backs `Query.models` (Go ent.resolvers.go:371: `r.client.Model.Query()
/// .Paginate(...)`). The host wires the real repository; tests use an
/// in-memory store.
#[async_trait::async_trait]
pub trait ModelQueryServices: Send + Sync {
    async fn models(&self, args: ModelConnectionArgs)
    -> Result<ModelConnection, ModelServiceError>;
}

/// Backs the three CRUD mutations (Go `biz.ModelService`; a partial Rust port
/// of the settings-validation half lives in
/// `conduit-services/src/model_service.rs`). `id` is the raw GraphQL `ID!`
/// scalar string; Go decodes it into `objects.GUID` and passes `.ID` (int) to
/// the service — concrete hosts perform the same decode.
#[async_trait::async_trait]
pub trait ModelMutationServices: Send + Sync {
    /// Mirrors `ModelService.CreateModel` (biz/model.go:311): validate the
    /// settings regex patterns, reject an existing `modelID` (the duplicate
    /// probe queries `model.ModelID` only), then create with ent column
    /// defaults (type `chat`, status `disabled`).
    async fn create_model(&self, input: CreateModelInput) -> Result<Model, ModelServiceError>;

    /// Mirrors `ModelService.UpdateModel` (biz/model.go:408): validate the
    /// settings, then a partial merge of the provided fields. There is NO
    /// duplicate check on update (Go parity — unlike UpdateChannel).
    async fn update_model(
        &self,
        id: &str,
        input: UpdateModelInput,
    ) -> Result<Model, ModelServiceError>;

    /// Mirrors `ModelService.DeleteModel` (biz/model.go:462): delete by id;
    /// failures are wrapped as "failed to delete model: ...".
    async fn delete_model(&self, id: &str) -> Result<(), ModelServiceError>;
}

/// Backs `Model.associatedChannelCount`, mirroring Go's force-resolver
/// `ModelService.CountModelAssociatedChannels`. Keeping it separate from CRUD
/// makes the derived field work for every producer of a `Model` object,
/// including Node lookups and bulk mutations.
#[async_trait::async_trait]
pub trait ModelAssociationCountServices: Send + Sync {
    async fn associated_channel_count(&self, model: &Model) -> Result<i64, ModelServiceError>;
}

/// Resolves the injected [`ModelQueryServices`] from the async-graphql data
/// bag; absent wiring surfaces the "service is not available" failure mode
/// (same convention as `channel::channel_query_services`).
pub(crate) fn model_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ModelQueryServices>, String> {
    match ctx.data::<Arc<dyn ModelQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ModelServiceError::ServiceUnavailable.to_string()),
    }
}

/// Resolves the injected [`ModelMutationServices`] from the data bag.
pub(crate) fn model_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ModelMutationServices>, String> {
    match ctx.data::<Arc<dyn ModelMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ModelServiceError::ServiceUnavailable.to_string()),
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
    use crate::sdl_parity::{
        assert_block_parity, assert_block_parity_with_extensions, block_fields, snapshot_text,
    };
    use crate::{AdminSchema, admin_schema_builder};

    type TestError = Box<dyn std::error::Error>;

    // ---------------------------------------------------------------------
    // In-memory service double. Mirrors the Go `biz.ModelService` call
    // sequences without DB/HTTP; the connection query mirrors the thin ent
    // delegation (no predicate lowering — the `where` filter is recorded and
    // passed through, as in Go where ent lowers it). Settings regex
    // validation (biz/model.go:51-113) is a service-internal concern ported
    // in conduit-services and is not re-modelled here.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct InMemoryModelService {
        models: Arc<Mutex<Vec<Model>>>,
        captured_query_args: Arc<Mutex<Vec<ModelConnectionArgs>>>,
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Fixed timestamp for stored rows (the wire format of `Time` is not
    /// under test here).
    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    /// A model card/settings pair in the shape gqlgen produces for an empty
    /// input object (Go zero values throughout).
    fn empty_card() -> ModelCard {
        ModelCard::from(ModelCardInput::default())
    }

    fn empty_settings() -> ModelSettings {
        ModelSettings::from(ModelSettingsInput::default())
    }

    fn sample_model(id: i64, model_id: &str) -> Model {
        Model {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            developer: "deepseek".to_owned(),
            model_id: model_id.to_owned(),
            model_type: ModelType::Chat,
            name: format!("Model {model_id}"),
            icon: "DeepSeek".to_owned(),
            group: "deepseek".to_owned(),
            model_card: empty_card(),
            settings: empty_settings(),
            status: ModelStatus::Enabled,
            remark: None,
            associated_channel_count: 0,
        }
    }

    #[async_trait::async_trait]
    impl ModelQueryServices for InMemoryModelService {
        async fn models(
            &self,
            args: ModelConnectionArgs,
        ) -> Result<ModelConnection, ModelServiceError> {
            lock(&self.captured_query_args).push(args.clone());

            let mut nodes: Vec<Model> = lock(&self.models).clone();
            if let Some(selection) = &args.order_by {
                nodes.sort_by(|a, b| {
                    let ordering = match selection.term {
                        ModelOrderTerm::Id => {
                            a.id.as_str()
                                .parse::<i64>()
                                .unwrap_or(i64::MAX)
                                .cmp(&b.id.as_str().parse::<i64>().unwrap_or(i64::MAX))
                        }
                        ModelOrderTerm::UpdatedAt => a.updated_at.0.cmp(&b.updated_at.0),
                        ModelOrderTerm::Name => a.name.cmp(&b.name),
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
            Ok(ModelConnection {
                edges: Some(
                    connection
                        .edges
                        .into_iter()
                        .map(|edge| {
                            Some(ModelEdge {
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
    impl ModelMutationServices for InMemoryModelService {
        async fn create_model(&self, input: CreateModelInput) -> Result<Model, ModelServiceError> {
            let mut guard = lock(&self.models);
            // Go CreateModel (biz/model.go:319-329): the duplicate probe
            // queries `model.ModelID(input.ModelID)` ONLY (the code comment
            // mentions developer+modelId, but the predicate is modelID) and
            // fails with `xerrors.DuplicateNameError("model", input.ModelID)`.
            if guard
                .iter()
                .any(|existing| existing.model_id == input.model_id)
            {
                return Err(ModelServiceError::DuplicateName(input.model_id));
            }

            // ent column defaults (internal/ent/schema/model.go): type
            // defaults to `chat` (L41, create input field is nullable);
            // status defaults to `disabled` (L50; the create input skips
            // status via SkipMutationCreateInput).
            let id = guard.len() as i64 + 1;
            let created = Model {
                id: ID::from(id.to_string()),
                created_at: epoch(),
                updated_at: epoch(),
                developer: input.developer,
                model_id: input.model_id,
                model_type: input.model_type.unwrap_or(ModelType::Chat),
                name: input.name,
                icon: input.icon,
                group: input.group,
                model_card: ModelCard::from(input.model_card),
                settings: ModelSettings::from(input.settings),
                status: ModelStatus::Disabled,
                remark: input.remark,
                associated_channel_count: 0,
            };
            guard.push(created.clone());
            Ok(created)
        }

        async fn update_model(
            &self,
            id: &str,
            input: UpdateModelInput,
        ) -> Result<Model, ModelServiceError> {
            let mut guard = lock(&self.models);
            // Go UpdateModel (biz/model.go:408) performs NO duplicate check —
            // unlike ChannelService.UpdateChannel. Do not add one here.
            let Some(model) = guard.iter_mut().find(|m| m.id.as_str() == id) else {
                // ent UpdateOneID on a missing row fails at Save; biz wraps
                // it as "failed to update model: %w".
                return Err(ModelServiceError::Update(
                    ModelServiceError::NotFound.to_string(),
                ));
            };

            // Field application mirrors Go biz/model.go:416-439 exactly:
            // SetNillable{Developer,ModelID,Type,Name,Group,Status,Icon};
            // conditional Set for modelCard/settings/remark; ClearRemark runs
            // after SetRemark (clear wins when both are provided).
            if let Some(v) = input.developer {
                model.developer = v;
            }
            if let Some(v) = input.model_id {
                model.model_id = v;
            }
            if let Some(v) = input.model_type {
                model.model_type = v;
            }
            if let Some(v) = input.name {
                model.name = v;
            }
            if let Some(v) = input.group {
                model.group = v;
            }
            if let Some(v) = input.status {
                model.status = v;
            }
            if let Some(v) = input.icon {
                model.icon = v;
            }
            if let Some(v) = input.model_card {
                model.model_card = ModelCard::from(v);
            }
            if let Some(v) = input.settings {
                model.settings = ModelSettings::from(v);
            }
            if let Some(v) = input.remark {
                model.remark = Some(v);
            }
            if input.clear_remark.unwrap_or(false) {
                model.remark = None;
            }

            Ok(model.clone())
        }

        async fn delete_model(&self, id: &str) -> Result<(), ModelServiceError> {
            let mut guard = lock(&self.models);
            let before = guard.len();
            guard.retain(|m| m.id.as_str() != id);
            if guard.len() == before {
                // Go: "failed to delete model: %w" wrapping ent not-found.
                return Err(ModelServiceError::Delete(
                    ModelServiceError::NotFound.to_string(),
                ));
            }
            Ok(())
        }
    }

    fn schema_with(store: &InMemoryModelService) -> AdminSchema {
        let query: Arc<dyn ModelQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn ModelMutationServices> = Arc::new(store.clone());
        admin_schema_builder().data(query).data(mutation).finish()
    }

    #[derive(Clone)]
    struct FixedAssociationCount(i64);

    #[async_trait::async_trait]
    impl ModelAssociationCountServices for FixedAssociationCount {
        async fn associated_channel_count(&self, _model: &Model) -> Result<i64, ModelServiceError> {
            Ok(self.0)
        }
    }

    fn schema_with_count(store: &InMemoryModelService, count: i64) -> AdminSchema {
        let query: Arc<dyn ModelQueryServices> = Arc::new(store.clone());
        let mutation: Arc<dyn ModelMutationServices> = Arc::new(store.clone());
        let counter: Arc<dyn ModelAssociationCountServices> =
            Arc::new(FixedAssociationCount(count));
        admin_schema_builder()
            .data(query)
            .data(mutation)
            .data(counter)
            .finish()
    }

    fn bare_schema() -> AdminSchema {
        crate::build_admin_schema()
    }

    // ---------------------------------------------------------------------
    // SDL parity: object types (helpers live in crate::sdl_parity — the
    // snapshot at tests/contracts/admin_graphql_schema.graphql is the oracle;
    // set equality catches missing AND invented fields in one shot)
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_model_type_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;

        // async-graphql flattens the snapshot's `extend type Model` field into
        // the generated base block. Keep that implemented extension explicit
        // so the base snapshot comparison still rejects unrelated drift.
        assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "type Model",
            "type Model",
            &[],
            &["associatedChannelCount: Int!"],
        )?;

        // The implements clause must match the snapshot's declaration.
        assert!(
            sdl.contains("type Model implements Node {"),
            "generated SDL must declare `type Model implements Node`"
        );
        assert!(snapshot.contains("type Model implements Node {"));

        // Guard the extension's provenance in the captured contract.
        assert!(snapshot.contains("associatedChannelCount: Int!"));
        Ok(())
    }

    #[test]
    fn sdl_model_connection_and_edge_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type ModelConnection",
            "type ModelConnection",
            &[],
        )?;
        assert_block_parity(&sdl, &snapshot, "type ModelEdge", "type ModelEdge", &[])?;
        Ok(())
    }

    #[test]
    fn sdl_model_card_types_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "type ModelCard",
            "type ModelCardReasoning",
            "type ModelCardModalities",
            "type ModelCardCost",
            "type ModelCardLimit",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_model_settings_and_association_types_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "type ModelSettings",
            "type ModelAssociation",
            "type ModelAssociationWhen",
            "type ChannelModelAssociation",
            "type ChannelRegexAssociation",
            "type RegexAssociation",
            "type ModelIDAssociation",
            "type ExcludeAssociation",
            "type ChannelTagsModelAssociation",
            "type ChannelTagsRegexAssociation",
            "type FilterCondition",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: input types
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_create_and_update_model_inputs_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input CreateModelInput",
            "input CreateModelInput",
            &[],
        )?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input UpdateModelInput",
            "input UpdateModelInput",
            &[],
        )?;
        Ok(())
    }

    #[test]
    fn sdl_model_where_input_matches_snapshot_completely() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        // No pending entries: the ent Model schema has no edges, so the
        // snapshot block carries scalar predicates only.
        assert_block_parity(
            &sdl,
            &snapshot,
            "input ModelWhereInput",
            "input ModelWhereInput",
            &[],
        )
    }

    #[test]
    fn sdl_model_support_inputs_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "input ModelCardInput",
            "input ModelCardReasoningInput",
            "input ModelCardModalitiesInput",
            "input ModelCardCostInput",
            "input ModelCardLimitInput",
            "input ModelSettingsInput",
            "input ModelAssociationInput",
            "input ModelAssociationWhenInput",
            "input ChannelModelAssociationInput",
            "input ChannelRegexAssociationInput",
            "input RegexAssociationInput",
            "input ModelIDAssociationInput",
            "input ExcludeAssociationInput",
            "input ChannelTagsModelAssociationInput",
            "input ChannelTagsRegexAssociationInput",
            "input FilterConditionInput",
            "input ModelOrder",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }

        // The block comparison strips default values; pin the `= ASC`
        // default of ModelOrder.direction exactly (snapshot line 3658) by
        // slicing the raw generated block.
        let start = sdl
            .find("input ModelOrder {")
            .ok_or("input ModelOrder not found in generated SDL")?;
        let end = sdl[start..]
            .find('}')
            .map(|offset| start + offset)
            .ok_or("input ModelOrder block never closed")?;
        assert!(
            sdl[start..end].contains("direction: OrderDirection! = ASC"),
            "generated SDL must render the ASC default on ModelOrder.direction"
        );
        assert!(snapshot.contains("direction: OrderDirection! = ASC"));
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: enums
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_model_enums_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "enum ModelStatus",
            "enum ModelType",
            "enum ModelOrderField",
            "enum FilterConditionType",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        // Spot-check the value counts (5 model types; 3 order fields — no
        // STATUS/TYPE ordering exists for models, unlike channels).
        let type_values = block_fields(&sdl, "enum ModelType")?;
        assert_eq!(type_values.len(), 5, "ModelType must carry 5 values");
        let order_values = block_fields(&sdl, "enum ModelOrderField")?;
        assert_eq!(
            order_values.len(),
            3,
            "ModelOrderField must carry exactly CREATED_AT/UPDATED_AT/NAME"
        );
        Ok(())
    }

    // ---------------------------------------------------------------------
    // SDL parity: root operation signatures
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_models_query_and_crud_mutations_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;

        // Query.models — async-graphql renders arguments inline; the
        // snapshot spreads them across lines (type Query, lines 5372-5402).
        assert!(
            sdl.contains(
                "models(after: Cursor, first: Int, before: Cursor, last: Int, \
                 orderBy: ModelOrder, where: ModelWhereInput): ModelConnection!"
            ),
            "generated SDL missing the models connection signature: {sdl}"
        );
        for token in [
            "orderBy: ModelOrder",
            "where: ModelWhereInput",
            "): ModelConnection!",
        ] {
            assert!(
                snapshot.contains(token),
                "snapshot missing models arg token `{token}`"
            );
        }

        // Mutations — flat one-line signatures in both dialects (snapshot
        // extend type Mutation, lines 9114/9116/9117).
        for signature in [
            "createModel(input: CreateModelInput!): Model!",
            "updateModel(id: ID!, input: UpdateModelInput!): Model!",
            "deleteModel(id: ID!): Boolean!",
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
    // Ordering lowering (Go ent.resolvers.go:372-374)
    // ---------------------------------------------------------------------

    #[test]
    fn resolve_model_order_remaps_created_at_to_default_id_order() {
        let selection = resolve_model_order(Some(ModelOrder {
            direction: OrderDirection::Desc,
            field: ModelOrderField::CreatedAt,
        }));
        assert_eq!(
            selection,
            Some(ModelOrderSelection {
                direction: OrderDirection::Desc,
                term: ModelOrderTerm::Id,
            })
        );
    }

    #[test]
    fn resolve_model_order_maps_other_fields_one_to_one() {
        for (field, term) in [
            (ModelOrderField::UpdatedAt, ModelOrderTerm::UpdatedAt),
            (ModelOrderField::Name, ModelOrderTerm::Name),
        ] {
            let selection = resolve_model_order(Some(ModelOrder {
                direction: OrderDirection::Asc,
                field,
            }));
            assert_eq!(
                selection.map(|s| s.term),
                Some(term),
                "field {field:?} must map to {term:?}"
            );
        }
        assert_eq!(resolve_model_order(None), None);
    }

    // ---------------------------------------------------------------------
    // Input → object conversion semantics
    // ---------------------------------------------------------------------

    #[test]
    fn model_card_conversion_zero_fills_go_value_fields() {
        // objects.ModelCard is all value fields: an empty input becomes the
        // Go zero value, and the nullable contract fields still carry a
        // zero-filled value (never null).
        let card = ModelCard::from(ModelCardInput::default());
        assert_eq!(
            card.reasoning,
            ModelCardReasoning {
                supported: false,
                default_: false,
            }
        );
        assert!(!card.tool_call);
        assert!(!card.temperature);
        assert_eq!(card.modalities, ModelCardModalities::default());
        assert_eq!(card.vision, Some(false));
        assert_eq!(card.cost, ModelCardCost::default());
        assert_eq!(card.limit, ModelCardLimit::default());
        assert_eq!(card.knowledge.as_deref(), Some(""));
        assert_eq!(card.release_date.as_deref(), Some(""));
        assert_eq!(card.last_updated.as_deref(), Some(""));
    }

    #[test]
    fn association_conversion_zero_fills_values_and_passes_pointers_through() {
        let converted = ModelAssociation::from(ModelAssociationInput {
            association_type: "channel_model".to_owned(),
            priority: None,
            disabled: None,
            when: Some(ModelAssociationWhenInput {
                enabled: Some(true),
                condition: Some(FilterConditionInput {
                    condition_type: FilterConditionType::Group,
                    logic: Some("AND".to_owned()),
                    conditions: None,
                    field: None,
                    operator: None,
                    value: None,
                }),
            }),
            channel_model: Some(ChannelModelAssociationInput {
                channel_id: 7,
                model_id: "gpt-4o".to_owned(),
            }),
            channel_regex: None,
            regex: None,
            model_id: None,
            channel_tags_model: None,
            channel_tags_regex: None,
        });

        assert_eq!(converted.association_type, "channel_model");
        // Go value fields zero-fill (Priority int / Disabled bool).
        assert_eq!(converted.priority, 0);
        assert!(!converted.disabled);
        // Go pointer fields pass through: set ones convert, unset stay null.
        let when = match converted.when {
            Some(when) => when,
            None => panic!("when must convert"),
        };
        assert!(when.enabled);
        let condition = match when.condition {
            Some(condition) => condition,
            None => panic!("condition must convert"),
        };
        assert_eq!(condition.condition_type, FilterConditionType::Group);
        assert_eq!(condition.logic.as_deref(), Some("AND"));
        // objects.Condition.Field/Operator are value strings → zero-fill "".
        assert_eq!(condition.field.as_deref(), Some(""));
        assert_eq!(condition.operator.as_deref(), Some(""));
        // objects.Condition.Value is `any` → stays null.
        assert_eq!(condition.value, None);
        assert_eq!(
            converted.channel_model,
            Some(ChannelModelAssociation {
                channel_id: 7,
                model_id: "gpt-4o".to_owned(),
            })
        );
        assert_eq!(converted.regex, None);
        assert_eq!(converted.model_id, None);
    }

    // ---------------------------------------------------------------------
    // Resolver: createModel
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn create_model_returns_created_model_with_ent_defaults() -> Result<(), TestError> {
        let store = InMemoryModelService::default();
        let schema = schema_with(&store);

        // `type` omitted → ent column default `chat`; status is not part of
        // CreateModelInput at all → ent column default `disabled`.
        let resp = schema
            .execute(
                r#"mutation {
                    createModel(input: {
                        developer: "deepseek",
                        modelID: "deepseek-chat",
                        name: "DeepSeek Chat",
                        icon: "DeepSeek",
                        group: "deepseek",
                        modelCard: {},
                        settings: { associations: [] },
                        remark: "primary"
                    }) {
                        id developer modelID type name icon group status remark associatedChannelCount
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let created = &data["createModel"];
        assert_eq!(created["id"], "1");
        assert_eq!(created["developer"], "deepseek");
        assert_eq!(created["modelID"], "deepseek-chat");
        assert_eq!(created["type"], "chat");
        assert_eq!(created["name"], "DeepSeek Chat");
        assert_eq!(created["icon"], "DeepSeek");
        assert_eq!(created["group"], "deepseek");
        assert_eq!(created["status"], "disabled");
        assert_eq!(created["remark"], "primary");
        assert_eq!(created["associatedChannelCount"], 0);
        Ok(())
    }

    #[tokio::test]
    async fn associated_channel_count_uses_injected_force_resolver() -> Result<(), TestError> {
        let store = InMemoryModelService::default();
        let schema = schema_with_count(&store, 7);
        let resp = schema
            .execute(
                r#"mutation {
                    createModel(input: {
                        developer: "openai", modelID: "gpt-4o", name: "GPT-4o",
                        icon: "OpenAI", group: "openai", modelCard: {},
                        settings: { associations: [] }
                    }) { associatedChannelCount }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data.into_json()?["createModel"]["associatedChannelCount"],
            7
        );
        Ok(())
    }

    #[tokio::test]
    async fn create_model_zero_fills_empty_card_like_gqlgen() -> Result<(), TestError> {
        // Pins the Go value-field decoding on the wire: an empty
        // `modelCard: {}` comes back with zero values, NOT nulls, because
        // objects.ModelCard has no pointer fields.
        let store = InMemoryModelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createModel(input: {
                        developer: "d", modelID: "m", name: "n", icon: "i",
                        group: "g", modelCard: {}, settings: { associations: [] }
                    }) {
                        modelCard {
                            reasoning { supported default }
                            toolCall temperature vision knowledge
                            modalities { input output }
                            cost { input output cacheRead cacheWrite }
                            limit { context output }
                        }
                        settings { disableDeveloperSettingsInheritance associations { type } }
                        remark
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let card = &data["createModel"]["modelCard"];
        assert_eq!(card["reasoning"]["supported"], false);
        assert_eq!(card["reasoning"]["default"], false);
        assert_eq!(card["toolCall"], false);
        assert_eq!(card["temperature"], false);
        assert_eq!(card["vision"], false);
        assert_eq!(card["knowledge"], "");
        assert_eq!(card["modalities"]["input"], serde_json::json!([]));
        assert_eq!(card["modalities"]["output"], serde_json::json!([]));
        assert_eq!(card["cost"]["input"], 0.0);
        assert_eq!(card["cost"]["cacheWrite"], 0.0);
        assert_eq!(card["limit"]["context"], 0);
        let settings = &data["createModel"]["settings"];
        assert_eq!(settings["disableDeveloperSettingsInheritance"], false);
        assert_eq!(settings["associations"], serde_json::json!([]));
        // remark was omitted → ent leaves the nillable column NULL.
        assert_eq!(data["createModel"]["remark"], serde_json::Value::Null);
        Ok(())
    }

    #[tokio::test]
    async fn create_model_duplicate_model_id_surfaces_go_error_message() {
        let store = InMemoryModelService::default();
        lock(&store.models).push(sample_model(1, "deepseek-chat"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    createModel(input: {
                        developer: "other",
                        modelID: "deepseek-chat",
                        name: "clone",
                        icon: "x",
                        group: "g",
                        modelCard: {},
                        settings: { associations: [] }
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        // Go xerrors.DuplicateNameError("model", input.ModelID):
        // "%s name '%s' already exists" — the MODEL ID fills the name slot.
        assert!(
            message.contains("model name 'deepseek-chat' already exists"),
            "unexpected error message: {message}"
        );
    }

    #[tokio::test]
    async fn create_model_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema
            .execute(
                r#"mutation {
                    createModel(input: {
                        developer: "d", modelID: "m", name: "n", icon: "i",
                        group: "g", modelCard: {}, settings: { associations: [] }
                    }) { id }
                }"#,
            )
            .await;
        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("model service is not available"),
            "unexpected error message: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: updateModel
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn update_model_applies_partial_merge() -> Result<(), TestError> {
        let store = InMemoryModelService::default();
        lock(&store.models).push(sample_model(7, "deepseek-chat"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateModel(id: "7", input: {
                        name: "Renamed",
                        status: archived,
                        remark: "note"
                    }) { id name status remark developer modelID type }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let updated = &data["updateModel"];
        assert_eq!(updated["id"], "7");
        assert_eq!(updated["name"], "Renamed");
        assert_eq!(updated["status"], "archived");
        assert_eq!(updated["remark"], "note");
        // Unset input fields keep the stored values (partial merge).
        assert_eq!(updated["developer"], "deepseek");
        assert_eq!(updated["modelID"], "deepseek-chat");
        assert_eq!(updated["type"], "chat");
        Ok(())
    }

    #[tokio::test]
    async fn update_model_clear_remark_wins_over_simultaneous_set() -> Result<(), TestError> {
        // Go biz/model.go:433-439: SetRemark runs first, ClearRemark second —
        // when both are provided the clear wins.
        let store = InMemoryModelService::default();
        let mut seeded = sample_model(3, "m3");
        seeded.remark = Some("keep?".to_owned());
        lock(&store.models).push(seeded);
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    updateModel(id: "3", input: {
                        remark: "overwritten",
                        clearRemark: true
                    }) { remark }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updateModel"]["remark"], serde_json::Value::Null);
        Ok(())
    }

    #[tokio::test]
    async fn update_model_allows_duplicate_model_id_go_parity() -> Result<(), TestError> {
        // Go UpdateModel (biz/model.go:408) performs NO duplicate check —
        // unlike ChannelService.UpdateChannel. Renaming a model onto another
        // row's modelID must therefore succeed at the service layer.
        let store = InMemoryModelService::default();
        lock(&store.models).push(sample_model(1, "m-a"));
        lock(&store.models).push(sample_model(2, "m-b"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateModel(id: "2", input: { modelID: "m-a" }) { modelID } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["updateModel"]["modelID"], "m-a");
        Ok(())
    }

    #[tokio::test]
    async fn update_model_missing_id_surfaces_wrapped_not_found() {
        let store = InMemoryModelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateModel(id: "404", input: { name: "x" }) { id } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("failed to update model: ent: model not found"),
            "unexpected error message: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: deleteModel
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn delete_model_returns_true_and_removes_row() -> Result<(), TestError> {
        let store = InMemoryModelService::default();
        lock(&store.models).push(sample_model(5, "victim"));
        let schema = schema_with(&store);

        let resp = schema.execute(r#"mutation { deleteModel(id: "5") }"#).await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["deleteModel"], true);
        assert!(lock(&store.models).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn delete_model_missing_id_surfaces_wrapped_error() {
        let store = InMemoryModelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { deleteModel(id: "404") }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("failed to delete model: ent: model not found"),
            "unexpected error message: {message}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolver: models connection query
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn models_returns_connection_with_total_count() -> Result<(), TestError> {
        let store = InMemoryModelService::default();
        lock(&store.models).push(sample_model(1, "a"));
        lock(&store.models).push(sample_model(2, "b"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    models {
                        totalCount
                        edges { cursor node { id modelID } }
                        pageInfo { hasNextPage hasPreviousPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let connection = &data["models"];
        assert_eq!(connection["totalCount"], 2);
        assert_eq!(connection["edges"][0]["node"]["modelID"], "a");
        assert_eq!(connection["edges"][1]["node"]["id"], "2");
        assert_eq!(connection["pageInfo"]["hasNextPage"], false);
        Ok(())
    }

    #[tokio::test]
    async fn models_empty_store_returns_empty_connection() -> Result<(), TestError> {
        let store = InMemoryModelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"{ models { totalCount edges { cursor } } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["models"]["totalCount"], 0);
        assert_eq!(data["models"]["edges"], serde_json::json!([]));
        Ok(())
    }

    #[tokio::test]
    async fn models_first_limits_page_and_flags_next_page() -> Result<(), TestError> {
        let store = InMemoryModelService::default();
        for (id, model_id) in [(1, "a"), (2, "b"), (3, "c")] {
            lock(&store.models).push(sample_model(id, model_id));
        }
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    models(first: 2) {
                        totalCount
                        edges { node { modelID } }
                        pageInfo { hasNextPage }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["models"]["totalCount"], 3);
        assert_eq!(
            data["models"]["edges"].as_array().map(std::vec::Vec::len),
            Some(2)
        );
        assert_eq!(data["models"]["pageInfo"]["hasNextPage"], true);
        Ok(())
    }

    #[tokio::test]
    async fn models_created_at_order_remaps_to_default_id_term() -> Result<(), TestError> {
        // Go Query.models (ent.resolvers.go:372-374): CREATED_AT is replaced
        // by ent.DefaultModelOrder (ID) with direction preserved.
        let store = InMemoryModelService::default();
        lock(&store.models).push(sample_model(1, "a"));
        lock(&store.models).push(sample_model(2, "b"));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    models(orderBy: { field: CREATED_AT, direction: DESC }) {
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
            Some(ModelOrderSelection {
                direction: OrderDirection::Desc,
                term: ModelOrderTerm::Id,
            })
        );
        // Desc-by-ID ordering is observable in the page.
        let data = resp.data.into_json()?;
        assert_eq!(data["models"]["edges"][0]["node"]["id"], "2");
        Ok(())
    }

    #[tokio::test]
    async fn models_order_direction_defaults_to_asc_when_omitted() -> Result<(), TestError> {
        // Contract: `direction: OrderDirection! = ASC` (snapshot line 3658).
        let store = InMemoryModelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"{ models(orderBy: { field: NAME }) { totalCount } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let captured = lock(&store.captured_query_args).clone();
        assert_eq!(
            captured[0].order_by,
            Some(ModelOrderSelection {
                direction: OrderDirection::Asc,
                term: ModelOrderTerm::Name,
            })
        );
        Ok(())
    }

    #[tokio::test]
    async fn models_passes_where_filter_through_to_service() -> Result<(), TestError> {
        // Go passes `where` straight to ent (`WithModelFilter`); the Rust
        // resolver must hand the predicate input to the service verbatim.
        let store = InMemoryModelService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"{
                    models(where: {
                        modelIDContainsFold: "deep",
                        statusIn: [enabled, disabled],
                        typeNEQ: embedding,
                        remarkIsNil: true
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
        assert_eq!(filter.model_id_contains_fold.as_deref(), Some("deep"));
        assert_eq!(
            filter.status_in,
            Some(vec![ModelStatus::Enabled, ModelStatus::Disabled])
        );
        assert_eq!(filter.type_neq, Some(ModelType::Embedding));
        assert_eq!(filter.remark_is_nil, Some(true));
        Ok(())
    }

    #[tokio::test]
    async fn models_without_wired_service_reports_unavailable() {
        let schema = bare_schema();
        let resp = schema.execute(r#"{ models { totalCount } }"#).await;
        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("model service is not available"),
            "unexpected error message: {message}"
        );
    }
}
