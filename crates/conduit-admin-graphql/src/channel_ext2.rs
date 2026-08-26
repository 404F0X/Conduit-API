//! GAP-A2 — Channel bulk-create / import / ordering + per-channel API-key
//! management GraphQL slice.
//!
//! Completes the channel-domain mutation surface begun in `crate::channel`
//! (base CRUD) and `crate::channel_ext` (status/duplicate/test/bulk-status/
//! sync). Every GraphQL type/input is copied field-for-field from the captured
//! contract snapshot `tests/contracts/admin_graphql_schema.graphql`, and every
//! resolver mirrors the Go body in
//! `conduit/internal/server/gql/conduit.resolvers.go`.
//!
//! ## Mutations ported (snapshot `type Mutation` lines 788-826)
//!
//!   - `bulkCreateChannels(input: BulkCreateChannelsInput!): [Channel!]!`
//!     (snapshot 791) — Go `BulkCreateChannels` → `ChannelService.BulkCreateChannels`.
//!     NOTE (contract correction vs the task brief): the snapshot input is a
//!     SINGLE flat `BulkCreateChannelsInput` carrying `apiKeys: [String!]!`
//!     (one channel definition fanned out across many keys), NOT
//!     `inputs: [CreateChannelInput!]!`.
//!   - `bulkImportChannels(input: BulkImportChannelsInput!): BulkImportChannelsResult!`
//!     (snapshot 804) — Go `BulkImportChannels` → `ChannelService.BulkImportChannels`.
//!     NOTE: return type is `...Result!` (not `...Payload!`).
//!   - `bulkUpdateChannelOrdering(input: BulkUpdateChannelOrderingInput!): BulkUpdateChannelOrderingResult!`
//!     (snapshot 805) — Go `BulkUpdateChannelOrdering`. NOTE: input wraps
//!     `channels: [ChannelOrderingItem!]!` (item field is `id`, not
//!     `channelID`); return type is `...Result!` (not `Boolean!`).
//!   - `disableChannelAPIKey(channelID: ID!, key: String!): Boolean!`
//!     (snapshot 810) — Go `DisableChannelAPIKey` → `ChannelService.DisableAPIKey`.
//!   - `enableChannelAPIKey(channelID: ID!, key: String!): Boolean!`
//!     (snapshot 811) — Go `EnableChannelAPIKey` → `ChannelService.EnableAPIKey`.
//!   - `enableAllChannelAPIKeys(channelID: ID!): Boolean!`
//!     (snapshot 812) — Go `EnableAllChannelAPIKeys` → `ChannelService.EnableAllAPIKeys`.
//!   - `enableSelectedChannelAPIKeys(channelID: ID!, keys: [String!]!): Boolean!`
//!     (snapshot 813) — Go `EnableSelectedChannelAPIKeys` → `ChannelService.EnableSelectedAPIKeys`.
//!   - `deleteDisabledChannelAPIKeys(channelID: ID!, keys: [String!]!): DeleteDisabledAPIKeysPayload!`
//!     (snapshot 814) — Go `DeleteDisabledChannelAPIKeys` → `ChannelService.DeleteDisabledAPIKeys`.
//!
//! ## Types owned by this slice
//!
//!   - `input BulkCreateChannelsInput` (snapshot) — flat single-channel def +
//!     `apiKeys: [String!]!`.
//!   - `input BulkImportChannelItem` / `input BulkImportChannelsInput`.
//!   - `type BulkImportChannelsResult` — success/created/failed/errors/channels.
//!   - `input ChannelOrderingItem` / `input BulkUpdateChannelOrderingInput`.
//!   - `type BulkUpdateChannelOrderingResult` — success/updated/channels.
//!   - `type DeleteDisabledAPIKeysPayload` — success/message.
//!
//! `ChannelType`, `Channel`, `ChannelSettingsInput`, `ChannelPoliciesInput` are
//! reused from `crate::channel`.
//!
//! ## Service wiring
//!
//! Introduces a NEW host-injected trait [`ChannelBulkMutationServices`] (kept
//! separate from `channel::ChannelMutationServices` /
//! `channel_ext::ChannelExtMutationServices` so their in-memory test doubles
//! stay untouched). The host wires one concrete type implementing all three.
//! Unwired => "channel service is not available" (reusing
//! [`ChannelServiceError::ServiceUnavailable`] so the frontend failure mode is
//! uniform across the channel slices).

use std::sync::Arc;

use async_graphql::{Context, ID, InputObject, SimpleObject};

use crate::channel::{
    Channel, ChannelPoliciesInput, ChannelServiceError, ChannelSettingsInput, ChannelType,
};

// ---------------------------------------------------------------------------
// bulkCreateChannels
// ---------------------------------------------------------------------------

/// `input BulkCreateChannelsInput` (snapshot):
/// ```graphql
/// input BulkCreateChannelsInput {
///   type: ChannelType!
///   name: String!
///   tags: [String!]
///   baseURL: String
///   apiKeys: [String!]!
///   supportedModels: [String!]!
///   autoSyncSupportedModels: Boolean
///   defaultTestModel: String!
///   settings: ChannelSettingsInput
///   policies: ChannelPoliciesInput
///   orderingWeight: Int
///   remark: String
/// }
/// ```
/// A single channel definition fanned out across `apiKeys` (Go
/// `biz.BulkCreateChannelsInput`, one channel row created per key).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "BulkCreateChannelsInput")]
pub struct BulkCreateChannelsInput {
    #[graphql(name = "type")]
    pub channel_type: ChannelType,
    pub name: String,
    pub tags: Option<Vec<String>>,
    #[graphql(name = "baseURL")]
    pub base_url: Option<String>,
    #[graphql(name = "apiKeys")]
    pub api_keys: Vec<String>,
    pub supported_models: Vec<String>,
    pub auto_sync_supported_models: Option<bool>,
    pub default_test_model: String,
    pub settings: Option<ChannelSettingsInput>,
    pub policies: Option<ChannelPoliciesInput>,
    pub ordering_weight: Option<i64>,
    pub remark: Option<String>,
}

// ---------------------------------------------------------------------------
// bulkImportChannels
// ---------------------------------------------------------------------------

/// `input BulkImportChannelItem` (snapshot):
/// ```graphql
/// input BulkImportChannelItem {
///   type: String!
///   name: String!
///   baseURL: String
///   apiKey: String
///   supportedModels: [String!]!
///   defaultTestModel: String!
/// }
/// ```
/// Note `type` is a plain `String!` here (not the `ChannelType` enum), matching
/// the import payload shape the frontend uploads.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "BulkImportChannelItem")]
pub struct BulkImportChannelItem {
    #[graphql(name = "type")]
    pub channel_type: String,
    pub name: String,
    #[graphql(name = "baseURL")]
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub supported_models: Vec<String>,
    pub default_test_model: String,
}

/// `input BulkImportChannelsInput { channels: [BulkImportChannelItem!]! }`.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "BulkImportChannelsInput")]
pub struct BulkImportChannelsInput {
    pub channels: Vec<BulkImportChannelItem>,
}

/// `type BulkImportChannelsResult` (snapshot):
/// ```graphql
/// type BulkImportChannelsResult {
///   success: Boolean!
///   created: Int!
///   failed: Int!
///   errors: [String!]
///   channels: [Channel!]!
/// }
/// ```
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "BulkImportChannelsResult")]
pub struct BulkImportChannelsResult {
    pub success: bool,
    pub created: i32,
    pub failed: i32,
    pub errors: Option<Vec<String>>,
    pub channels: Vec<Channel>,
}

// ---------------------------------------------------------------------------
// bulkUpdateChannelOrdering
// ---------------------------------------------------------------------------

/// `input ChannelOrderingItem { id: ID! orderingWeight: Int! }` (snapshot).
/// Note the field is `id` (not `channelID`).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "ChannelOrderingItem")]
pub struct ChannelOrderingItem {
    pub id: ID,
    pub ordering_weight: i64,
}

/// `input BulkUpdateChannelOrderingInput { channels: [ChannelOrderingItem!]! }`.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "BulkUpdateChannelOrderingInput")]
pub struct BulkUpdateChannelOrderingInput {
    pub channels: Vec<ChannelOrderingItem>,
}

/// `type BulkUpdateChannelOrderingResult` (snapshot):
/// ```graphql
/// type BulkUpdateChannelOrderingResult {
///   success: Boolean!
///   updated: Int!
///   channels: [Channel!]!
/// }
/// ```
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "BulkUpdateChannelOrderingResult")]
pub struct BulkUpdateChannelOrderingResult {
    pub success: bool,
    pub updated: i32,
    pub channels: Vec<Channel>,
}

// ---------------------------------------------------------------------------
// deleteDisabledChannelAPIKeys
// ---------------------------------------------------------------------------

/// `type DeleteDisabledAPIKeysPayload { success: Boolean! message: String }`
/// (snapshot). The `APIKeys` acronym is pinned via the explicit type name.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "DeleteDisabledAPIKeysPayload")]
pub struct DeleteDisabledApiKeysPayload {
    pub success: bool,
    pub message: Option<String>,
}

// ---------------------------------------------------------------------------
// Service trait (host-injected)
// ---------------------------------------------------------------------------

/// Backs the bulk-create / import / ordering + per-channel API-key management
/// mutations. The Go counterpart is `biz.ChannelService` fx-injected into the
/// resolver root (`resolver.go`); this trait captures exactly the methods those
/// resolver bodies call. Errors reuse [`ChannelServiceError`] so the unwired
/// failure message ("channel service is not available") matches the other
/// channel slices.
#[async_trait::async_trait]
pub trait ChannelBulkMutationServices: Send + Sync {
    /// Mirrors Go `BulkCreateChannels` → `ChannelService.BulkCreateChannels`
    /// (conduit.resolvers.go:166): create one channel row per entry in
    /// `input.api_keys`, return every created channel.
    async fn bulk_create_channels(
        &self,
        input: BulkCreateChannelsInput,
    ) -> Result<Vec<Channel>, ChannelServiceError>;

    /// Mirrors Go `BulkImportChannels` → `ChannelService.BulkImportChannels`
    /// (conduit.resolvers.go:319): best-effort import; the result carries the
    /// success flag, created/failed counts, per-item errors, and the created
    /// channels.
    async fn bulk_import_channels(
        &self,
        channels: Vec<BulkImportChannelItem>,
    ) -> Result<BulkImportChannelsResult, ChannelServiceError>;

    /// Mirrors Go `BulkUpdateChannelOrdering` →
    /// `ChannelService.BulkUpdateChannelOrdering` (conduit.resolvers.go:335):
    /// persist the new `ordering_weight` for each item, return the updated
    /// channels (the resolver derives `updated = len(channels)` +
    /// `success = true`).
    async fn bulk_update_channel_ordering(
        &self,
        items: Vec<ChannelOrderingItem>,
    ) -> Result<Vec<Channel>, ChannelServiceError>;

    /// Mirrors Go `DisableChannelAPIKey` → `ChannelService.DisableAPIKey`
    /// (conduit.resolvers.go:349): the resolver passes the fixed
    /// `(0, "Manually disabled by user")` disable-reason pair; the host applies
    /// the same defaults.
    async fn disable_channel_api_key(
        &self,
        channel_id: &str,
        key: &str,
    ) -> Result<(), ChannelServiceError>;

    /// Mirrors Go `EnableChannelAPIKey` → `ChannelService.EnableAPIKey`
    /// (conduit.resolvers.go:358).
    async fn enable_channel_api_key(
        &self,
        channel_id: &str,
        key: &str,
    ) -> Result<(), ChannelServiceError>;

    /// Mirrors Go `EnableAllChannelAPIKeys` → `ChannelService.EnableAllAPIKeys`
    /// (conduit.resolvers.go:367).
    async fn enable_all_channel_api_keys(
        &self,
        channel_id: &str,
    ) -> Result<(), ChannelServiceError>;

    /// Mirrors Go `EnableSelectedChannelAPIKeys` →
    /// `ChannelService.EnableSelectedAPIKeys` (conduit.resolvers.go:376).
    async fn enable_selected_channel_api_keys(
        &self,
        channel_id: &str,
        keys: Vec<String>,
    ) -> Result<(), ChannelServiceError>;

    /// Mirrors Go `DeleteDisabledChannelAPIKeys` →
    /// `ChannelService.DeleteDisabledAPIKeys` (conduit.resolvers.go:385):
    /// remove the listed disabled keys, return the success/message payload.
    async fn delete_disabled_channel_api_keys(
        &self,
        channel_id: &str,
        keys: Vec<String>,
    ) -> Result<DeleteDisabledApiKeysPayload, ChannelServiceError>;
}

/// Resolves the injected [`ChannelBulkMutationServices`] from the async-graphql
/// data bag; absent wiring surfaces the shared "channel service is not
/// available" failure mode.
pub(crate) fn channel_bulk_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ChannelBulkMutationServices>, String> {
    match ctx.data::<Arc<dyn ChannelBulkMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ChannelServiceError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Resolver wiring (for the coordinator).
//
// async-graphql's `#[Object]` macro forbids splitting a root's impl across
// modules (E0119), so this slice contributes NO `#[Object]` block; the
// resolver bodies below live in the single `#[Object] impl MutationRoot`
// (mutation.rs). The `TestMutationRoot` in the test module is a byte-for-byte
// reference implementation.
//
// ```ignore
// async fn bulk_create_channels(&self, ctx, input: BulkCreateChannelsInput)
//     -> Result<Vec<Channel>, String>
// async fn bulk_import_channels(&self, ctx, input: BulkImportChannelsInput)
//     -> Result<BulkImportChannelsResult, String>   // forwards input.channels
// async fn bulk_update_channel_ordering(&self, ctx, input: BulkUpdateChannelOrderingInput)
//     -> Result<BulkUpdateChannelOrderingResult, String>  // success=true, updated=len
// async fn disable_channel_api_key(&self, ctx, channelID: ID, key: String)   -> Result<bool, String>
// async fn enable_channel_api_key(&self, ctx, channelID: ID, key: String)    -> Result<bool, String>
// async fn enable_all_channel_api_keys(&self, ctx, channelID: ID)            -> Result<bool, String>
// async fn enable_selected_channel_api_keys(&self, ctx, channelID: ID, keys: Vec<String>) -> Result<bool, String>
// async fn delete_disabled_channel_api_keys(&self, ctx, channelID: ID, keys: Vec<String>)
//     -> Result<DeleteDisabledApiKeysPayload, String>
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
    use crate::channel::ChannelStatus;

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

    fn epoch() -> crate::scalars::TimeScalar {
        crate::scalars::TimeScalar(chrono::DateTime::<chrono::Utc>::default())
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
            status: ChannelStatus::Disabled,
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

    // ---------------------------------------------------------------------
    // In-memory fake service.
    // ---------------------------------------------------------------------

    /// Records a `(channel_id, key)` pair per API-key toggle call.
    type IdKeyCalls = Arc<Mutex<Vec<(String, String)>>>;
    /// Records a `(channel_id, keys)` pair per multi-key call.
    type IdKeysCalls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

    #[derive(Default)]
    struct FakeChannelBulkService {
        /// Force every method to fail with this error (service-error path).
        fail: Option<ChannelServiceError>,
        created: Arc<Mutex<Vec<Channel>>>,
        // Records for call-forwarding assertions.
        disable_calls: IdKeyCalls,
        enable_calls: IdKeyCalls,
        enable_all_calls: Arc<Mutex<Vec<String>>>,
        enable_selected_calls: IdKeysCalls,
        delete_disabled_calls: IdKeysCalls,
        ordering_calls: Arc<Mutex<Vec<ChannelOrderingItem>>>,
    }

    #[async_trait::async_trait]
    impl ChannelBulkMutationServices for FakeChannelBulkService {
        async fn bulk_create_channels(
            &self,
            input: BulkCreateChannelsInput,
        ) -> Result<Vec<Channel>, ChannelServiceError> {
            if let Some(err) = &self.fail {
                return Err(err.clone());
            }
            // One channel per api key (Go fan-out semantics).
            let mut out = Vec::new();
            for (i, _key) in input.api_keys.iter().enumerate() {
                let ch = sample_channel(i as i64 + 1, &input.name);
                out.push(ch);
            }
            *lock(&self.created) = out.clone();
            Ok(out)
        }

        async fn bulk_import_channels(
            &self,
            channels: Vec<BulkImportChannelItem>,
        ) -> Result<BulkImportChannelsResult, ChannelServiceError> {
            if let Some(err) = &self.fail {
                return Err(err.clone());
            }
            let created_channels: Vec<Channel> = channels
                .iter()
                .enumerate()
                .map(|(i, item)| sample_channel(i as i64 + 1, &item.name))
                .collect();
            Ok(BulkImportChannelsResult {
                success: true,
                created: created_channels.len() as i32,
                failed: 0,
                errors: None,
                channels: created_channels,
            })
        }

        async fn bulk_update_channel_ordering(
            &self,
            items: Vec<ChannelOrderingItem>,
        ) -> Result<Vec<Channel>, ChannelServiceError> {
            if let Some(err) = &self.fail {
                return Err(err.clone());
            }
            lock(&self.ordering_calls).extend(items.iter().cloned());
            let updated: Vec<Channel> = items
                .iter()
                .map(|item| {
                    let mut ch =
                        sample_channel(item.id.as_str().parse::<i64>().unwrap_or(0), "reordered");
                    ch.ordering_weight = item.ordering_weight;
                    ch
                })
                .collect();
            Ok(updated)
        }

        async fn disable_channel_api_key(
            &self,
            channel_id: &str,
            key: &str,
        ) -> Result<(), ChannelServiceError> {
            if let Some(err) = &self.fail {
                return Err(err.clone());
            }
            lock(&self.disable_calls).push((channel_id.to_owned(), key.to_owned()));
            Ok(())
        }

        async fn enable_channel_api_key(
            &self,
            channel_id: &str,
            key: &str,
        ) -> Result<(), ChannelServiceError> {
            if let Some(err) = &self.fail {
                return Err(err.clone());
            }
            lock(&self.enable_calls).push((channel_id.to_owned(), key.to_owned()));
            Ok(())
        }

        async fn enable_all_channel_api_keys(
            &self,
            channel_id: &str,
        ) -> Result<(), ChannelServiceError> {
            if let Some(err) = &self.fail {
                return Err(err.clone());
            }
            lock(&self.enable_all_calls).push(channel_id.to_owned());
            Ok(())
        }

        async fn enable_selected_channel_api_keys(
            &self,
            channel_id: &str,
            keys: Vec<String>,
        ) -> Result<(), ChannelServiceError> {
            if let Some(err) = &self.fail {
                return Err(err.clone());
            }
            lock(&self.enable_selected_calls).push((channel_id.to_owned(), keys));
            Ok(())
        }

        async fn delete_disabled_channel_api_keys(
            &self,
            channel_id: &str,
            keys: Vec<String>,
        ) -> Result<DeleteDisabledApiKeysPayload, ChannelServiceError> {
            if let Some(err) = &self.fail {
                return Err(err.clone());
            }
            let count = keys.len();
            lock(&self.delete_disabled_calls).push((channel_id.to_owned(), keys));
            Ok(DeleteDisabledApiKeysPayload {
                success: true,
                message: Some(format!("deleted {count} disabled keys")),
            })
        }
    }

    // ---------------------------------------------------------------------
    // Reference resolver root — byte-for-byte what the coordinator pasted into
    // the real `#[Object] impl MutationRoot` (mutation.rs). `#[Object]` cannot
    // be split across modules, so this test-only root exercises the resolvers
    // against the fake service.
    // ---------------------------------------------------------------------

    struct TestMutationRoot;

    #[Object]
    impl TestMutationRoot {
        async fn bulk_create_channels(
            &self,
            ctx: &Context<'_>,
            input: BulkCreateChannelsInput,
        ) -> Result<Vec<Channel>, String> {
            let mut input = input;
            crate::channel::validate_and_normalize_channel_settings_input(input.settings.as_mut())
                .map_err(|error| error.to_string())?;
            let services = channel_bulk_mutation_services(ctx)?;
            services
                .bulk_create_channels(input)
                .await
                .map_err(|err| err.to_string())
        }

        async fn bulk_import_channels(
            &self,
            ctx: &Context<'_>,
            input: BulkImportChannelsInput,
        ) -> Result<BulkImportChannelsResult, String> {
            let services = channel_bulk_mutation_services(ctx)?;
            services
                .bulk_import_channels(input.channels)
                .await
                .map_err(|err| err.to_string())
        }

        async fn bulk_update_channel_ordering(
            &self,
            ctx: &Context<'_>,
            input: BulkUpdateChannelOrderingInput,
        ) -> Result<BulkUpdateChannelOrderingResult, String> {
            let services = channel_bulk_mutation_services(ctx)?;
            let channels = services
                .bulk_update_channel_ordering(input.channels)
                .await
                .map_err(|err| err.to_string())?;
            Ok(BulkUpdateChannelOrderingResult {
                success: true,
                updated: channels.len() as i32,
                channels,
            })
        }

        #[graphql(name = "disableChannelAPIKey")]
        async fn disable_channel_api_key(
            &self,
            ctx: &Context<'_>,
            #[graphql(name = "channelID")] channel_id: ID,
            key: String,
        ) -> Result<bool, String> {
            let services = channel_bulk_mutation_services(ctx)?;
            services
                .disable_channel_api_key(channel_id.as_str(), key.as_str())
                .await
                .map_err(|err| err.to_string())?;
            Ok(true)
        }

        #[graphql(name = "enableChannelAPIKey")]
        async fn enable_channel_api_key(
            &self,
            ctx: &Context<'_>,
            #[graphql(name = "channelID")] channel_id: ID,
            key: String,
        ) -> Result<bool, String> {
            let services = channel_bulk_mutation_services(ctx)?;
            services
                .enable_channel_api_key(channel_id.as_str(), key.as_str())
                .await
                .map_err(|err| err.to_string())?;
            Ok(true)
        }

        #[graphql(name = "enableAllChannelAPIKeys")]
        async fn enable_all_channel_api_keys(
            &self,
            ctx: &Context<'_>,
            #[graphql(name = "channelID")] channel_id: ID,
        ) -> Result<bool, String> {
            let services = channel_bulk_mutation_services(ctx)?;
            services
                .enable_all_channel_api_keys(channel_id.as_str())
                .await
                .map_err(|err| err.to_string())?;
            Ok(true)
        }

        #[graphql(name = "enableSelectedChannelAPIKeys")]
        async fn enable_selected_channel_api_keys(
            &self,
            ctx: &Context<'_>,
            #[graphql(name = "channelID")] channel_id: ID,
            keys: Vec<String>,
        ) -> Result<bool, String> {
            let services = channel_bulk_mutation_services(ctx)?;
            services
                .enable_selected_channel_api_keys(channel_id.as_str(), keys)
                .await
                .map_err(|err| err.to_string())?;
            Ok(true)
        }

        #[graphql(name = "deleteDisabledChannelAPIKeys")]
        async fn delete_disabled_channel_api_keys(
            &self,
            ctx: &Context<'_>,
            #[graphql(name = "channelID")] channel_id: ID,
            keys: Vec<String>,
        ) -> Result<DeleteDisabledApiKeysPayload, String> {
            let services = channel_bulk_mutation_services(ctx)?;
            services
                .delete_disabled_channel_api_keys(channel_id.as_str(), keys)
                .await
                .map_err(|err| err.to_string())
        }
    }

    struct TestQueryRoot;

    #[Object]
    impl TestQueryRoot {
        /// Placeholder — a schema needs a Query root.
        async fn probe(&self) -> bool {
            true
        }
    }

    type TestSchema = Schema<TestQueryRoot, TestMutationRoot, EmptySubscription>;

    fn test_schema_builder() -> SchemaBuilder<TestQueryRoot, TestMutationRoot, EmptySubscription> {
        Schema::build(TestQueryRoot, TestMutationRoot, EmptySubscription)
            .register_output_type::<crate::channel::Node>()
    }

    fn schema_with(service: FakeChannelBulkService) -> TestSchema {
        let arc: Arc<dyn ChannelBulkMutationServices> = Arc::new(service);
        test_schema_builder().data(arc).finish()
    }

    // ---- SDL parity --------------------------------------------------

    #[test]
    fn sdl_input_types_match_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let sdl = test_schema_builder().finish().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        for header in [
            "input BulkCreateChannelsInput",
            "input BulkImportChannelItem",
            "input BulkImportChannelsInput",
            "input ChannelOrderingItem",
            "input BulkUpdateChannelOrderingInput",
        ] {
            crate::sdl_parity::assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_output_types_match_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let sdl = test_schema_builder().finish().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;
        for header in [
            "type BulkImportChannelsResult",
            "type BulkUpdateChannelOrderingResult",
            "type DeleteDisabledAPIKeysPayload",
        ] {
            crate::sdl_parity::assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_mutation_signatures_present() {
        let sdl = test_schema_builder().finish().sdl();
        for expected in [
            "bulkCreateChannels(input: BulkCreateChannelsInput!): [Channel!]!",
            "bulkImportChannels(input: BulkImportChannelsInput!): BulkImportChannelsResult!",
            "disableChannelAPIKey(channelID: ID!, key: String!): Boolean!",
            "enableChannelAPIKey(channelID: ID!, key: String!): Boolean!",
            "enableAllChannelAPIKeys(channelID: ID!): Boolean!",
            "enableSelectedChannelAPIKeys(channelID: ID!, keys: [String!]!): Boolean!",
        ] {
            assert!(sdl.contains(expected), "SDL missing `{expected}`:\n{sdl}");
        }
        // The two multi-line-arg signatures render collapsed; check the field
        // name + return type independently.
        assert!(
            sdl.contains("bulkUpdateChannelOrdering(")
                && sdl.contains("BulkUpdateChannelOrderingResult!"),
            "SDL missing bulkUpdateChannelOrdering:\n{sdl}"
        );
        assert!(
            sdl.contains("deleteDisabledChannelAPIKeys(")
                && sdl.contains("DeleteDisabledAPIKeysPayload!"),
            "SDL missing deleteDisabledChannelAPIKeys:\n{sdl}"
        );
    }

    // ---- resolver happy paths ----------------------------------------

    #[tokio::test]
    async fn bulk_create_channels_fans_out_one_per_key() {
        let schema = schema_with(FakeChannelBulkService::default());
        let resp = schema
            .execute(
                r#"mutation { bulkCreateChannels(input: {
                    type: openai, name: "batch", apiKeys: ["k1","k2","k3"],
                    supportedModels: ["gpt-4o"], defaultTestModel: "gpt-4o"
                }) { id name } }"#,
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("bulkCreateChannels")) {
            Some(Value::List(items)) => assert_eq!(items.len(), 3),
            other => panic!("expected 3-item list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bulk_create_channels_rejects_incomplete_billing_metadata() {
        let service = FakeChannelBulkService::default();
        let created = Arc::clone(&service.created);
        let schema = schema_with(service);
        let resp = schema
            .execute(
                r#"mutation { bulkCreateChannels(input: {
                    type: openai, name: "batch", apiKeys: ["k1"],
                    supportedModels: ["gpt-4o"], defaultTestModel: "gpt-4o",
                    settings: { rechargeMultiplier: "1" }
                }) { id } }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        assert!(
            resp.errors[0]
                .message
                .contains("billing_currency and recharge_multiplier must be provided together"),
            "unexpected error: {}",
            resp.errors[0].message
        );
        assert!(lock(&created).is_empty());
    }

    #[tokio::test]
    async fn bulk_import_channels_returns_result_aggregate() {
        let schema = schema_with(FakeChannelBulkService::default());
        let resp = schema
            .execute(
                r#"mutation { bulkImportChannels(input: { channels: [
                    { type: "openai", name: "c1", supportedModels: ["gpt-4o"], defaultTestModel: "gpt-4o" },
                    { type: "openai", name: "c2", supportedModels: ["gpt-4o"], defaultTestModel: "gpt-4o" }
                ] }) { success created failed channels { id } } }"#,
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("created: 2"), "created count missing: {s}");
        assert!(s.contains("success: true"), "success missing: {s}");
    }

    #[tokio::test]
    async fn bulk_update_channel_ordering_returns_updated_count() {
        let service = FakeChannelBulkService::default();
        let ordering_calls = Arc::clone(&service.ordering_calls);
        let schema = schema_with(service);
        let resp = schema
            .execute(
                r#"mutation { bulkUpdateChannelOrdering(input: { channels: [
                    { id: "1", orderingWeight: 10 },
                    { id: "2", orderingWeight: 20 }
                ] }) { success updated channels { id } } }"#,
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("updated: 2"), "updated count missing: {s}");
        // The item field is `id` (contract), forwarded verbatim.
        let calls = lock(&ordering_calls);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id.as_str(), "1");
        assert_eq!(calls[0].ordering_weight, 10);
    }

    #[tokio::test]
    async fn api_key_toggles_forward_channel_id_and_key() {
        let service = FakeChannelBulkService::default();
        let disable_calls = Arc::clone(&service.disable_calls);
        let enable_calls = Arc::clone(&service.enable_calls);
        let enable_all_calls = Arc::clone(&service.enable_all_calls);
        let enable_selected_calls = Arc::clone(&service.enable_selected_calls);
        let schema = schema_with(service);

        let resp = schema
            .execute(
                r#"mutation {
                    disableChannelAPIKey(channelID: "7", key: "sk-a")
                    enableChannelAPIKey(channelID: "7", key: "sk-b")
                    enableAllChannelAPIKeys(channelID: "7")
                    enableSelectedChannelAPIKeys(channelID: "7", keys: ["sk-c","sk-d"])
                }"#,
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);

        assert_eq!(
            lock(&disable_calls).as_slice(),
            &[("7".to_owned(), "sk-a".to_owned())]
        );
        assert_eq!(
            lock(&enable_calls).as_slice(),
            &[("7".to_owned(), "sk-b".to_owned())]
        );
        assert_eq!(lock(&enable_all_calls).as_slice(), &["7".to_owned()]);
        let selected = lock(&enable_selected_calls);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0, "7");
        assert_eq!(selected[0].1, vec!["sk-c".to_owned(), "sk-d".to_owned()]);
    }

    #[tokio::test]
    async fn delete_disabled_channel_api_keys_returns_payload() {
        let service = FakeChannelBulkService::default();
        let delete_calls = Arc::clone(&service.delete_disabled_calls);
        let schema = schema_with(service);
        let resp = schema
            .execute(
                r#"mutation { deleteDisabledChannelAPIKeys(channelID: "3", keys: ["sk-x","sk-y"]) { success message } }"#,
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("success: true"), "success missing: {s}");
        let calls = lock(&delete_calls);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "3");
        assert_eq!(calls[0].1, vec!["sk-x".to_owned(), "sk-y".to_owned()]);
    }

    // ---- service-error path ------------------------------------------

    #[tokio::test]
    async fn bulk_create_surfaces_service_error() {
        let schema = schema_with(FakeChannelBulkService {
            fail: Some(ChannelServiceError::Create("boom".to_owned())),
            ..FakeChannelBulkService::default()
        });
        let resp = schema
            .execute(
                r#"mutation { bulkCreateChannels(input: {
                    type: openai, name: "batch", apiKeys: ["k1"],
                    supportedModels: ["gpt-4o"], defaultTestModel: "gpt-4o"
                }) { id } }"#,
            )
            .await;
        assert_eq!(resp.errors.len(), 1);
        assert!(
            format!("{}", resp.errors[0]).contains("failed to create channel"),
            "msg: {}",
            resp.errors[0]
        );
    }

    // ---- unwired fallback --------------------------------------------

    #[tokio::test]
    async fn resolvers_surface_service_unavailable_when_unwired() {
        let schema: TestSchema = test_schema_builder().finish();
        let resp = schema
            .execute(r#"mutation { enableAllChannelAPIKeys(channelID: "1") }"#)
            .await;
        assert_eq!(resp.errors.len(), 1);
        assert!(
            format!("{}", resp.errors[0]).contains("channel service is not available"),
            "msg: {}",
            resp.errors[0]
        );
    }
}
