//! GAP-A — Channel extended-mutation GraphQL slice.
//!
//! Ports the channel mutations the frontend `channels` page calls that are not
//! part of the base CRUD trio (`createChannel`/`updateChannel`/`deleteChannel`,
//! already in `crate::channel` + `crate::mutation`). Every GraphQL type/input
//! is copied field-for-field from the captured contract snapshot
//! `tests/contracts/admin_graphql_schema.graphql`, and every resolver mirrors
//! the Go body in `conduit/internal/server/gql/conduit.resolvers.go`.
//!
//! ## Mutations ported (snapshot `type Mutation` lines 788-868)
//!
//!   - `updateChannelStatus(id: ID!, status: ChannelStatus!): Channel!`
//!     (snapshot 794) — Go `UpdateChannelStatus` → `ChannelService.UpdateChannelStatus`.
//!   - `duplicateChannel(sourceID: ID!, input: CreateChannelInput!): Channel!`
//!     (snapshot 790) — Go `DuplicateChannel` → `ChannelService.DuplicateChannel`.
//!   - `saveChannelEndpoints(input: SaveChannelEndpointsInput!): Channel!`
//!     (snapshot 793) — Go `SaveChannelEndpoints` → `ChannelService.SaveChannelEndpoints`.
//!   - `testChannel(input: TestChannelInput!): TestChannelPayload!`
//!     (snapshot 801) — Go `TestChannel` → `TestChannelOrchestrator.TestChannel`.
//!   - `testChannelAPIKey(channelID: ID!, key: String!, modelID: String): TestAPIKeyResult!`
//!     (snapshot 803) — Go `TestChannelAPIKey` → `TestChannelOrchestrator.TestSingleAPIKey`.
//!   - `testChannelAPIKeys(channelID: ID!, modelID: String): TestChannelAPIKeysPayload!`
//!     (snapshot 802) — Go `TestChannelAPIKeys` → `TestChannelOrchestrator.TestChannelAPIKeys`.
//!   - `bulkArchiveChannels(ids: [ID!]!): Boolean!` (snapshot 796) — Go
//!     `BulkArchiveChannels`.
//!   - `bulkDisableChannels(ids: [ID!]!): Boolean!` (snapshot 797).
//!   - `bulkEnableChannels(ids: [ID!]!): Boolean!` (snapshot 798).
//!   - `bulkRecoverChannels(ids: [ID!]!): Boolean!` (snapshot 799).
//!   - `bulkDeleteChannels(ids: [ID!]!): Boolean!` (snapshot 800).
//!   - `syncChannelModels(channelID: ID!, pattern: String): SyncChannelModelsPayload!`
//!     (snapshot 868) — Go `SyncChannelModels` → `ChannelService.SyncChannelModels`.
//!
//! ## Types owned by this slice
//!
//!   - `type TestChannelPayload` (snapshot 258-block) — latency/success/message/error.
//!   - `type TestAPIKeyResult` — keyPrefix/success/latency/error/disabled.
//!   - `type TestChannelAPIKeysPayload` — channelID/total/successCount/failedCount/results.
//!   - `type SyncChannelModelsPayload` — channelID/supportedModels.
//!   - `input TestChannelInput` — channelID/modelID/proxy.
//!   - `input SaveChannelEndpointsInput` — channelID/endpoints.
//!
//! `ChannelEndpointInput` and `ProxyConfigInput` already exist in
//! `crate::channel` and are reused verbatim.
//!
//! ## Service wiring
//!
//! This slice introduces a NEW host-injected trait
//! [`ChannelExtMutationServices`] rather than extending the existing
//! `channel::ChannelMutationServices` (which would break that module's
//! in-memory test double). The host wires a single concrete type that
//! implements both traits. Unwired => "channel service is not available"
//! (same message the base channel slice uses, so the frontend failure mode is
//! uniform).

use std::sync::Arc;

use async_graphql::{Context, ID, InputObject, SimpleObject};

use crate::channel::{Channel, ChannelEndpointInput, ChannelServiceError, ProxyConfigInput};

// ---------------------------------------------------------------------------
// Output object types
// ---------------------------------------------------------------------------

/// `type TestChannelPayload` (snapshot):
/// ```graphql
/// type TestChannelPayload {
///   latency: Float!
///   success: Boolean!
///   message: String
///   error: String
/// }
/// ```
/// Mirrors Go `TestChannelPayload` (models_gen.go) built from the
/// orchestrator's `TestChannel` result (conduit.resolvers.go).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "TestChannelPayload")]
pub struct TestChannelPayload {
    pub latency: f64,
    pub success: bool,
    pub message: Option<String>,
    pub error: Option<String>,
}

/// `type TestAPIKeyResult` (snapshot):
/// ```graphql
/// type TestAPIKeyResult {
///   keyPrefix: String!
///   success: Boolean!
///   latency: Float!
///   error: String
///   disabled: Boolean!
/// }
/// ```
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "TestAPIKeyResult")]
pub struct TestApiKeyResult {
    pub key_prefix: String,
    pub success: bool,
    pub latency: f64,
    pub error: Option<String>,
    pub disabled: bool,
}

/// `type TestChannelAPIKeysPayload` (snapshot):
/// ```graphql
/// type TestChannelAPIKeysPayload {
///   channelID: ID!
///   total: Int!
///   successCount: Int!
///   failedCount: Int!
///   results: [TestAPIKeyResult!]!
/// }
/// ```
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "TestChannelAPIKeysPayload")]
pub struct TestChannelApiKeysPayload {
    /// All-caps acronym tag: default camelCase would emit `channelId`.
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub total: i32,
    pub success_count: i32,
    pub failed_count: i32,
    pub results: Vec<TestApiKeyResult>,
}

/// `type SyncChannelModelsPayload` (snapshot 66-block):
/// ```graphql
/// type SyncChannelModelsPayload {
///   channelID: ID!
///   supportedModels: [String!]!
/// }
/// ```
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "SyncChannelModelsPayload")]
pub struct SyncChannelModelsPayload {
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub supported_models: Vec<String>,
}

// ---------------------------------------------------------------------------
// Input object types
// ---------------------------------------------------------------------------

/// `input TestChannelInput` (snapshot):
/// ```graphql
/// input TestChannelInput {
///   channelID: ID!
///   modelID: String
///   proxy: ProxyConfigInput
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "TestChannelInput")]
pub struct TestChannelInput {
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    /// All-caps acronym tag `modelID` — pinned so it does not render `modelId`.
    #[graphql(name = "modelID")]
    pub model_id: Option<String>,
    pub proxy: Option<ProxyConfigInput>,
}

/// `input SaveChannelEndpointsInput` (snapshot 104-block):
/// ```graphql
/// input SaveChannelEndpointsInput {
///   channelID: ID!
///   endpoints: [ChannelEndpointInput!]!
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "SaveChannelEndpointsInput")]
pub struct SaveChannelEndpointsInput {
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub endpoints: Vec<ChannelEndpointInput>,
}

// ---------------------------------------------------------------------------
// Service trait (host-injected)
// ---------------------------------------------------------------------------

/// Backs the channel extended-mutation slice. The host wires ONE concrete type
/// that also implements `channel::ChannelMutationServices`; the Go counterpart
/// is `biz.ChannelService` + the `TestChannelOrchestrator` fx-injected into the
/// resolver root (`resolver.go`). Errors reuse [`ChannelServiceError`] so the
/// unwired failure message ("channel service is not available") matches the
/// base channel slice.
#[async_trait::async_trait]
pub trait ChannelExtMutationServices: Send + Sync {
    /// Mirrors Go `UpdateChannelStatus` → `ChannelService.UpdateChannelStatus`
    /// (conduit.resolvers.go). `id` is the raw `ID!` scalar; the host decodes
    /// the `gid://conduit/Channel/<id>` wire form and passes the int id.
    async fn update_channel_status(
        &self,
        id: &str,
        status: crate::channel::ChannelStatus,
    ) -> Result<Channel, ChannelServiceError>;

    /// Mirrors Go `DuplicateChannel` → `ChannelService.DuplicateChannel`:
    /// clone the source channel, applying the provided `CreateChannelInput`
    /// overrides.
    async fn duplicate_channel(
        &self,
        actor_user_id: Option<i64>,
        source_id: &str,
        input: crate::channel::CreateChannelInput,
    ) -> Result<Channel, ChannelServiceError>;

    /// Mirrors Go `SaveChannelEndpoints` → `ChannelService.SaveChannelEndpoints`.
    async fn save_channel_endpoints(
        &self,
        input: SaveChannelEndpointsInput,
    ) -> Result<Channel, ChannelServiceError>;

    /// Mirrors Go `TestChannel` → `TestChannelOrchestrator.TestChannel`.
    async fn test_channel(
        &self,
        input: TestChannelInput,
    ) -> Result<TestChannelPayload, ChannelServiceError>;

    /// Mirrors Go `TestChannelAPIKey` → `TestChannelOrchestrator.TestSingleAPIKey`.
    async fn test_channel_api_key(
        &self,
        channel_id: &str,
        key: &str,
        model_id: Option<String>,
    ) -> Result<TestApiKeyResult, ChannelServiceError>;

    /// Mirrors Go `TestChannelAPIKeys` → `TestChannelOrchestrator.TestChannelAPIKeys`.
    async fn test_channel_api_keys(
        &self,
        channel_id: &str,
        model_id: Option<String>,
    ) -> Result<TestChannelApiKeysPayload, ChannelServiceError>;

    /// Mirrors Go `BulkArchiveChannels` → `ChannelService.BulkArchiveChannels`.
    /// `ids` are the raw `ID!` scalars; the host decodes them via the
    /// equivalent of `objects.IntGuids`.
    async fn bulk_archive_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError>;

    /// Mirrors Go `BulkDisableChannels`.
    async fn bulk_disable_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError>;

    /// Mirrors Go `BulkEnableChannels`.
    async fn bulk_enable_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError>;

    /// Mirrors Go `BulkRecoverChannels`.
    async fn bulk_recover_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError>;

    /// Mirrors Go `BulkDeleteChannels`.
    async fn bulk_delete_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError>;

    /// Mirrors Go `SyncChannelModels` → `ChannelService.SyncChannelModels`:
    /// sync the channel's supported models against the upstream list,
    /// optionally filtered by `pattern`. Returns the id + refreshed models.
    async fn sync_channel_models(
        &self,
        channel_id: &str,
        pattern: Option<String>,
    ) -> Result<SyncChannelModelsPayload, ChannelServiceError>;
}

/// Resolves the injected [`ChannelExtMutationServices`] from the async-graphql
/// data bag, surfacing the Go-equivalent "channel service is not available"
/// message when no service was wired.
pub(crate) fn channel_ext_mutation_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ChannelExtMutationServices>, String> {
    match ctx.data::<Arc<dyn ChannelExtMutationServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ChannelServiceError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Resolver wiring (for the coordinator / already pasted into the real roots).
//
// async-graphql's `#[Object]` macro cannot be split across modules (two blocks
// on the same root type → E0119). This slice therefore contributes NO
// `#[Object] impl MutationRoot`; it exposes the typed service-lookup helper
// [`channel_ext_mutation_services`] + the types, and the resolver method bodies
// are pasted into the single `#[Object] impl MutationRoot` in `mutation.rs`.
// The `TestMutationRoot` in the test module is a byte-for-byte reference impl.
//
// Mutation methods (pasted into `#[Object] impl MutationRoot` in `mutation.rs`):
//
// ```ignore
// async fn update_channel_status(&self, ctx, id: ID, status: ChannelStatus) -> Result<Channel, String>
// async fn duplicate_channel(&self, ctx, source_id: ID, input: CreateChannelInput) -> Result<Channel, String>  // arg name `sourceID`
// async fn save_channel_endpoints(&self, ctx, input: SaveChannelEndpointsInput) -> Result<Channel, String>
// async fn test_channel(&self, ctx, input: TestChannelInput) -> Result<TestChannelPayload, String>
// async fn test_channel_api_key(&self, ctx, channel_id: ID, key: String, model_id: Option<String>) -> Result<TestApiKeyResult, String>  // args channelID/key/modelID
// async fn test_channel_api_keys(&self, ctx, channel_id: ID, model_id: Option<String>) -> Result<TestChannelApiKeysPayload, String>     // args channelID/modelID
// async fn bulk_archive_channels(&self, ctx, ids: Vec<ID>) -> Result<bool, String>   // + disable/enable/recover/delete
// async fn sync_channel_models(&self, ctx, channel_id: ID, pattern: Option<String>) -> Result<SyncChannelModelsPayload, String>  // args channelID/pattern
// ```
// ===========================================================================

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::{EmptySubscription, Name, Object, Schema, Value};

    use super::*;
    use crate::channel::{Channel, ChannelStatus, ChannelType, CreateChannelInput};
    use crate::scalars::TimeScalar;
    use crate::sdl_parity::{assert_block_parity, snapshot_text};

    type TestError = Box<dyn std::error::Error>;

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

    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    fn sample_channel(id: i64, name: &str, status: ChannelStatus) -> Channel {
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
            status,
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

    #[derive(Default)]
    struct InMemoryChannelExtService {
        channels: Arc<Mutex<Vec<Channel>>>,
        bulk_archive_calls: Arc<Mutex<Vec<Vec<String>>>>,
        bulk_disable_calls: Arc<Mutex<Vec<Vec<String>>>>,
        bulk_enable_calls: Arc<Mutex<Vec<Vec<String>>>>,
        bulk_recover_calls: Arc<Mutex<Vec<Vec<String>>>>,
        bulk_delete_calls: Arc<Mutex<Vec<Vec<String>>>>,
        test_channel_result: Option<TestChannelPayload>,
        test_channel_error: Option<ChannelServiceError>,
        fail_missing_channel: bool,
    }

    fn ids_to_strings(ids: &[ID]) -> Vec<String> {
        ids.iter().map(|id| id.as_str().to_owned()).collect()
    }

    #[async_trait::async_trait]
    impl ChannelExtMutationServices for InMemoryChannelExtService {
        async fn update_channel_status(
            &self,
            id: &str,
            status: ChannelStatus,
        ) -> Result<Channel, ChannelServiceError> {
            let mut guard = lock(&self.channels);
            match guard.iter_mut().find(|c| c.id.as_str() == id) {
                Some(channel) => {
                    channel.status = status;
                    Ok(channel.clone())
                }
                None => Err(ChannelServiceError::NotFound),
            }
        }

        async fn duplicate_channel(
            &self,
            _actor_user_id: Option<i64>,
            source_id: &str,
            input: CreateChannelInput,
        ) -> Result<Channel, ChannelServiceError> {
            let mut guard = lock(&self.channels);
            if !guard.iter().any(|c| c.id.as_str() == source_id) {
                return Err(ChannelServiceError::NotFound);
            }
            let new_id = guard.len() as i64 + 1;
            let mut cloned = sample_channel(new_id, &input.name, ChannelStatus::Disabled);
            cloned.channel_type = input.channel_type;
            cloned.supported_models = input.supported_models;
            cloned.default_test_model = input.default_test_model;
            guard.push(cloned.clone());
            Ok(cloned)
        }

        async fn save_channel_endpoints(
            &self,
            input: SaveChannelEndpointsInput,
        ) -> Result<Channel, ChannelServiceError> {
            let mut guard = lock(&self.channels);
            match guard.iter_mut().find(|c| c.id == input.channel_id) {
                Some(channel) => {
                    channel.endpoints = Some(
                        input
                            .endpoints
                            .into_iter()
                            .map(|e| crate::channel::ChannelEndpoint {
                                api_format: e.api_format,
                                path: e.path,
                                base_url: e.base_url,
                                transport: e.transport,
                            })
                            .collect(),
                    );
                    Ok(channel.clone())
                }
                None => Err(ChannelServiceError::NotFound),
            }
        }

        async fn test_channel(
            &self,
            _input: TestChannelInput,
        ) -> Result<TestChannelPayload, ChannelServiceError> {
            if let Some(err) = &self.test_channel_error {
                return Err(err.clone());
            }
            Ok(self
                .test_channel_result
                .clone()
                .unwrap_or(TestChannelPayload {
                    latency: 12.5,
                    success: true,
                    message: Some("ok".to_owned()),
                    error: None,
                }))
        }

        async fn test_channel_api_key(
            &self,
            _channel_id: &str,
            key: &str,
            _model_id: Option<String>,
        ) -> Result<TestApiKeyResult, ChannelServiceError> {
            let key_prefix = key.chars().take(6).collect::<String>();
            Ok(TestApiKeyResult {
                key_prefix,
                success: true,
                latency: 8.0,
                error: None,
                disabled: false,
            })
        }

        async fn test_channel_api_keys(
            &self,
            channel_id: &str,
            _model_id: Option<String>,
        ) -> Result<TestChannelApiKeysPayload, ChannelServiceError> {
            Ok(TestChannelApiKeysPayload {
                channel_id: ID::from(channel_id.to_owned()),
                total: 2,
                success_count: 1,
                failed_count: 1,
                results: vec![
                    TestApiKeyResult {
                        key_prefix: "sk-aaa".to_owned(),
                        success: true,
                        latency: 5.0,
                        error: None,
                        disabled: false,
                    },
                    TestApiKeyResult {
                        key_prefix: "sk-bbb".to_owned(),
                        success: false,
                        latency: 0.0,
                        error: Some("401".to_owned()),
                        disabled: true,
                    },
                ],
            })
        }

        async fn bulk_archive_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError> {
            lock(&self.bulk_archive_calls).push(ids_to_strings(&ids));
            if self.fail_missing_channel {
                return Err(ChannelServiceError::NotFound);
            }
            Ok(())
        }

        async fn bulk_disable_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError> {
            lock(&self.bulk_disable_calls).push(ids_to_strings(&ids));
            Ok(())
        }

        async fn bulk_enable_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError> {
            lock(&self.bulk_enable_calls).push(ids_to_strings(&ids));
            Ok(())
        }

        async fn bulk_recover_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError> {
            lock(&self.bulk_recover_calls).push(ids_to_strings(&ids));
            Ok(())
        }

        async fn bulk_delete_channels(&self, ids: Vec<ID>) -> Result<(), ChannelServiceError> {
            lock(&self.bulk_delete_calls).push(ids_to_strings(&ids));
            Ok(())
        }

        async fn sync_channel_models(
            &self,
            channel_id: &str,
            _pattern: Option<String>,
        ) -> Result<SyncChannelModelsPayload, ChannelServiceError> {
            let guard = lock(&self.channels);
            match guard.iter().find(|c| c.id.as_str() == channel_id) {
                Some(channel) => Ok(SyncChannelModelsPayload {
                    channel_id: ID::from(channel_id.to_owned()),
                    supported_models: channel.supported_models.clone(),
                }),
                None => Err(ChannelServiceError::NotFound),
            }
        }
    }

    // ---------------------------------------------------------------------
    // Test reference roots. `#[Object]` cannot be split across modules, so
    // these mirror what the coordinator pastes into the real MutationRoot
    // (mutation.rs). A Query root is required for a schema; the bare probe
    // suffices.
    // ---------------------------------------------------------------------

    struct TestQueryRoot;

    #[Object]
    impl TestQueryRoot {
        async fn probe(&self) -> bool {
            true
        }
    }

    struct TestMutationRoot;

    #[Object]
    impl TestMutationRoot {
        async fn update_channel_status(
            &self,
            ctx: &Context<'_>,
            id: ID,
            status: ChannelStatus,
        ) -> Result<Channel, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.update_channel_status(id.as_str(), status)
                .await
                .map_err(|e| e.to_string())
        }

        #[graphql(name = "duplicateChannel")]
        async fn duplicate_channel(
            &self,
            ctx: &Context<'_>,
            #[graphql(name = "sourceID")] source_id: ID,
            input: CreateChannelInput,
        ) -> Result<Channel, String> {
            let mut input = input;
            crate::channel::validate_and_normalize_channel_settings_input(input.settings.as_mut())
                .map_err(|error| error.to_string())?;
            let s = channel_ext_mutation_services(ctx)?;
            let actor_user_id = ctx
                .data_opt::<crate::me::CurrentUser>()
                .map(|user| user.user_id);
            s.duplicate_channel(actor_user_id, source_id.as_str(), input)
                .await
                .map_err(|e| e.to_string())
        }

        async fn save_channel_endpoints(
            &self,
            ctx: &Context<'_>,
            input: SaveChannelEndpointsInput,
        ) -> Result<Channel, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.save_channel_endpoints(input)
                .await
                .map_err(|e| e.to_string())
        }

        async fn test_channel(
            &self,
            ctx: &Context<'_>,
            input: TestChannelInput,
        ) -> Result<TestChannelPayload, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.test_channel(input).await.map_err(|e| e.to_string())
        }

        #[graphql(name = "testChannelAPIKey")]
        async fn test_channel_api_key(
            &self,
            ctx: &Context<'_>,
            #[graphql(name = "channelID")] channel_id: ID,
            key: String,
            #[graphql(name = "modelID")] model_id: Option<String>,
        ) -> Result<TestApiKeyResult, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.test_channel_api_key(channel_id.as_str(), &key, model_id)
                .await
                .map_err(|e| e.to_string())
        }

        #[graphql(name = "testChannelAPIKeys")]
        async fn test_channel_api_keys(
            &self,
            ctx: &Context<'_>,
            #[graphql(name = "channelID")] channel_id: ID,
            #[graphql(name = "modelID")] model_id: Option<String>,
        ) -> Result<TestChannelApiKeysPayload, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.test_channel_api_keys(channel_id.as_str(), model_id)
                .await
                .map_err(|e| e.to_string())
        }

        async fn bulk_archive_channels(
            &self,
            ctx: &Context<'_>,
            ids: Vec<ID>,
        ) -> Result<bool, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.bulk_archive_channels(ids)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        async fn bulk_disable_channels(
            &self,
            ctx: &Context<'_>,
            ids: Vec<ID>,
        ) -> Result<bool, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.bulk_disable_channels(ids)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        async fn bulk_enable_channels(
            &self,
            ctx: &Context<'_>,
            ids: Vec<ID>,
        ) -> Result<bool, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.bulk_enable_channels(ids)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        async fn bulk_recover_channels(
            &self,
            ctx: &Context<'_>,
            ids: Vec<ID>,
        ) -> Result<bool, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.bulk_recover_channels(ids)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        async fn bulk_delete_channels(
            &self,
            ctx: &Context<'_>,
            ids: Vec<ID>,
        ) -> Result<bool, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.bulk_delete_channels(ids)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        #[graphql(name = "syncChannelModels")]
        async fn sync_channel_models(
            &self,
            ctx: &Context<'_>,
            #[graphql(name = "channelID")] channel_id: ID,
            pattern: Option<String>,
        ) -> Result<SyncChannelModelsPayload, String> {
            let s = channel_ext_mutation_services(ctx)?;
            s.sync_channel_models(channel_id.as_str(), pattern)
                .await
                .map_err(|e| e.to_string())
        }
    }

    type TestSchema = Schema<TestQueryRoot, TestMutationRoot, EmptySubscription>;

    fn test_schema_builder()
    -> async_graphql::SchemaBuilder<TestQueryRoot, TestMutationRoot, EmptySubscription> {
        Schema::build(TestQueryRoot, TestMutationRoot, EmptySubscription)
            .register_output_type::<crate::channel::Node>()
    }

    fn schema_with(store: &InMemoryChannelExtService) -> TestSchema {
        let svc: Arc<dyn ChannelExtMutationServices> = Arc::new(InMemoryChannelExtService {
            channels: Arc::clone(&store.channels),
            bulk_archive_calls: Arc::clone(&store.bulk_archive_calls),
            bulk_disable_calls: Arc::clone(&store.bulk_disable_calls),
            bulk_enable_calls: Arc::clone(&store.bulk_enable_calls),
            bulk_recover_calls: Arc::clone(&store.bulk_recover_calls),
            bulk_delete_calls: Arc::clone(&store.bulk_delete_calls),
            test_channel_result: store.test_channel_result.clone(),
            test_channel_error: store.test_channel_error.clone(),
            fail_missing_channel: store.fail_missing_channel,
        });
        test_schema_builder().data(svc).finish()
    }

    fn bare_schema() -> TestSchema {
        test_schema_builder().finish()
    }

    // -----------------------------------------------------------------
    // SDL parity — every type this slice contributes must match snapshot.
    // -----------------------------------------------------------------

    #[test]
    fn sdl_output_types_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "type TestChannelPayload",
            "type TestAPIKeyResult",
            "type TestChannelAPIKeysPayload",
            "type SyncChannelModelsPayload",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_input_types_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = snapshot_text()?;
        for header in ["input TestChannelInput", "input SaveChannelEndpointsInput"] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_mutation_signatures_present() {
        let sdl = bare_schema().sdl();
        for expected in [
            "updateChannelStatus(",
            "duplicateChannel(",
            "saveChannelEndpoints(",
            "testChannel(",
            "testChannelAPIKey(",
            "testChannelAPIKeys(",
            "bulkArchiveChannels(",
            "bulkDisableChannels(",
            "bulkEnableChannels(",
            "bulkRecoverChannels(",
            "bulkDeleteChannels(",
            "syncChannelModels(",
        ] {
            assert!(
                sdl.contains(expected),
                "SDL missing mutation {expected}:\n{sdl}"
            );
        }
        // Acronym argument/field names render verbatim.
        assert!(
            sdl.contains("channelID: ID!"),
            "channelID acronym missing:\n{sdl}"
        );
        assert!(
            sdl.contains("keyPrefix: String!"),
            "keyPrefix field missing:\n{sdl}"
        );
    }

    // -----------------------------------------------------------------
    // Resolver happy-paths.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn update_channel_status_sets_status_and_returns_channel() {
        let store = InMemoryChannelExtService::default();
        lock(&store.channels).push(sample_channel(1, "c1", ChannelStatus::Enabled));
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { updateChannelStatus(id: "1", status: disabled) { id status } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("status: disabled"), "status not updated: {s}");
    }

    #[tokio::test]
    async fn duplicate_channel_clones_source() {
        let store = InMemoryChannelExtService::default();
        lock(&store.channels).push(sample_channel(1, "orig", ChannelStatus::Enabled));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation { duplicateChannel(sourceID: "1", input: { type: openai, name: "copy", credentials: {}, supportedModels: ["gpt-4o"], defaultTestModel: "gpt-4o" }) { id name } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("name: \"copy\""), "clone name missing: {s}");
    }

    #[tokio::test]
    async fn duplicate_channel_rejects_invalid_billing_currency() {
        let store = InMemoryChannelExtService::default();
        lock(&store.channels).push(sample_channel(1, "orig", ChannelStatus::Enabled));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation {
                    duplicateChannel(sourceID: "1", input: {
                        type: openai,
                        name: "copy",
                        credentials: {},
                        supportedModels: ["gpt-4o"],
                        defaultTestModel: "gpt-4o",
                        settings: {
                            billingCurrency: "US1",
                            rechargeMultiplier: "1"
                        }
                    }) { id }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        assert!(
            resp.errors[0]
                .message
                .contains("billing_currency must match [A-Z]{3}"),
            "unexpected error: {}",
            resp.errors[0].message
        );
        assert_eq!(lock(&store.channels).len(), 1);
    }

    #[tokio::test]
    async fn save_channel_endpoints_persists_and_returns_channel() {
        let store = InMemoryChannelExtService::default();
        lock(&store.channels).push(sample_channel(7, "c7", ChannelStatus::Enabled));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation { saveChannelEndpoints(input: { channelID: "7", endpoints: [{ apiFormat: "openai", baseURL: "https://api" }] }) { id } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let saved = lock(&store.channels)
            .iter()
            .find(|c| c.id.as_str() == "7")
            .and_then(|c| c.endpoints.clone());
        match saved {
            Some(endpoints) => assert_eq!(endpoints.len(), 1),
            None => panic!("endpoints not persisted"),
        }
    }

    #[tokio::test]
    async fn test_channel_returns_payload() {
        let store = InMemoryChannelExtService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation { testChannel(input: { channelID: "1", modelID: "gpt-4o" }) { latency success message error } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let payload = match obj.get(&Name::new("testChannel")) {
            Some(v) => as_object(v),
            None => panic!("testChannel field missing"),
        };
        match payload.get(&Name::new("success")) {
            Some(Value::Boolean(true)) => {}
            other => panic!("success field unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_channel_surfaces_error() {
        let store = InMemoryChannelExtService {
            test_channel_error: Some(ChannelServiceError::Update("boom".to_owned())),
            ..InMemoryChannelExtService::default()
        };
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { testChannel(input: { channelID: "1" }) { success } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        assert!(format!("{}", resp.errors[0]).contains("boom"));
    }

    #[tokio::test]
    async fn test_channel_api_key_returns_prefixed_result() {
        let store = InMemoryChannelExtService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation { testChannelAPIKey(channelID: "1", key: "sk-secret123", modelID: "gpt-4o") { keyPrefix success disabled } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("keyPrefix: \"sk-sec\""), "key prefix wrong: {s}");
    }

    #[tokio::test]
    async fn test_channel_api_keys_returns_aggregate() {
        let store = InMemoryChannelExtService::default();
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation { testChannelAPIKeys(channelID: "3") { channelID total successCount failedCount results { keyPrefix success } } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let payload = match obj.get(&Name::new("testChannelAPIKeys")) {
            Some(v) => as_object(v),
            None => panic!("testChannelAPIKeys field missing"),
        };
        match payload.get(&Name::new("total")) {
            Some(Value::Number(n)) => assert_eq!(n.as_i64(), Some(2)),
            other => panic!("total unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn bulk_status_mutations_return_true_and_forward_ids() {
        let store = InMemoryChannelExtService::default();
        let schema = schema_with(&store);

        for (field, calls) in [
            ("bulkArchiveChannels", &store.bulk_archive_calls),
            ("bulkDisableChannels", &store.bulk_disable_calls),
            ("bulkEnableChannels", &store.bulk_enable_calls),
            ("bulkRecoverChannels", &store.bulk_recover_calls),
            ("bulkDeleteChannels", &store.bulk_delete_calls),
        ] {
            let query = format!(r#"mutation {{ {field}(ids: ["1", "2"]) }}"#);
            let resp = schema.execute(&query).await;
            assert!(resp.errors.is_empty(), "{field} errors: {:?}", resp.errors);
            let captured = lock(calls);
            assert_eq!(
                captured.last().map(Vec::as_slice),
                Some(&["1".to_owned(), "2".to_owned()][..]),
                "{field} did not forward ids"
            );
        }
    }

    #[tokio::test]
    async fn bulk_archive_surfaces_service_error() {
        let store = InMemoryChannelExtService {
            fail_missing_channel: true,
            ..InMemoryChannelExtService::default()
        };
        let schema = schema_with(&store);

        let resp = schema
            .execute(r#"mutation { bulkArchiveChannels(ids: ["9"]) }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        assert!(format!("{}", resp.errors[0]).contains("channel not found"));
    }

    #[tokio::test]
    async fn sync_channel_models_returns_id_and_models() {
        let store = InMemoryChannelExtService::default();
        lock(&store.channels).push(sample_channel(5, "c5", ChannelStatus::Enabled));
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"mutation { syncChannelModels(channelID: "5") { channelID supportedModels } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("gpt-4o"), "supported models missing: {s}");
    }

    // -----------------------------------------------------------------
    // Service-unavailable fallback.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn resolvers_surface_service_unavailable_when_unwired() {
        let schema = bare_schema();

        let resp = schema
            .execute(r#"mutation { bulkArchiveChannels(ids: ["1"]) }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        assert!(
            format!("{}", resp.errors[0]).contains("channel service is not available"),
            "unexpected msg: {}",
            resp.errors[0]
        );
    }
}
