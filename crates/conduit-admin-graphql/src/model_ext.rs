//! GAP-B — Model extended-domain GraphQL resolvers.
//!
//! Ports the extended model queries/mutations declared in
//! `conduit/internal/server/gql/model.graphql` + `price.graphql` (Go) and
//! resolved in `model.resolvers.go` / `price.resolvers.go`. The Rust SDL must
//! match the captured snapshot at
//! `tests/contracts/admin_graphql_schema.graphql`.
//!
//! ## Operations ported
//!
//! Queries (snapshot lines 9104-9111):
//!   - `Query.fetchModels(input: FetchModelsInput!): FetchModelsPayload!` — Go
//!     resolver `FetchModels` (`model.resolvers.go:104-126`): require the
//!     write-channels scope, call the model fetcher, map the result to the
//!     payload (`models` + optional `error`).
//!   - `Query.queryModels(input: QueryModelsInput!): [ModelIdentityWithStatus!]!`
//!     — Go resolver `QueryModels` (`model.resolvers.go:128-169`): branch on
//!     `systemService.ModelSettingsOrDefault().QueryAllChannelModels ||
//!     input.includeAllChannelModels` — when true, list all channel models
//!     (`channelService.ListModels`); otherwise list configured models
//!     (`modelService.ListModels`). The whole branch is delegated to the host
//!     so this crate stays free of the system/channel/model service split.
//!   - `Query.queryModelChannelConnections(associations: [ModelAssociationInput!]!):
//!     [ModelChannelConnection!]!` — Go resolver
//!     (`model.resolvers.go:171-174`): `modelService.QueryModelChannelConnections`.
//!   - `Query.queryUnassociatedChannels: [UnassociatedChannel!]!` — Go resolver
//!     (`model.resolvers.go:176-179`): `modelService.QueryUnassociatedChannels`.
//!
//! Mutations (snapshot lines 9113-9123, 9214-9216):
//!   - `bulkCreateModels(inputs: [CreateModelInput!]!): [Model!]!` — Go
//!     `BulkCreateModels` (`model.resolvers.go:31-34`).
//!   - `updateModelStatus(id: ID!, status: ModelStatus!): Boolean!` — Go
//!     `UpdateModelStatus` (`model.resolvers.go:50-58`): returns `true` on
//!     success, `false, err` on failure.
//!   - `bulkArchiveModels(ids: [ID!]!): Boolean!` — Go `BulkArchiveModels`
//!     (`model.resolvers.go:60-69`); ids decoded via `objects.IntGuids`.
//!   - `bulkDisableModels(ids: [ID!]!): Boolean!` — Go `BulkDisableModels`
//!     (`model.resolvers.go:71-80`).
//!   - `bulkEnableModels(ids: [ID!]!): Boolean!` — Go `BulkEnableModels`
//!     (`model.resolvers.go:82-91`).
//!   - `bulkDeleteModels(ids: [ID!]!): Boolean!` — Go `BulkDeleteModels`
//!     (`model.resolvers.go:93-102`).
//!   - `saveChannelModelPrices(channelId: ID!, input: [SaveChannelModelPriceInput!]!):
//!     [ChannelModelPrice!]!` — retained as a compatibility entry point that
//!     stages a provider-price ChangeSet instead of writing formal prices.
//!
//! ## Deferred (declared by the snapshot but NOT completed in this slice)
//!
//!   - `type ChannelModelPrice implements Node` (snapshot line 1900) is
//!     defined here with its scalar + self-domain fields and the `price:
//!     ModelPrice!` embedded object, but the two Node-edge fields — `channel:
//!     Channel!` (line 1914) and `versions: [ChannelModelPriceVersion!]` (line
//!     1915) — are deferred, matching the codebase convention that
//!     cross-domain edges land in a follow-up slice. The `implements Node`
//!     clause is contributed by the shared `crate::channel::Node` interface
//!     enum (not touched here); until `ChannelModelPrice` is added to that
//!     enum the SDL emits `type ChannelModelPrice` without `implements Node`.
//!     See the Leader wiring notes.
//!
//! ## Service wiring
//!
//! The admin-graphql crate stays free of DB / HTTP concerns. The host wires a
//! concrete implementation of [`ModelExtServices`] into the schema data bag
//! at build time; resolver-level tests inject an in-memory fake. Mirrors the
//! dependency-injection pattern used by
//! [`crate::system::SystemSettingsServices`] and
//! [`crate::model::ModelQueryServices`].

use std::sync::Arc;

use async_graphql::{Context, Enum, ID, InputObject, SimpleObject};

use crate::channel::{Channel, ChannelStatus};
use crate::model::{CreateModelInput, Model, ModelAssociationInput, ModelStatus};
use crate::request_usage::PriceItemCode;
use crate::scalars::{DecimalScalar, TimeScalar};

// ===========================================================================
// Enums — snapshot-exact value spellings (lowercase/snake_case pinned so the
// default SCREAMING_SNAKE renaming cannot mangle them).
// ===========================================================================

/// `enum PricingMode { flat_fee usage_per_unit usage_tiered }` — snapshot
/// lines 9127-9131, bound to Go `objects.PricingMode` (`objects/price.go:9`).
/// The Go type also declares `usage_volume` but the GraphQL contract snapshot
/// only surfaces these three; the SDL is the contract, so only three ship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PricingMode {
    #[graphql(name = "flat_fee")]
    FlatFee,
    #[graphql(name = "usage_per_unit")]
    UsagePerUnit,
    #[graphql(name = "usage_tiered")]
    UsageTiered,
}

/// `enum PromptWriteCacheVariantCode { five_min one_hour }` — snapshot lines
/// 9140-9143, bound to Go `objects.PromptWriteCacheVariantCode`
/// (`objects/price.go:246-253`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
pub enum PromptWriteCacheVariantCode {
    #[graphql(name = "five_min")]
    FiveMin,
    #[graphql(name = "one_hour")]
    OneHour,
}

// ===========================================================================
// Output types — model extended queries.
// ===========================================================================

/// `type ModelIdentify { id: String! }` — snapshot lines 505-507. Mirrors Go
/// `biz.ModelIdentify` (`biz/model.go:867-869`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ModelIdentify")]
pub struct ModelIdentify {
    pub id: String,
}

/// `type FetchModelsPayload { models: [ModelIdentify!]! error: String }` —
/// snapshot lines 500-503. Mirrors the gql-layer `FetchModelsPayload`
/// (`models_gen.go:248`) built from `biz.FetchModelsResult`.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "FetchModelsPayload")]
pub struct FetchModelsPayload {
    pub models: Vec<ModelIdentify>,
    /// `error: String` — nullable; carries the fetcher's soft error string
    /// (Go `result.Error` is `*string`).
    pub error: Option<String>,
}

/// `type ModelIdentityWithStatus { id: String! status: ChannelStatus! }` —
/// snapshot lines 509-512. Mirrors Go `biz.ModelIdentityWithStatus`
/// (`biz/channel.go:407-410`); note the status is the **channel** status enum,
/// not the model status.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ModelIdentityWithStatus")]
pub struct ModelIdentityWithStatus {
    pub id: String,
    pub status: ChannelStatus,
}

/// `type ChannelModelEntry { requestModel: String! actualModel: String!
/// source: String! }` — snapshot lines 9089-9093. Mirrors Go
/// `biz.ChannelModelEntry` (`biz/channel.go:30-40`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ChannelModelEntry")]
pub struct ChannelModelEntry {
    pub request_model: String,
    pub actual_model: String,
    pub source: String,
}

/// `type ModelChannelConnection { channel: Channel! models: [ChannelModelEntry!]!
/// priority: Int! }` — snapshot lines 9083-9087. Mirrors Go
/// `biz.ModelChannelConnection` (`biz/model_association_matcher.go:15-19`).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "ModelChannelConnection")]
pub struct ModelChannelConnection {
    pub channel: Channel,
    pub models: Vec<ChannelModelEntry>,
    pub priority: i32,
}

/// `type UnassociatedChannel { channel: Channel! models: [String!]! }` —
/// snapshot lines 9095-9098. Mirrors Go `biz.UnassociatedChannel`
/// (`biz/model.go:871-874`).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "UnassociatedChannel")]
pub struct UnassociatedChannel {
    pub channel: Channel,
    pub models: Vec<String>,
}

// ===========================================================================
// Output types — ModelPrice pricing family (embedded object, self-domain).
// ===========================================================================

/// `type PriceTier { upTo: Int pricePerUnit: Decimal! }` — snapshot lines
/// 9167-9170. Mirrors Go `objects.PriceTier` (`objects/price.go:204-206`);
/// `upTo` is `*int64` (nullable, last tier omits it).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "PriceTier")]
pub struct PriceTier {
    pub up_to: Option<i64>,
    pub price_per_unit: DecimalScalar,
}

/// `type TieredPricing { tiers: [PriceTier!]! }` — snapshot lines 9163-9165.
/// Mirrors Go `objects.TieredPricing` (`objects/price.go:181-183`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "TieredPricing")]
pub struct TieredPricing {
    pub tiers: Vec<PriceTier>,
}

/// `type Pricing { mode: PricingMode! flatFee: Decimal usagePerUnit: Decimal
/// usageTiered: TieredPricing }` — snapshot lines 9156-9161. Mirrors Go
/// `objects.Pricing` (`objects/price.go:32-38`); the three non-mode fields are
/// nullable (`*decimal.Decimal` / `*TieredPricing`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "Pricing")]
pub struct Pricing {
    pub mode: PricingMode,
    pub flat_fee: Option<DecimalScalar>,
    pub usage_per_unit: Option<DecimalScalar>,
    pub usage_tiered: Option<TieredPricing>,
}

/// `type PromptWriteCacheVariant { variantCode: PromptWriteCacheVariantCode!
/// pricing: Pricing! }` — snapshot lines 9172-9175. Mirrors Go
/// `objects.PromptWriteCacheVariant` (`objects/price.go:257-260`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "PromptWriteCacheVariant")]
pub struct PromptWriteCacheVariant {
    pub variant_code: PromptWriteCacheVariantCode,
    pub pricing: Pricing,
}

/// `type ModelPriceItem { itemCode: PriceItemCode! pricing: Pricing!
/// promptWriteCacheVariants: [PromptWriteCacheVariant!] }` — snapshot lines
/// 9150-9154. Mirrors Go `objects.ModelPriceItem` (`objects/price.go:289-297`);
/// `promptWriteCacheVariants` is a nullable list (Go `omitempty`). `itemCode`
/// reuses the shared [`crate::request_usage::PriceItemCode`] enum.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ModelPriceItem")]
pub struct ModelPriceItem {
    pub item_code: PriceItemCode,
    pub pricing: Pricing,
    pub prompt_write_cache_variants: Option<Vec<PromptWriteCacheVariant>>,
}

/// `type ModelPrice { items: [ModelPriceItem!]! }` — snapshot lines 9146-9148.
/// Mirrors Go `objects.ModelPrice` (`objects/price.go:328-330`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ModelPrice")]
pub struct ModelPrice {
    pub items: Vec<ModelPriceItem>,
}

/// `type ChannelModelPrice implements Node` — snapshot lines 1900-1916.
/// Scalar + self-domain fields only (`price: ModelPrice!` embedded object is
/// self-domain). The two Node-edge fields `channel: Channel!` and
/// `versions: [ChannelModelPriceVersion!]` are DEFERRED (module doc). The
/// `implements Node` clause is contributed by the shared `crate::channel::Node`
/// interface enum, not this struct — see Leader wiring notes.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ChannelModelPrice")]
pub struct ChannelModelPrice {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    /// `channelID: ID!` — all-caps `ID` acronym tag (snapshot line 1904);
    /// camelCase would emit `channelId`.
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    /// `modelID: String!` — acronym tag (snapshot line 1905).
    #[graphql(name = "modelID")]
    pub model_id: String,
    /// Real-world accounting currency for this imported price.
    pub currency_code: String,
    pub price: ModelPrice,
    /// `referenceID: String!` — acronym tag (snapshot line 1913).
    #[graphql(name = "referenceID")]
    pub reference_id: String,
}

// ===========================================================================
// Input types.
// ===========================================================================

/// `input FetchModelsInput { channelType: String! baseURL: String! apiKey:
/// String channelID: ID }` — snapshot lines 493-498. Mirrors Go
/// `biz.FetchModelsInput` (`biz/model_fetcher.go:157-163`).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "FetchModelsInput")]
pub struct FetchModelsInput {
    pub channel_type: String,
    /// `baseURL: String!` — all-caps `URL` acronym tag (snapshot line 495);
    /// camelCase would emit `baseUrl`.
    #[graphql(name = "baseURL")]
    pub base_url: String,
    pub api_key: Option<String>,
    /// `channelID: ID` — acronym tag (snapshot line 497); nullable.
    #[graphql(name = "channelID")]
    pub channel_id: Option<ID>,
}

/// `input QueryModelsInput { statusIn: [ChannelStatus!] includeMapping: Boolean
/// includePrefix: Boolean includeAllChannelModels: Boolean }` — snapshot lines
/// 514-519. All fields nullable (Go pointers / `lo.FromPtrOr` defaults).
#[derive(Debug, Clone, PartialEq, Eq, Default, InputObject)]
#[graphql(name = "QueryModelsInput")]
pub struct QueryModelsInput {
    pub status_in: Option<Vec<ChannelStatus>>,
    pub include_mapping: Option<bool>,
    pub include_prefix: Option<bool>,
    pub include_all_channel_models: Option<bool>,
}

/// `input PriceTierInput { upTo: Int pricePerUnit: Decimal! }` — snapshot
/// lines 9199-9202.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "PriceTierInput")]
pub struct PriceTierInput {
    pub up_to: Option<i64>,
    pub price_per_unit: DecimalScalar,
}

/// `input TieredPricingInput { tiers: [PriceTierInput!]! }` — snapshot lines
/// 9195-9197.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "TieredPricingInput")]
pub struct TieredPricingInput {
    pub tiers: Vec<PriceTierInput>,
}

/// `input PricingInput { mode: PricingMode! flatFee: Decimal usagePerUnit:
/// Decimal usageTiered: TieredPricingInput }` — snapshot lines 9188-9193.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "PricingInput")]
pub struct PricingInput {
    pub mode: PricingMode,
    pub flat_fee: Option<DecimalScalar>,
    pub usage_per_unit: Option<DecimalScalar>,
    pub usage_tiered: Option<TieredPricingInput>,
}

/// `input PromptWriteCacheVariantInput { variantCode:
/// PromptWriteCacheVariantCode! pricing: PricingInput! }` — snapshot lines
/// 9204-9207.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "PromptWriteCacheVariantInput")]
pub struct PromptWriteCacheVariantInput {
    pub variant_code: PromptWriteCacheVariantCode,
    pub pricing: PricingInput,
}

/// `input ModelPriceItemInput { itemCode: PriceItemCode! pricing: PricingInput!
/// promptWriteCacheVariants: [PromptWriteCacheVariantInput!] }` — snapshot
/// lines 9182-9186.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "ModelPriceItemInput")]
pub struct ModelPriceItemInput {
    pub item_code: PriceItemCode,
    pub pricing: PricingInput,
    pub prompt_write_cache_variants: Option<Vec<PromptWriteCacheVariantInput>>,
}

/// `input ModelPriceInput { items: [ModelPriceItemInput!]! }` — snapshot lines
/// 9178-9180.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "ModelPriceInput")]
pub struct ModelPriceInput {
    pub items: Vec<ModelPriceItemInput>,
}

/// Channel import-price input. `currencyCode` is explicit and required so an
/// edit never silently relabels an existing numeric price with the system's
/// current accounting currency. NOTE the field is `modelId` (camel) here, in
/// contrast to the acronym `modelID` on the `ChannelModelPrice` output type.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "SaveChannelModelPriceInput")]
pub struct SaveChannelModelPriceInput {
    pub model_id: String,
    pub currency_code: String,
    pub price: ModelPriceInput,
}

// ===========================================================================
// Service trait (host-injected).
// ===========================================================================

/// Error surface for the model-ext slice. `ServiceUnavailable` mirrors the
/// unwired-schema fallback used across the crate; `FetchModels` mirrors the Go
/// wrapping prefix (`model.resolvers.go:113`). The other variants carry the
/// raw Go error string (the Go resolvers return `err` / `false, err`
/// unwrapped).
#[derive(Debug, Clone, thiserror::Error)]
pub enum ModelExtError {
    #[error("model service is not available")]
    ServiceUnavailable,
    /// Mirrors Go `model.resolvers.go:113`
    /// (`fmt.Errorf("failed to fetch models: %w", err)`).
    #[error("failed to fetch models: {0}")]
    FetchModels(String),
    #[error("failed to query models: {0}")]
    QueryModels(String),
    #[error("failed to query model channel connections: {0}")]
    QueryModelChannelConnections(String),
    #[error("failed to query unassociated channels: {0}")]
    QueryUnassociatedChannels(String),
    #[error("failed to update model status: {0}")]
    UpdateModelStatus(String),
    #[error("failed to bulk create models: {0}")]
    BulkCreateModels(String),
    #[error("failed to bulk archive models: {0}")]
    BulkArchiveModels(String),
    #[error("failed to bulk disable models: {0}")]
    BulkDisableModels(String),
    #[error("failed to bulk enable models: {0}")]
    BulkEnableModels(String),
    #[error("failed to bulk delete models: {0}")]
    BulkDeleteModels(String),
    #[error("failed to save channel model prices: {0}")]
    SaveChannelModelPrices(String),
}

/// Backs the model extended-domain queries + mutations. Each method
/// corresponds to one Go resolver; the host implementation owns the request
/// context, scope checks, and the service-layer split (fetcher / channel /
/// model / system services).
#[async_trait::async_trait]
pub trait ModelExtServices: Send + Sync {
    /// Mirrors Go `FetchModels` (`model.resolvers.go:104-126`): the host is
    /// responsible for the `RequireScope(WriteChannels)` check the Go resolver
    /// performs before delegating to the fetcher, and for mapping the
    /// fetcher result to the payload.
    async fn fetch_models(
        &self,
        input: FetchModelsInput,
    ) -> Result<FetchModelsPayload, ModelExtError>;

    /// Mirrors Go `QueryModels` (`model.resolvers.go:128-169`): the host
    /// implements the full branch — read `ModelSettingsOrDefault`, and when
    /// `QueryAllChannelModels || input.includeAllChannelModels` list all
    /// channel models, otherwise list configured models.
    async fn query_models(
        &self,
        input: QueryModelsInput,
    ) -> Result<Vec<ModelIdentityWithStatus>, ModelExtError>;

    /// Mirrors Go `QueryModelChannelConnections`
    /// (`model.resolvers.go:171-174`): resolve the associations into channel
    /// connections. The host decodes each [`ModelAssociationInput`] into the
    /// Go `objects.ModelAssociation` shape.
    async fn query_model_channel_connections(
        &self,
        associations: Vec<ModelAssociationInput>,
    ) -> Result<Vec<ModelChannelConnection>, ModelExtError>;

    /// Mirrors Go `QueryUnassociatedChannels`
    /// (`model.resolvers.go:176-179`).
    async fn query_unassociated_channels(&self) -> Result<Vec<UnassociatedChannel>, ModelExtError>;

    /// Mirrors Go `BulkCreateModels` (`model.resolvers.go:31-34`).
    async fn bulk_create_models(
        &self,
        inputs: Vec<CreateModelInput>,
    ) -> Result<Vec<Model>, ModelExtError>;

    /// Mirrors Go `UpdateModelStatus` (`model.resolvers.go:50-58`): the `id`
    /// carries the `gid://conduit/Model/<id>` wire form; the host decodes it.
    /// Returns unit on success (the resolver maps success to `true`).
    async fn update_model_status(&self, id: ID, status: ModelStatus) -> Result<(), ModelExtError>;

    /// Mirrors Go `BulkArchiveModels` (`model.resolvers.go:60-69`): the host
    /// decodes each id via the equivalent of `objects.IntGuids`.
    async fn bulk_archive_models(&self, ids: Vec<ID>) -> Result<(), ModelExtError>;

    /// Mirrors Go `BulkDisableModels` (`model.resolvers.go:71-80`).
    async fn bulk_disable_models(&self, ids: Vec<ID>) -> Result<(), ModelExtError>;

    /// Mirrors Go `BulkEnableModels` (`model.resolvers.go:82-91`).
    async fn bulk_enable_models(&self, ids: Vec<ID>) -> Result<(), ModelExtError>;

    /// Mirrors Go `BulkDeleteModels` (`model.resolvers.go:93-102`).
    async fn bulk_delete_models(&self, ids: Vec<ID>) -> Result<(), ModelExtError>;

    /// Compatibility entry point for the old price mutation. The host stages
    /// a provider-price ChangeSet and returns the unchanged formal prices.
    async fn save_channel_model_prices(
        &self,
        actor_user_id: Option<i64>,
        channel_id: ID,
        input: Vec<SaveChannelModelPriceInput>,
    ) -> Result<Vec<ChannelModelPrice>, ModelExtError>;
}

/// Resolves the injected [`ModelExtServices`] from the async-graphql context
/// data bag, surfacing the Go-equivalent "service unavailable" message when no
/// service was wired.
pub(crate) fn model_ext_services(ctx: &Context<'_>) -> Result<Arc<dyn ModelExtServices>, String> {
    match ctx.data::<Arc<dyn ModelExtServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ModelExtError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Resolver wiring (for the coordinator).
//
// IMPORTANT: async-graphql's `#[Object]` macro generates the resolver trait
// impls for the root type, so a root's `#[Object] impl` block CANNOT be split
// across modules (two blocks on the same type → E0119 conflicting impl). This
// slice therefore does NOT contribute its own `#[Object] impl QueryRoot` /
// `impl MutationRoot`; instead it exposes the typed service-lookup helper
// [`model_ext_services`] and the types, and the coordinator pastes the four
// query methods into the single `#[Object] impl QueryRoot` in `lib.rs` and the
// seven mutation methods into the single `#[Object] impl MutationRoot` in
// `mutation.rs`. The `TestQueryRoot` / `TestMutationRoot` in the test module
// below are byte-for-byte reference implementations (they compile + run the
// resolvers against the fake service).
//
// Query methods (paste into `#[Object] impl QueryRoot` in `lib.rs`):
//
// ```ignore
// async fn fetch_models(
//     &self,
//     ctx: &Context<'_>,
//     input: crate::model_ext::FetchModelsInput,
// ) -> Result<crate::model_ext::FetchModelsPayload, String> {
//     let s = crate::model_ext::model_ext_services(ctx)?;
//     s.fetch_models(input).await.map_err(|e| e.to_string())
// }
//
// async fn query_models(
//     &self,
//     ctx: &Context<'_>,
//     input: crate::model_ext::QueryModelsInput,
// ) -> Result<Vec<crate::model_ext::ModelIdentityWithStatus>, String> {
//     let s = crate::model_ext::model_ext_services(ctx)?;
//     s.query_models(input).await.map_err(|e| e.to_string())
// }
//
// async fn query_model_channel_connections(
//     &self,
//     ctx: &Context<'_>,
//     associations: Vec<crate::model::ModelAssociationInput>,
// ) -> Result<Vec<crate::model_ext::ModelChannelConnection>, String> {
//     let s = crate::model_ext::model_ext_services(ctx)?;
//     s.query_model_channel_connections(associations).await.map_err(|e| e.to_string())
// }
//
// async fn query_unassociated_channels(
//     &self,
//     ctx: &Context<'_>,
// ) -> Result<Vec<crate::model_ext::UnassociatedChannel>, String> {
//     let s = crate::model_ext::model_ext_services(ctx)?;
//     s.query_unassociated_channels().await.map_err(|e| e.to_string())
// }
// ```
//
// Mutation methods (paste into `#[Object] impl MutationRoot` in `mutation.rs`;
// each bulk/status mutation returns `true` on success, mirroring the Go
// resolvers that return `false, err` on failure and `true` otherwise):
//
// ```ignore
// async fn bulk_create_models(
//     &self,
//     ctx: &Context<'_>,
//     inputs: Vec<crate::model::CreateModelInput>,
// ) -> Result<Vec<crate::model::Model>, String> {
//     let s = crate::model_ext::model_ext_services(ctx)?;
//     s.bulk_create_models(inputs).await.map_err(|e| e.to_string())
// }
//
// async fn update_model_status(
//     &self,
//     ctx: &Context<'_>,
//     id: async_graphql::ID,
//     status: crate::model::ModelStatus,
// ) -> Result<bool, String> {
//     let s = crate::model_ext::model_ext_services(ctx)?;
//     s.update_model_status(id, status).await.map_err(|e| e.to_string())?;
//     Ok(true)
// }
//
// async fn bulk_archive_models(&self, ctx: &Context<'_>, ids: Vec<async_graphql::ID>) -> Result<bool, String> {
//     let s = crate::model_ext::model_ext_services(ctx)?;
//     s.bulk_archive_models(ids).await.map_err(|e| e.to_string())?;
//     Ok(true)
// }
// // bulk_disable_models / bulk_enable_models / bulk_delete_models — identical
// // shape, calling the matching trait method.
//
// async fn save_channel_model_prices(
//     &self,
//     ctx: &Context<'_>,
//     channel_id: async_graphql::ID,
//     input: Vec<crate::model_ext::SaveChannelModelPriceInput>,
// ) -> Result<Vec<crate::model_ext::ChannelModelPrice>, String> {
//     let s = crate::model_ext::model_ext_services(ctx)?;
//     s.save_channel_model_prices(channel_id, input).await.map_err(|e| e.to_string())
// }
// ```
// ===========================================================================

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::{EmptySubscription, Name, Object, Schema, SchemaBuilder, Value};

    use super::*;
    use crate::channel::{ChannelStatus, ChannelType};
    use crate::model::{ModelCard, ModelCardInput, ModelSettings, ModelSettingsInput, ModelType};

    // ---------------------------------------------------------------------
    // Helpers.
    // ---------------------------------------------------------------------

    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    /// Mutex-guard helper that never panics on poison.
    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn as_object(value: &Value) -> &async_graphql::indexmap::IndexMap<Name, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected Value::Object, got {other:?}"),
        }
    }

    fn sample_channel(id: i64, name: &str) -> Channel {
        Channel {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            channel_type: ChannelType::Openai,
            base_url: None,
            website_url: None,
            quota_currency: "USD".to_string(),
            actual_quota_used: None,
            quota_remaining: None,
            name: name.to_owned(),
            status: ChannelStatus::Enabled,
            supported_models: vec!["gpt-4o".to_owned()],
            manual_models: None,
            auto_sync_supported_models: false,
            auto_sync_model_pattern: None,
            tags: None,
            default_test_model: "gpt-4o".to_owned(),
            policies: None,
            settings: None,
            ordering_weight: 0,
            error_message: None,
            remark: None,
            endpoints: None,
        }
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
            model_card: ModelCard::from(ModelCardInput::default()),
            settings: ModelSettings::from(ModelSettingsInput::default()),
            status: ModelStatus::Enabled,
            remark: None,
            associated_channel_count: 0,
        }
    }

    fn sample_channel_model_price() -> ChannelModelPrice {
        ChannelModelPrice {
            id: ID::from("gid://conduit/ChannelModelPrice/1"),
            created_at: epoch(),
            updated_at: epoch(),
            channel_id: ID::from("gid://conduit/Channel/7"),
            model_id: "gpt-4o".to_owned(),
            currency_code: "CNY".to_owned(),
            price: ModelPrice {
                items: vec![ModelPriceItem {
                    item_code: PriceItemCode::PromptTokens,
                    pricing: Pricing {
                        mode: PricingMode::UsagePerUnit,
                        flat_fee: None,
                        usage_per_unit: Some(DecimalScalar(rust_decimal::Decimal::new(1, 4))),
                        usage_tiered: None,
                    },
                    prompt_write_cache_variants: None,
                }],
            },
            reference_id: "ref-1".to_owned(),
        }
    }

    // ---------------------------------------------------------------------
    // In-memory fake service.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct FakeModelExtServices {
        fetch_payload: FetchModelsPayloadStub,
        fetch_error: Option<ModelExtError>,
        query_models_result: Vec<ModelIdentityWithStatus>,
        query_models_error: Option<ModelExtError>,
        connections: Vec<ModelChannelConnectionStub>,
        connections_error: Option<ModelExtError>,
        unassociated: Vec<UnassociatedChannelStub>,
        unassociated_error: Option<ModelExtError>,
        created_models: Vec<Model>,
        create_error: Option<ModelExtError>,
        update_status_calls: Arc<Mutex<Vec<(String, ModelStatus)>>>,
        update_status_error: Option<ModelExtError>,
        archive_calls: Arc<Mutex<Vec<Vec<String>>>>,
        disable_calls: Arc<Mutex<Vec<Vec<String>>>>,
        enable_calls: Arc<Mutex<Vec<Vec<String>>>>,
        delete_calls: Arc<Mutex<Vec<Vec<String>>>>,
        bulk_error: Option<ModelExtError>,
        save_price_calls: Arc<Mutex<Vec<(String, usize)>>>,
        save_price_currencies: Arc<Mutex<Vec<Vec<String>>>>,
        save_price_result: Vec<ChannelModelPrice>,
        save_price_error: Option<ModelExtError>,
    }

    // Newtype stubs so the fake can carry defaults without the wrapped types
    // needing `Default` (Channel has no Default derive).
    #[derive(Clone, Default)]
    struct FetchModelsPayloadStub(Option<FetchModelsPayload>);
    #[derive(Clone)]
    struct ModelChannelConnectionStub(ModelChannelConnection);
    #[derive(Clone)]
    struct UnassociatedChannelStub(UnassociatedChannel);

    #[async_trait::async_trait]
    impl ModelExtServices for FakeModelExtServices {
        async fn fetch_models(
            &self,
            _input: FetchModelsInput,
        ) -> Result<FetchModelsPayload, ModelExtError> {
            match &self.fetch_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.fetch_payload.0.clone().unwrap_or(FetchModelsPayload {
                    models: Vec::new(),
                    error: None,
                })),
            }
        }

        async fn query_models(
            &self,
            _input: QueryModelsInput,
        ) -> Result<Vec<ModelIdentityWithStatus>, ModelExtError> {
            match &self.query_models_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.query_models_result.clone()),
            }
        }

        async fn query_model_channel_connections(
            &self,
            _associations: Vec<ModelAssociationInput>,
        ) -> Result<Vec<ModelChannelConnection>, ModelExtError> {
            match &self.connections_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.connections.iter().map(|c| c.0.clone()).collect()),
            }
        }

        async fn query_unassociated_channels(
            &self,
        ) -> Result<Vec<UnassociatedChannel>, ModelExtError> {
            match &self.unassociated_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.unassociated.iter().map(|c| c.0.clone()).collect()),
            }
        }

        async fn bulk_create_models(
            &self,
            _inputs: Vec<CreateModelInput>,
        ) -> Result<Vec<Model>, ModelExtError> {
            match &self.create_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.created_models.clone()),
            }
        }

        async fn update_model_status(
            &self,
            id: ID,
            status: ModelStatus,
        ) -> Result<(), ModelExtError> {
            lock(&self.update_status_calls).push((id.to_string(), status));
            match &self.update_status_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn bulk_archive_models(&self, ids: Vec<ID>) -> Result<(), ModelExtError> {
            lock(&self.archive_calls).push(ids.iter().map(|i| i.to_string()).collect());
            match &self.bulk_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn bulk_disable_models(&self, ids: Vec<ID>) -> Result<(), ModelExtError> {
            lock(&self.disable_calls).push(ids.iter().map(|i| i.to_string()).collect());
            match &self.bulk_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn bulk_enable_models(&self, ids: Vec<ID>) -> Result<(), ModelExtError> {
            lock(&self.enable_calls).push(ids.iter().map(|i| i.to_string()).collect());
            match &self.bulk_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn bulk_delete_models(&self, ids: Vec<ID>) -> Result<(), ModelExtError> {
            lock(&self.delete_calls).push(ids.iter().map(|i| i.to_string()).collect());
            match &self.bulk_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn save_channel_model_prices(
            &self,
            _actor_user_id: Option<i64>,
            channel_id: ID,
            input: Vec<SaveChannelModelPriceInput>,
        ) -> Result<Vec<ChannelModelPrice>, ModelExtError> {
            lock(&self.save_price_calls).push((channel_id.to_string(), input.len()));
            lock(&self.save_price_currencies).push(
                input
                    .iter()
                    .map(|price| price.currency_code.clone())
                    .collect(),
            );
            match &self.save_price_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.save_price_result.clone()),
            }
        }
    }

    // ---------------------------------------------------------------------
    // Test-only reference roots. `#[Object]` cannot be split across modules,
    // so these are the byte-for-byte reference bodies the coordinator pastes
    // into the real `impl QueryRoot` (lib.rs) / `impl MutationRoot`
    // (mutation.rs). They exercise the resolver logic against a fake service.
    // ---------------------------------------------------------------------

    struct TestQueryRoot;

    #[Object]
    impl TestQueryRoot {
        async fn fetch_models(
            &self,
            ctx: &Context<'_>,
            input: FetchModelsInput,
        ) -> Result<FetchModelsPayload, String> {
            let services = model_ext_services(ctx)?;
            services
                .fetch_models(input)
                .await
                .map_err(|err| err.to_string())
        }

        async fn query_models(
            &self,
            ctx: &Context<'_>,
            input: QueryModelsInput,
        ) -> Result<Vec<ModelIdentityWithStatus>, String> {
            let services = model_ext_services(ctx)?;
            services
                .query_models(input)
                .await
                .map_err(|err| err.to_string())
        }

        async fn query_model_channel_connections(
            &self,
            ctx: &Context<'_>,
            associations: Vec<ModelAssociationInput>,
        ) -> Result<Vec<ModelChannelConnection>, String> {
            let services = model_ext_services(ctx)?;
            services
                .query_model_channel_connections(associations)
                .await
                .map_err(|err| err.to_string())
        }

        async fn query_unassociated_channels(
            &self,
            ctx: &Context<'_>,
        ) -> Result<Vec<UnassociatedChannel>, String> {
            let services = model_ext_services(ctx)?;
            services
                .query_unassociated_channels()
                .await
                .map_err(|err| err.to_string())
        }
    }

    struct TestMutationRoot;

    #[Object]
    impl TestMutationRoot {
        async fn bulk_create_models(
            &self,
            ctx: &Context<'_>,
            inputs: Vec<CreateModelInput>,
        ) -> Result<Vec<Model>, String> {
            let services = model_ext_services(ctx)?;
            services
                .bulk_create_models(inputs)
                .await
                .map_err(|err| err.to_string())
        }

        async fn update_model_status(
            &self,
            ctx: &Context<'_>,
            id: ID,
            status: ModelStatus,
        ) -> Result<bool, String> {
            let services = model_ext_services(ctx)?;
            services
                .update_model_status(id, status)
                .await
                .map_err(|err| err.to_string())?;
            Ok(true)
        }

        async fn bulk_archive_models(
            &self,
            ctx: &Context<'_>,
            ids: Vec<ID>,
        ) -> Result<bool, String> {
            let services = model_ext_services(ctx)?;
            services
                .bulk_archive_models(ids)
                .await
                .map_err(|err| err.to_string())?;
            Ok(true)
        }

        async fn bulk_disable_models(
            &self,
            ctx: &Context<'_>,
            ids: Vec<ID>,
        ) -> Result<bool, String> {
            let services = model_ext_services(ctx)?;
            services
                .bulk_disable_models(ids)
                .await
                .map_err(|err| err.to_string())?;
            Ok(true)
        }

        async fn bulk_enable_models(
            &self,
            ctx: &Context<'_>,
            ids: Vec<ID>,
        ) -> Result<bool, String> {
            let services = model_ext_services(ctx)?;
            services
                .bulk_enable_models(ids)
                .await
                .map_err(|err| err.to_string())?;
            Ok(true)
        }

        async fn bulk_delete_models(
            &self,
            ctx: &Context<'_>,
            ids: Vec<ID>,
        ) -> Result<bool, String> {
            let services = model_ext_services(ctx)?;
            services
                .bulk_delete_models(ids)
                .await
                .map_err(|err| err.to_string())?;
            Ok(true)
        }

        async fn save_channel_model_prices(
            &self,
            ctx: &Context<'_>,
            channel_id: ID,
            input: Vec<SaveChannelModelPriceInput>,
        ) -> Result<Vec<ChannelModelPrice>, String> {
            let services = model_ext_services(ctx)?;
            let actor_user_id = ctx
                .data_opt::<crate::me::CurrentUser>()
                .map(|user| user.user_id);
            services
                .save_channel_model_prices(actor_user_id, channel_id, input)
                .await
                .map_err(|err| err.to_string())
        }
    }

    type TestSchema = Schema<TestQueryRoot, TestMutationRoot, EmptySubscription>;

    fn test_schema_builder() -> SchemaBuilder<TestQueryRoot, TestMutationRoot, EmptySubscription> {
        // `Channel implements Node`, so the Relay `Node` interface must be
        // registered explicitly (same as `admin_schema_builder`).
        Schema::build(TestQueryRoot, TestMutationRoot, EmptySubscription)
            .register_output_type::<crate::channel::Node>()
    }

    fn schema_with(services: FakeModelExtServices) -> TestSchema {
        let arc: Arc<dyn ModelExtServices> = Arc::new(services);
        test_schema_builder().data(arc).finish()
    }

    // ---- resolver: fetch_models -------------------------------------

    #[tokio::test]
    async fn fetch_models_returns_payload() {
        let fake = FakeModelExtServices {
            fetch_payload: FetchModelsPayloadStub(Some(FetchModelsPayload {
                models: vec![ModelIdentify {
                    id: "gpt-4o".to_owned(),
                }],
                error: None,
            })),
            ..FakeModelExtServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute(
                r#"{ fetchModels(input: { channelType: "openai", baseURL: "https://api" }) { models { id } error } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("id: \"gpt-4o\""), "missing model id: {s}");
    }

    #[tokio::test]
    async fn fetch_models_surfaces_error() {
        let fake = FakeModelExtServices {
            fetch_error: Some(ModelExtError::FetchModels("boom".to_owned())),
            ..FakeModelExtServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute(r#"{ fetchModels(input: { channelType: "openai", baseURL: "https://api" }) { error } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to fetch models"), "msg: {msg}");
        assert!(msg.contains("boom"), "msg: {msg}");
    }

    // ---- resolver: query_models -------------------------------------

    #[tokio::test]
    async fn query_models_returns_list_with_channel_status() {
        let fake = FakeModelExtServices {
            query_models_result: vec![ModelIdentityWithStatus {
                id: "gpt-4o".to_owned(),
                status: ChannelStatus::Enabled,
            }],
            ..FakeModelExtServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute("{ queryModels(input: { includeAllChannelModels: true }) { id status } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("queryModels")) {
            Some(Value::List(items)) => assert_eq!(items.len(), 1),
            other => panic!("expected list, got {other:?}"),
        }
    }

    // ---- resolver: query_model_channel_connections ------------------

    #[tokio::test]
    async fn query_model_channel_connections_returns_connections() {
        let fake = FakeModelExtServices {
            connections: vec![ModelChannelConnectionStub(ModelChannelConnection {
                channel: sample_channel(1, "c1"),
                models: vec![ChannelModelEntry {
                    request_model: "gpt-4o".to_owned(),
                    actual_model: "gpt-4o-2024".to_owned(),
                    source: "direct".to_owned(),
                }],
                priority: 5,
            })],
            ..FakeModelExtServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute(
                r#"{ queryModelChannelConnections(associations: []) { channel { id name } models { requestModel actualModel source } priority } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("requestModel: \"gpt-4o\""), "missing entry: {s}");
        assert!(s.contains("priority: 5"), "missing priority: {s}");
    }

    // ---- resolver: query_unassociated_channels ----------------------

    #[tokio::test]
    async fn query_unassociated_channels_returns_list() {
        let fake = FakeModelExtServices {
            unassociated: vec![UnassociatedChannelStub(UnassociatedChannel {
                channel: sample_channel(2, "c2"),
                models: vec!["m1".to_owned(), "m2".to_owned()],
            })],
            ..FakeModelExtServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute("{ queryUnassociatedChannels { channel { id } models } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("queryUnassociatedChannels")) {
            Some(Value::List(items)) => assert_eq!(items.len(), 1),
            other => panic!("expected list, got {other:?}"),
        }
    }

    // ---- resolver: bulk_create_models -------------------------------

    #[tokio::test]
    async fn bulk_create_models_returns_created() {
        let fake = FakeModelExtServices {
            created_models: vec![sample_model(1, "gpt-4o")],
            ..FakeModelExtServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute(
                r#"mutation { bulkCreateModels(inputs: [{ developer: "openai", modelID: "gpt-4o", name: "GPT-4o", icon: "", group: "", modelCard: {}, settings: { associations: [] } }]) { id modelID } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("modelID: \"gpt-4o\""), "missing modelID: {s}");
    }

    // ---- resolver: update_model_status ------------------------------

    #[tokio::test]
    async fn update_model_status_returns_true_and_forwards_args() {
        let fake = FakeModelExtServices::default();
        let schema = schema_with(fake.clone());

        let resp = schema
            .execute(
                r#"mutation { updateModelStatus(id: "gid://conduit/Model/3", status: archived) }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("updateModelStatus")) {
            Some(Value::Boolean(true)) => {}
            other => panic!("expected true, got {other:?}"),
        }
        let calls = lock(&fake.update_status_calls).clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "gid://conduit/Model/3");
        assert_eq!(calls[0].1, ModelStatus::Archived);
    }

    #[tokio::test]
    async fn update_model_status_surfaces_error() {
        let fake = FakeModelExtServices {
            update_status_error: Some(ModelExtError::UpdateModelStatus("nope".to_owned())),
            ..FakeModelExtServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute(
                r#"mutation { updateModelStatus(id: "gid://conduit/Model/3", status: enabled) }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to update model status"), "msg: {msg}");
    }

    // ---- resolver: bulk_* -------------------------------------------

    #[tokio::test]
    async fn bulk_archive_models_forwards_ids_and_returns_true() {
        let fake = FakeModelExtServices::default();
        let schema = schema_with(fake.clone());

        let resp = schema
            .execute(r#"mutation { bulkArchiveModels(ids: ["gid://conduit/Model/1", "gid://conduit/Model/2"]) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let calls = lock(&fake.archive_calls).clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), 2);
    }

    #[tokio::test]
    async fn bulk_disable_enable_delete_return_true() {
        let fake = FakeModelExtServices::default();
        let schema = schema_with(fake.clone());

        for (mutation, field) in [
            (
                r#"mutation { bulkDisableModels(ids: ["gid://conduit/Model/1"]) }"#,
                "bulkDisableModels",
            ),
            (
                r#"mutation { bulkEnableModels(ids: ["gid://conduit/Model/1"]) }"#,
                "bulkEnableModels",
            ),
            (
                r#"mutation { bulkDeleteModels(ids: ["gid://conduit/Model/1"]) }"#,
                "bulkDeleteModels",
            ),
        ] {
            let resp = schema.execute(mutation).await;
            assert!(
                resp.errors.is_empty(),
                "errors ({field}): {:?}",
                resp.errors
            );
            let obj = as_object(&resp.data);
            match obj.get(&Name::new(field)) {
                Some(Value::Boolean(true)) => {}
                other => panic!("{field} expected true, got {other:?}"),
            }
        }
        assert_eq!(lock(&fake.disable_calls).len(), 1);
        assert_eq!(lock(&fake.enable_calls).len(), 1);
        assert_eq!(lock(&fake.delete_calls).len(), 1);
    }

    // ---- resolver: save_channel_model_prices ------------------------

    #[tokio::test]
    async fn save_channel_model_prices_forwards_and_returns_prices() {
        let fake = FakeModelExtServices {
            save_price_result: vec![sample_channel_model_price()],
            ..FakeModelExtServices::default()
        };
        let schema = schema_with(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    saveChannelModelPrices(
                        channelId: "gid://conduit/Channel/7",
                        input: [{
                            modelId: "gpt-4o",
                            currencyCode: "CNY",
                            price: { items: [{
                                itemCode: prompt_tokens,
                                pricing: { mode: usage_per_unit, usagePerUnit: "0.0001" }
                            }] }
                        }]
                    ) { id channelID modelID currencyCode referenceID price { items { itemCode pricing { mode usagePerUnit } } } }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("modelID: \"gpt-4o\""), "missing modelID: {s}");
        assert!(s.contains("channelID:"), "missing channelID acronym: {s}");
        assert!(
            s.contains("referenceID:"),
            "missing referenceID acronym: {s}"
        );
        let calls = lock(&fake.save_price_calls).clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "gid://conduit/Channel/7");
        assert_eq!(calls[0].1, 1);
        assert_eq!(
            lock(&fake.save_price_currencies).as_slice(),
            &[vec!["CNY".to_string()]]
        );
    }

    // ---- service-unavailable fallback -------------------------------

    #[tokio::test]
    async fn resolvers_surface_service_unavailable_when_unwired() {
        let schema: TestSchema = test_schema_builder().finish();

        let resp = schema
            .execute("{ queryUnassociatedChannels { models } }")
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("model service is not available"),
            "unexpected msg: {msg}"
        );

        let resp = schema
            .execute(r#"mutation { bulkArchiveModels(ids: ["x"]) }"#)
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("model service is not available"),
            "unexpected msg: {msg}"
        );
    }

    // ---- SDL shape parity -------------------------------------------

    #[test]
    fn sdl_contains_model_ext_types_and_signatures() {
        let arc: Arc<dyn ModelExtServices> = Arc::new(FakeModelExtServices::default());
        let sdl = test_schema_builder().data(arc).finish().sdl();

        for expected in [
            "type ModelIdentify {",
            "type FetchModelsPayload {",
            "type ModelIdentityWithStatus {",
            "type ChannelModelEntry {",
            "type ModelChannelConnection {",
            "type UnassociatedChannel {",
            "type ModelPrice {",
            "type ModelPriceItem {",
            "type Pricing {",
            "type TieredPricing {",
            "type PriceTier {",
            "type PromptWriteCacheVariant {",
            "type ChannelModelPrice {",
            "enum PricingMode",
            "enum PromptWriteCacheVariantCode",
            "input FetchModelsInput {",
            "input QueryModelsInput {",
            "input ModelPriceInput {",
            "input ModelPriceItemInput {",
            "input PricingInput {",
            "input TieredPricingInput {",
            "input PriceTierInput {",
            "input PromptWriteCacheVariantInput {",
            "input SaveChannelModelPriceInput {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}");
        }
        assert!(
            sdl.contains("currencyCode: String!"),
            "SaveChannelModelPriceInput.currencyCode must be required"
        );

        for expected in [
            "fetchModels(input: FetchModelsInput!): FetchModelsPayload!",
            "queryModels(input: QueryModelsInput!): [ModelIdentityWithStatus!]!",
            "queryUnassociatedChannels: [UnassociatedChannel!]!",
            "updateModelStatus(id: ID!, status: ModelStatus!): Boolean!",
            "bulkCreateModels(inputs: [CreateModelInput!]!): [Model!]!",
            "bulkArchiveModels(ids: [ID!]!): Boolean!",
            "bulkDisableModels(ids: [ID!]!): Boolean!",
            "bulkEnableModels(ids: [ID!]!): Boolean!",
            "bulkDeleteModels(ids: [ID!]!): Boolean!",
        ] {
            assert!(sdl.contains(expected), "SDL missing signature {expected}");
        }

        // Acronym field names.
        assert!(sdl.contains("baseURL: String!"), "missing baseURL: {sdl}");
        assert!(sdl.contains("channelID: ID!"), "missing channelID: {sdl}");
        assert!(sdl.contains("modelID: String!"), "missing modelID: {sdl}");
        assert!(
            sdl.contains("currencyCode: String!"),
            "missing currencyCode: {sdl}"
        );
        assert!(
            sdl.contains("referenceID: String!"),
            "missing referenceID: {sdl}"
        );
    }
}
