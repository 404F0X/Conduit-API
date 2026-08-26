//! GAP-D (remainder) — system-settings GraphQL slice: video storage, webhook
//! notifier config, auto-backup settings, and the two onboarding-completion
//! mutations.
//!
//! Ports the settings operations declared in the captured contract snapshot
//! `tests/contracts/admin_graphql_schema.graphql` that the earlier system
//! slices (`crate::system`, `crate::system_ext`) left pending. Every GraphQL
//! type/input is copied field-for-field from the snapshot; every resolver
//! mirrors the Go body in `conduit/internal/server/gql/system.resolvers.go`
//! and `backup.resolvers.go`.
//!
//! ## Queries ported (snapshot `extend type Query`)
//!
//!   - `webhookNotifierConfig: WebhookNotifierConfig!` (snapshot 9773) — Go
//!     `queryResolver.WebhookNotifierConfig` (system.resolvers.go:418).
//!   - `videoStorageSettings: VideoStorageSettings!` (snapshot 9781) — Go
//!     `queryResolver.VideoStorageSettings` (system.resolvers.go:518).
//!   - `autoBackupSettings: AutoBackupSettings!` (snapshot 1038) — Go
//!     `queryResolver.AutoBackupSettings` (backup.resolvers.go:140).
//!
//! ## Mutations ported (snapshot `extend type Mutation`)
//!
//!   - `updateWebhookNotifierConfig(input: WebhookNotifierConfigInput!): Boolean!`
//!     (snapshot 9704) — Go `UpdateWebhookNotifierConfig`
//!     (system.resolvers.go:70).
//!   - `updateVideoStorageSettings(input: UpdateVideoStorageSettingsInput!): Boolean!`
//!     (snapshot 9712) — Go `UpdateVideoStorageSettings`
//!     (system.resolvers.go:173).
//!   - `updateAutoBackupSettings(input: UpdateAutoBackupSettingsInput!): Boolean!`
//!     (snapshot 1042) — Go `UpdateAutoBackupSettings` (backup.resolvers.go:55):
//!     read current, apply the partial merge (`None` preserves current),
//!     persist. NOTE (Go parity): the Go resolver is owner-gated
//!     (`contexts.GetUser` + `IsOwner`, returns `ErrNotOwner`); the scope check
//!     is the host's responsibility (same convention the other admin slices
//!     use), so this slice performs only the read-merge-write.
//!   - `completeSystemModelSettingOnboarding(input: CompleteSystemModelSettingOnboardingInput!): Boolean!`
//!     (snapshot 9708) — Go `CompleteSystemModelSettingOnboarding`
//!     (system.resolvers.go:123).
//!   - `completeAutoDisableChannelOnboarding(input: CompleteAutoDisableChannelOnboardingInput!): Boolean!`
//!     (snapshot 9709) — Go `CompleteAutoDisableChannelOnboarding`
//!     (system.resolvers.go:133).
//!
//! ## Types owned by this slice
//!
//!   - `type VideoStorageSettings` + `input UpdateVideoStorageSettingsInput`.
//!   - `type WebhookNotifierConfig` / `WebhookTarget` / `WebhookSubscription` /
//!     `HeaderEntry` + their `*Input` twins.
//!   - `type AutoBackupSettings` + `input UpdateAutoBackupSettingsInput` +
//!     `enum BackupFrequency`.
//!   - `input CompleteSystemModelSettingOnboardingInput` /
//!     `input CompleteAutoDisableChannelOnboardingInput` (both carry the
//!     placeholder `dummy: String` the Go schema declares).
//!
//! `ProxyConfig` / `ProxyConfigInput` already exist in `crate::channel` and are
//! reused verbatim (webhook targets carry an optional proxy).
//!
//! ## Not ported here (still pending — separate payloads / owner-gated flows)
//!
//! `triggerAutoBackup: TriggerBackupPayload!`, the backup/restore operations,
//! and the GC-cleanup / cache-diagnostics groups each carry their own payload
//! types and (mostly) owner-only gating; they belong to a follow-up slice to
//! keep this diff reviewable.
//!
//! ## Service wiring
//!
//! Introduces a NEW host-injected trait [`SystemSettingsExtServices`] rather
//! than extending `system::SystemSettingsServices` (which would break that
//! module's in-memory test double). The host wires one concrete type that
//! implements every system-settings trait. Unwired => "system service is not
//! available" (same message the base system slice uses).

use std::sync::Arc;

use async_graphql::{Context, Enum, InputObject, SimpleObject};

use crate::channel::{ProxyConfig, ProxyConfigInput};
use crate::scalars::TimeScalar;

// ===========================================================================
// Video storage settings
// ===========================================================================

/// `type VideoStorageSettings` (snapshot 9644-9649). Mirrors Go
/// `biz.VideoStorageSettings` (system.go). All four fields non-null.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "VideoStorageSettings")]
pub struct VideoStorageSettings {
    pub enabled: bool,
    /// All-caps acronym: default camelCase would emit `dataStorageId`.
    #[graphql(name = "dataStorageID")]
    pub data_storage_id: i32,
    pub scan_interval_minutes: i32,
    pub scan_limit: i32,
}

/// `input UpdateVideoStorageSettingsInput` (snapshot 9651-9656). All fields
/// nullable; the Go resolver forwards the decoded `biz.VideoStorageSettings`
/// struct straight to `SetVideoStorageSettings` — this conversion materializes
/// it from the partial input using zero defaults for absent fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateVideoStorageSettingsInput")]
pub struct UpdateVideoStorageSettingsInput {
    pub enabled: Option<bool>,
    #[graphql(name = "dataStorageID")]
    pub data_storage_id: Option<i32>,
    pub scan_interval_minutes: Option<i32>,
    pub scan_limit: Option<i32>,
}

impl From<UpdateVideoStorageSettingsInput> for VideoStorageSettings {
    fn from(input: UpdateVideoStorageSettingsInput) -> Self {
        VideoStorageSettings {
            enabled: input.enabled.unwrap_or(false),
            data_storage_id: input.data_storage_id.unwrap_or(0),
            scan_interval_minutes: input.scan_interval_minutes.unwrap_or(0),
            scan_limit: input.scan_limit.unwrap_or(0),
        }
    }
}

// ===========================================================================
// Webhook notifier config
// ===========================================================================

/// `type HeaderEntry { key value }` (snapshot). Both fields non-null.
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "HeaderEntry")]
pub struct HeaderEntry {
    pub key: String,
    pub value: String,
}

/// `input HeaderEntryInput { key value }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "HeaderEntryInput")]
pub struct HeaderEntryInput {
    pub key: String,
    pub value: String,
}

impl From<HeaderEntryInput> for HeaderEntry {
    fn from(input: HeaderEntryInput) -> Self {
        HeaderEntry {
            key: input.key,
            value: input.value,
        }
    }
}

/// `type WebhookTarget` (snapshot). `proxy` is nullable and reuses the shared
/// `ProxyConfig` output type from `crate::channel`.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "WebhookTarget")]
pub struct WebhookTarget {
    pub name: String,
    pub enabled: bool,
    pub url: String,
    pub proxy: Option<ProxyConfig>,
    pub timeout_ms: i32,
    pub headers: Vec<HeaderEntry>,
    pub body: String,
}

/// `input WebhookTargetInput` — `headers` is nullable on input, `proxy` reuses
/// the shared `ProxyConfigInput`.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "WebhookTargetInput")]
pub struct WebhookTargetInput {
    pub name: String,
    pub enabled: bool,
    pub url: String,
    pub proxy: Option<ProxyConfigInput>,
    pub timeout_ms: i32,
    pub headers: Option<Vec<HeaderEntryInput>>,
    pub body: String,
}

/// `type WebhookSubscription { event targetNames }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "WebhookSubscription")]
pub struct WebhookSubscription {
    pub event: String,
    pub target_names: Vec<String>,
}

/// `input WebhookSubscriptionInput { event targetNames }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "WebhookSubscriptionInput")]
pub struct WebhookSubscriptionInput {
    pub event: String,
    pub target_names: Vec<String>,
}

/// `type WebhookNotifierConfig { targets subscriptions }`. Mirrors Go
/// `biz.WebhookNotifierConfig`.
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "WebhookNotifierConfig")]
pub struct WebhookNotifierConfig {
    pub targets: Vec<WebhookTarget>,
    pub subscriptions: Vec<WebhookSubscription>,
}

/// `input WebhookNotifierConfigInput` — both lists nullable. The Go resolver
/// forwards the decoded `biz.WebhookNotifierConfig` to `SetWebhookNotifierConfig`;
/// this conversion materializes it, defaulting absent lists to empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "WebhookNotifierConfigInput")]
pub struct WebhookNotifierConfigInput {
    pub targets: Option<Vec<WebhookTargetInput>>,
    pub subscriptions: Option<Vec<WebhookSubscriptionInput>>,
}

impl From<WebhookTargetInput> for WebhookTarget {
    fn from(input: WebhookTargetInput) -> Self {
        WebhookTarget {
            name: input.name,
            enabled: input.enabled,
            url: input.url,
            proxy: input.proxy.map(ProxyConfig::from),
            timeout_ms: input.timeout_ms,
            headers: input
                .headers
                .map(|hs| hs.into_iter().map(HeaderEntry::from).collect())
                .unwrap_or_default(),
            body: input.body,
        }
    }
}

impl From<WebhookSubscriptionInput> for WebhookSubscription {
    fn from(input: WebhookSubscriptionInput) -> Self {
        WebhookSubscription {
            event: input.event,
            target_names: input.target_names,
        }
    }
}

impl From<WebhookNotifierConfigInput> for WebhookNotifierConfig {
    fn from(input: WebhookNotifierConfigInput) -> Self {
        WebhookNotifierConfig {
            targets: input
                .targets
                .map(|ts| ts.into_iter().map(WebhookTarget::from).collect())
                .unwrap_or_default(),
            subscriptions: input
                .subscriptions
                .map(|ss| ss.into_iter().map(WebhookSubscription::from).collect())
                .unwrap_or_default(),
        }
    }
}

// ===========================================================================
// Auto-backup settings
// ===========================================================================

/// `enum BackupFrequency { daily weekly monthly }` — snapshot. Lowercase
/// values pinned explicitly (default SCREAMING_SNAKE would mangle them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "BackupFrequency")]
pub enum BackupFrequency {
    #[graphql(name = "daily")]
    Daily,
    #[graphql(name = "weekly")]
    Weekly,
    #[graphql(name = "monthly")]
    Monthly,
}

/// `type AutoBackupSettings` (snapshot). Mirrors Go `biz.AutoBackupSettings`.
/// `lastBackupAt` / `lastBackupError` are nullable; all toggles + counts
/// non-null.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "AutoBackupSettings")]
pub struct AutoBackupSettings {
    pub enabled: bool,
    pub frequency: BackupFrequency,
    #[graphql(name = "dataStorageID")]
    pub data_storage_id: i32,
    pub include_channels: bool,
    pub include_models: bool,
    #[graphql(name = "includeAPIKeys")]
    pub include_api_keys: bool,
    pub include_model_prices: bool,
    pub include_usage_stats: bool,
    pub include_request_logs: bool,
    pub retention_days: i32,
    pub last_backup_at: Option<TimeScalar>,
    pub last_backup_error: Option<String>,
}

/// `input UpdateAutoBackupSettingsInput` (snapshot). Every field nullable; the
/// Go resolver reads the current settings and overrides only the `Some` fields
/// (backup.resolvers.go:55-). `lastBackupAt`/`lastBackupError` are read-only
/// (not in the input).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateAutoBackupSettingsInput")]
pub struct UpdateAutoBackupSettingsInput {
    pub enabled: Option<bool>,
    pub frequency: Option<BackupFrequency>,
    #[graphql(name = "dataStorageID")]
    pub data_storage_id: Option<i32>,
    pub include_channels: Option<bool>,
    pub include_models: Option<bool>,
    #[graphql(name = "includeAPIKeys")]
    pub include_api_keys: Option<bool>,
    pub include_model_prices: Option<bool>,
    pub include_usage_stats: Option<bool>,
    pub include_request_logs: Option<bool>,
    pub retention_days: Option<i32>,
}

/// Pure helper applying the Go resolver's partial merge (backup.resolvers.go:
/// 63-): each `Some` input field overrides the current value; `None` fields are
/// left untouched. `lastBackupAt`/`lastBackupError` are preserved from current
/// (not part of the input).
pub fn merge_auto_backup_settings(
    current: AutoBackupSettings,
    input: &UpdateAutoBackupSettingsInput,
) -> AutoBackupSettings {
    AutoBackupSettings {
        enabled: input.enabled.unwrap_or(current.enabled),
        frequency: input.frequency.unwrap_or(current.frequency),
        data_storage_id: input.data_storage_id.unwrap_or(current.data_storage_id),
        include_channels: input.include_channels.unwrap_or(current.include_channels),
        include_models: input.include_models.unwrap_or(current.include_models),
        include_api_keys: input.include_api_keys.unwrap_or(current.include_api_keys),
        include_model_prices: input
            .include_model_prices
            .unwrap_or(current.include_model_prices),
        include_usage_stats: input
            .include_usage_stats
            .unwrap_or(current.include_usage_stats),
        include_request_logs: input
            .include_request_logs
            .unwrap_or(current.include_request_logs),
        retention_days: input.retention_days.unwrap_or(current.retention_days),
        last_backup_at: current.last_backup_at,
        last_backup_error: current.last_backup_error,
    }
}

// ===========================================================================
// Onboarding-completion inputs
// ===========================================================================

/// `input CompleteSystemModelSettingOnboardingInput { dummy: String }` — the Go
/// schema carries a placeholder field so the input is non-null at the GraphQL
/// layer even though the resolver ignores it.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "CompleteSystemModelSettingOnboardingInput")]
pub struct CompleteSystemModelSettingOnboardingInput {
    pub dummy: Option<String>,
}

/// `input CompleteAutoDisableChannelOnboardingInput { dummy: String }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "CompleteAutoDisableChannelOnboardingInput")]
pub struct CompleteAutoDisableChannelOnboardingInput {
    pub dummy: Option<String>,
}

// ===========================================================================
// Service trait (host-injected)
// ===========================================================================

/// Error surface for this slice. Messages mirror the Go `fmt.Errorf("...: %w")`
/// prefixes so frontend error handling stays stable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SystemSettingsExtError {
    #[error("system service is not available")]
    ServiceUnavailable,
    #[error("failed to get video storage settings: {0}")]
    VideoStorage(String),
    #[error("failed to update video storage settings: {0}")]
    UpdateVideoStorage(String),
    #[error("failed to get webhook notifier config: {0}")]
    WebhookConfig(String),
    #[error("failed to update webhook notifier config: {0}")]
    UpdateWebhookConfig(String),
    #[error("failed to get auto backup settings: {0}")]
    AutoBackup(String),
    #[error("failed to update auto backup settings: {0}")]
    UpdateAutoBackup(String),
    #[error("failed to complete system model setting onboarding: {0}")]
    CompleteModelOnboarding(String),
    #[error("failed to complete auto disable channel onboarding: {0}")]
    CompleteAutoDisableOnboarding(String),
}

/// Backs the video / webhook / auto-backup settings reads + writes and the two
/// onboarding-completion mutations. The host wires one concrete type that also
/// implements the other system-settings traits.
#[async_trait::async_trait]
pub trait SystemSettingsExtServices: Send + Sync {
    /// Mirrors Go `queryResolver.VideoStorageSettings` (system.resolvers.go:518).
    async fn video_storage_settings(&self) -> Result<VideoStorageSettings, SystemSettingsExtError>;

    /// Mirrors Go `UpdateVideoStorageSettings` write half
    /// (system.resolvers.go:173): persist the typed settings.
    async fn set_video_storage_settings(
        &self,
        settings: VideoStorageSettings,
    ) -> Result<(), SystemSettingsExtError>;

    /// Mirrors Go `queryResolver.WebhookNotifierConfig`
    /// (system.resolvers.go:418).
    async fn webhook_notifier_config(
        &self,
    ) -> Result<WebhookNotifierConfig, SystemSettingsExtError>;

    /// Mirrors Go `UpdateWebhookNotifierConfig` write half
    /// (system.resolvers.go:70): persist the typed config.
    async fn set_webhook_notifier_config(
        &self,
        config: WebhookNotifierConfig,
    ) -> Result<(), SystemSettingsExtError>;

    /// Mirrors Go `queryResolver.AutoBackupSettings` (backup.resolvers.go:140).
    async fn auto_backup_settings(&self) -> Result<AutoBackupSettings, SystemSettingsExtError>;

    /// Mirrors Go `UpdateAutoBackupSettings` write half (backup.resolvers.go:55):
    /// persist the merged settings. The resolver performs the read+merge; this
    /// method only writes. The owner-scope check is the host's responsibility.
    async fn set_auto_backup_settings(
        &self,
        settings: AutoBackupSettings,
    ) -> Result<(), SystemSettingsExtError>;

    /// Mirrors Go `CompleteSystemModelSettingOnboarding`
    /// (system.resolvers.go:123).
    async fn complete_system_model_setting_onboarding(&self) -> Result<(), SystemSettingsExtError>;

    /// Mirrors Go `CompleteAutoDisableChannelOnboarding`
    /// (system.resolvers.go:133).
    async fn complete_auto_disable_channel_onboarding(&self) -> Result<(), SystemSettingsExtError>;
}

/// Resolves the injected [`SystemSettingsExtServices`] from the async-graphql
/// data bag; absent wiring surfaces the shared "system service is not
/// available" failure mode.
pub(crate) fn system_settings_ext_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn SystemSettingsExtServices>, String> {
    match ctx.data::<Arc<dyn SystemSettingsExtServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(SystemSettingsExtError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Resolver wiring (for the coordinator).
//
// async-graphql's `#[Object]` macro forbids splitting a root's impl across
// modules (E0119), so this slice contributes no `#[Object]` block. The resolver
// bodies are pasted into the single `#[Object] impl QueryRoot` (lib.rs) and the
// single `#[Object] impl MutationRoot` (mutation.rs); the `TestQueryRoot` /
// `TestMutationRoot` in the test module below are byte-for-byte reference
// implementations.
//
// Query methods (paste into `#[Object] impl QueryRoot` in `lib.rs`):
//
// ```ignore
// async fn video_storage_settings(&self, ctx: &Context<'_>)
//     -> Result<crate::system_settings_ext::VideoStorageSettings, String> {
//     let s = crate::system_settings_ext::system_settings_ext_services(ctx)?;
//     s.video_storage_settings().await.map_err(|e| e.to_string())
// }
// async fn webhook_notifier_config(&self, ctx: &Context<'_>)
//     -> Result<crate::system_settings_ext::WebhookNotifierConfig, String> {
//     let s = crate::system_settings_ext::system_settings_ext_services(ctx)?;
//     s.webhook_notifier_config().await.map_err(|e| e.to_string())
// }
// async fn auto_backup_settings(&self, ctx: &Context<'_>)
//     -> Result<crate::system_settings_ext::AutoBackupSettings, String> {
//     let s = crate::system_settings_ext::system_settings_ext_services(ctx)?;
//     s.auto_backup_settings().await.map_err(|e| e.to_string())
// }
// ```
//
// Mutation methods (paste into `#[Object] impl MutationRoot` in `mutation.rs`;
// each returns `true` on success, mirroring the Go resolvers):
//
// ```ignore
// async fn update_video_storage_settings(&self, ctx, input: UpdateVideoStorageSettingsInput) -> Result<bool, String>
// async fn update_webhook_notifier_config(&self, ctx, input: WebhookNotifierConfigInput) -> Result<bool, String>
// async fn update_auto_backup_settings(&self, ctx, input: UpdateAutoBackupSettingsInput) -> Result<bool, String>  // read-merge-write
// async fn complete_system_model_setting_onboarding(&self, ctx, input: CompleteSystemModelSettingOnboardingInput) -> Result<bool, String>
// async fn complete_auto_disable_channel_onboarding(&self, ctx, input: CompleteAutoDisableChannelOnboardingInput) -> Result<bool, String>
// ```
// ===========================================================================

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::{Context, EmptySubscription, Name, Object, Schema, SchemaBuilder, Value};

    use super::*;
    use crate::sdl_parity::{assert_block_parity, snapshot_text};

    type TestError = Box<dyn std::error::Error>;

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

    impl Default for AutoBackupSettings {
        fn default() -> Self {
            Self {
                enabled: false,
                frequency: BackupFrequency::Daily,
                data_storage_id: 0,
                include_channels: false,
                include_models: false,
                include_api_keys: false,
                include_model_prices: false,
                include_usage_stats: false,
                include_request_logs: false,
                retention_days: 0,
                last_backup_at: None,
                last_backup_error: None,
            }
        }
    }

    #[derive(Default)]
    struct FakeSystemSettingsExtService {
        video: Arc<Mutex<VideoStorageSettings>>,
        video_read_error: Option<SystemSettingsExtError>,
        set_video_calls: Arc<Mutex<Vec<VideoStorageSettings>>>,
        webhook: Arc<Mutex<WebhookNotifierConfig>>,
        webhook_read_error: Option<SystemSettingsExtError>,
        set_webhook_calls: Arc<Mutex<Vec<WebhookNotifierConfig>>>,
        auto_backup: Arc<Mutex<AutoBackupSettings>>,
        auto_backup_read_error: Option<SystemSettingsExtError>,
        set_auto_backup_calls: Arc<Mutex<Vec<AutoBackupSettings>>>,
        model_onboarding_calls: Arc<Mutex<u32>>,
        auto_disable_onboarding_calls: Arc<Mutex<u32>>,
    }

    #[async_trait::async_trait]
    impl SystemSettingsExtServices for FakeSystemSettingsExtService {
        async fn video_storage_settings(
            &self,
        ) -> Result<VideoStorageSettings, SystemSettingsExtError> {
            match &self.video_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(*lock(&self.video)),
            }
        }

        async fn set_video_storage_settings(
            &self,
            settings: VideoStorageSettings,
        ) -> Result<(), SystemSettingsExtError> {
            lock(&self.set_video_calls).push(settings);
            *lock(&self.video) = settings;
            Ok(())
        }

        async fn webhook_notifier_config(
            &self,
        ) -> Result<WebhookNotifierConfig, SystemSettingsExtError> {
            match &self.webhook_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.webhook).clone()),
            }
        }

        async fn set_webhook_notifier_config(
            &self,
            config: WebhookNotifierConfig,
        ) -> Result<(), SystemSettingsExtError> {
            lock(&self.set_webhook_calls).push(config.clone());
            *lock(&self.webhook) = config;
            Ok(())
        }

        async fn auto_backup_settings(&self) -> Result<AutoBackupSettings, SystemSettingsExtError> {
            match &self.auto_backup_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.auto_backup).clone()),
            }
        }

        async fn set_auto_backup_settings(
            &self,
            settings: AutoBackupSettings,
        ) -> Result<(), SystemSettingsExtError> {
            lock(&self.set_auto_backup_calls).push(settings.clone());
            *lock(&self.auto_backup) = settings;
            Ok(())
        }

        async fn complete_system_model_setting_onboarding(
            &self,
        ) -> Result<(), SystemSettingsExtError> {
            *lock(&self.model_onboarding_calls) += 1;
            Ok(())
        }

        async fn complete_auto_disable_channel_onboarding(
            &self,
        ) -> Result<(), SystemSettingsExtError> {
            *lock(&self.auto_disable_onboarding_calls) += 1;
            Ok(())
        }
    }

    // Reference roots. `#[Object]` cannot be split across modules, so these
    // mirror what the coordinator pastes into the real QueryRoot (lib.rs) /
    // MutationRoot (mutation.rs).
    struct TestQueryRoot;

    #[Object]
    impl TestQueryRoot {
        async fn video_storage_settings(
            &self,
            ctx: &Context<'_>,
        ) -> Result<VideoStorageSettings, String> {
            let s = system_settings_ext_services(ctx)?;
            s.video_storage_settings().await.map_err(|e| e.to_string())
        }

        async fn webhook_notifier_config(
            &self,
            ctx: &Context<'_>,
        ) -> Result<WebhookNotifierConfig, String> {
            let s = system_settings_ext_services(ctx)?;
            s.webhook_notifier_config().await.map_err(|e| e.to_string())
        }

        async fn auto_backup_settings(
            &self,
            ctx: &Context<'_>,
        ) -> Result<AutoBackupSettings, String> {
            let s = system_settings_ext_services(ctx)?;
            s.auto_backup_settings().await.map_err(|e| e.to_string())
        }
    }

    struct TestMutationRoot;

    #[Object]
    impl TestMutationRoot {
        async fn update_video_storage_settings(
            &self,
            ctx: &Context<'_>,
            input: UpdateVideoStorageSettingsInput,
        ) -> Result<bool, String> {
            let s = system_settings_ext_services(ctx)?;
            let settings: VideoStorageSettings = input.into();
            s.set_video_storage_settings(settings)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        async fn update_webhook_notifier_config(
            &self,
            ctx: &Context<'_>,
            input: WebhookNotifierConfigInput,
        ) -> Result<bool, String> {
            let s = system_settings_ext_services(ctx)?;
            let config: WebhookNotifierConfig = input.into();
            s.set_webhook_notifier_config(config)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        async fn update_auto_backup_settings(
            &self,
            ctx: &Context<'_>,
            input: UpdateAutoBackupSettingsInput,
        ) -> Result<bool, String> {
            let s = system_settings_ext_services(ctx)?;
            let current = s.auto_backup_settings().await.map_err(|e| e.to_string())?;
            let merged = merge_auto_backup_settings(current, &input);
            s.set_auto_backup_settings(merged)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        async fn complete_system_model_setting_onboarding(
            &self,
            ctx: &Context<'_>,
            _input: CompleteSystemModelSettingOnboardingInput,
        ) -> Result<bool, String> {
            let s = system_settings_ext_services(ctx)?;
            s.complete_system_model_setting_onboarding()
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        async fn complete_auto_disable_channel_onboarding(
            &self,
            ctx: &Context<'_>,
            _input: CompleteAutoDisableChannelOnboardingInput,
        ) -> Result<bool, String> {
            let s = system_settings_ext_services(ctx)?;
            s.complete_auto_disable_channel_onboarding()
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
    }

    type TestSchema = Schema<TestQueryRoot, TestMutationRoot, EmptySubscription>;

    fn test_schema_builder() -> SchemaBuilder<TestQueryRoot, TestMutationRoot, EmptySubscription> {
        Schema::build(TestQueryRoot, TestMutationRoot, EmptySubscription)
    }

    fn schema_with(service: FakeSystemSettingsExtService) -> TestSchema {
        let arc: Arc<dyn SystemSettingsExtServices> = Arc::new(service);
        test_schema_builder().data(arc).finish()
    }

    // ---- SDL parity ----

    #[test]
    fn sdl_output_types_match_snapshot() -> Result<(), TestError> {
        let sdl = test_schema_builder().finish().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "type VideoStorageSettings",
            "type WebhookNotifierConfig",
            "type WebhookTarget",
            "type WebhookSubscription",
            "type HeaderEntry",
            "type AutoBackupSettings",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_input_types_match_snapshot() -> Result<(), TestError> {
        let sdl = test_schema_builder().finish().sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "input UpdateVideoStorageSettingsInput",
            "input WebhookNotifierConfigInput",
            "input WebhookTargetInput",
            "input WebhookSubscriptionInput",
            "input HeaderEntryInput",
            "input UpdateAutoBackupSettingsInput",
            "input CompleteSystemModelSettingOnboardingInput",
            "input CompleteAutoDisableChannelOnboardingInput",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_backup_frequency_enum_matches_snapshot() -> Result<(), TestError> {
        let sdl = test_schema_builder().finish().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "enum BackupFrequency",
            "enum BackupFrequency",
            &[],
        )?;
        Ok(())
    }

    #[test]
    fn sdl_acronym_fields_render_verbatim() {
        let sdl = test_schema_builder().finish().sdl();
        assert!(
            sdl.contains("dataStorageID: Int!"),
            "VideoStorage/AutoBackup dataStorageID acronym missing: {sdl}"
        );
        assert!(
            sdl.contains("includeAPIKeys: Boolean!"),
            "AutoBackup includeAPIKeys acronym missing: {sdl}"
        );
    }

    // ---- resolver: video storage ----

    #[tokio::test]
    async fn video_storage_settings_returns_service_data() {
        let fake = FakeSystemSettingsExtService {
            video: Arc::new(Mutex::new(VideoStorageSettings {
                enabled: true,
                data_storage_id: 7,
                scan_interval_minutes: 15,
                scan_limit: 100,
            })),
            ..FakeSystemSettingsExtService::default()
        };
        let schema = schema_with(fake);
        let resp = schema
            .execute(
                "{ videoStorageSettings { enabled dataStorageID scanIntervalMinutes scanLimit } }",
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let vss = match obj.get(&Name::new("videoStorageSettings")) {
            Some(v) => v,
            None => panic!("videoStorageSettings missing"),
        };
        let fields = as_object(vss);
        match fields.get(&Name::new("dataStorageID")) {
            Some(Value::Number(n)) => assert_eq!(n.as_i64(), Some(7)),
            other => panic!("dataStorageID unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_video_storage_settings_persists() -> Result<(), TestError> {
        let fake = FakeSystemSettingsExtService::default();
        let calls = Arc::clone(&fake.set_video_calls);
        let schema = schema_with(fake);
        let resp = schema
            .execute(
                r#"mutation { updateVideoStorageSettings(input: { enabled: true, dataStorageID: 3, scanIntervalMinutes: 30, scanLimit: 50 }) }"#,
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let recorded = lock(&calls);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].data_storage_id, 3);
        assert!(recorded[0].enabled);
        Ok(())
    }

    // ---- resolver: webhook config ----

    #[tokio::test]
    async fn webhook_notifier_config_returns_targets_and_subscriptions() {
        let fake = FakeSystemSettingsExtService {
            webhook: Arc::new(Mutex::new(WebhookNotifierConfig {
                targets: vec![WebhookTarget {
                    name: "primary".to_owned(),
                    enabled: true,
                    url: "https://hooks.example/1".to_owned(),
                    proxy: None,
                    timeout_ms: 5000,
                    headers: vec![HeaderEntry {
                        key: "X-Token".to_owned(),
                        value: "abc".to_owned(),
                    }],
                    body: "{}".to_owned(),
                }],
                subscriptions: vec![WebhookSubscription {
                    event: "request.failed".to_owned(),
                    target_names: vec!["primary".to_owned()],
                }],
            })),
            ..FakeSystemSettingsExtService::default()
        };
        let schema = schema_with(fake);
        let resp = schema
            .execute(
                "{ webhookNotifierConfig { targets { name url timeoutMs headers { key value } } subscriptions { event targetNames } } }",
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("name: \"primary\""), "target missing: {s}");
        assert!(s.contains("event: \"request.failed\""), "sub missing: {s}");
    }

    #[tokio::test]
    async fn update_webhook_notifier_config_persists() -> Result<(), TestError> {
        let fake = FakeSystemSettingsExtService::default();
        let calls = Arc::clone(&fake.set_webhook_calls);
        let schema = schema_with(fake);
        let resp = schema
            .execute(
                r#"mutation { updateWebhookNotifierConfig(input: { targets: [{ name: "t1", enabled: true, url: "https://h/1", timeoutMs: 1000, body: "{}" }], subscriptions: [{ event: "e1", targetNames: ["t1"] }] }) }"#,
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let recorded = lock(&calls);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].targets.len(), 1);
        assert_eq!(recorded[0].targets[0].name, "t1");
        assert_eq!(recorded[0].subscriptions.len(), 1);
        Ok(())
    }

    // ---- resolver: auto-backup (read-merge-write) ----

    #[tokio::test]
    async fn update_auto_backup_settings_merges_over_current() -> Result<(), TestError> {
        let fake = FakeSystemSettingsExtService {
            auto_backup: Arc::new(Mutex::new(AutoBackupSettings {
                enabled: false,
                frequency: BackupFrequency::Weekly,
                data_storage_id: 2,
                include_channels: true,
                retention_days: 30,
                ..AutoBackupSettings::default()
            })),
            ..FakeSystemSettingsExtService::default()
        };
        let calls = Arc::clone(&fake.set_auto_backup_calls);
        let schema = schema_with(fake);
        // Only flip `enabled`; every other field must be preserved from current.
        let resp = schema
            .execute(r#"mutation { updateAutoBackupSettings(input: { enabled: true }) }"#)
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let recorded = lock(&calls);
        assert_eq!(recorded.len(), 1);
        let saved = &recorded[0];
        assert!(saved.enabled, "enabled must be overridden to true");
        assert_eq!(
            saved.frequency,
            BackupFrequency::Weekly,
            "frequency preserved"
        );
        assert_eq!(saved.data_storage_id, 2, "dataStorageID preserved");
        assert!(saved.include_channels, "includeChannels preserved");
        assert_eq!(saved.retention_days, 30, "retentionDays preserved");
        Ok(())
    }

    #[test]
    fn merge_auto_backup_keeps_current_when_input_none() {
        let current = AutoBackupSettings {
            enabled: true,
            frequency: BackupFrequency::Monthly,
            data_storage_id: 9,
            retention_days: 14,
            ..AutoBackupSettings::default()
        };
        let merged =
            merge_auto_backup_settings(current.clone(), &UpdateAutoBackupSettingsInput::default());
        assert_eq!(merged, current);
    }

    // ---- resolver: onboarding completions ----

    #[tokio::test]
    async fn complete_onboarding_mutations_forward_to_service() -> Result<(), TestError> {
        let fake = FakeSystemSettingsExtService::default();
        let model_calls = Arc::clone(&fake.model_onboarding_calls);
        let auto_calls = Arc::clone(&fake.auto_disable_onboarding_calls);
        let schema = schema_with(fake);

        let resp = schema
            .execute(r#"mutation { completeSystemModelSettingOnboarding(input: {}) }"#)
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);

        let resp = schema
            .execute(r#"mutation { completeAutoDisableChannelOnboarding(input: {}) }"#)
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);

        assert_eq!(*lock(&model_calls), 1);
        assert_eq!(*lock(&auto_calls), 1);
        Ok(())
    }

    // ---- service-unavailable fallback ----

    #[tokio::test]
    async fn resolvers_surface_service_unavailable_when_unwired() {
        let schema: TestSchema = test_schema_builder().finish();
        let resp = schema.execute("{ videoStorageSettings { enabled } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("system service is not available"),
            "unexpected msg: {msg}"
        );
    }
}
