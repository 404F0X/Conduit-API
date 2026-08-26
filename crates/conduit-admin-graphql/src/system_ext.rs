//! GAP-D — system-settings GraphQL slice (channel settings, pass-through,
//! scopes).
//!
//! Ports the remaining `extend type Query` / `extend type Mutation` operations
//! from `conduit/internal/server/gql/system.graphql` +
//! `scopes.graphql` that the frontend settings page depends on and that were
//! still missing on the Rust side (see `.codex/WIRING_TODO.md` GAP-01):
//!
//! | kind     | field                          | Go resolver |
//! |----------|--------------------------------|-------------|
//! | Query    | `systemChannelSettings`        | `system.resolvers.go:503-510` |
//! | Mutation | `updateSystemChannelSettings`  | `system.resolvers.go:143-158` |
//! | Query    | `passThroughSettings`          | `system.resolvers.go:555-563` |
//! | Mutation | `updatePassThroughSettings`    | `system.resolvers.go:317-324` |
//! | Query    | `allScopes(level: String)`     | `scopes.resolvers.go:15-40` |
//!
//! The Rust SDL must match the captured snapshot at
//! `tests/contracts/admin_graphql_schema.graphql`.
//!
//! ## Enum reuse
//!
//! `ChannelModelAutoSyncSetting.frequency` uses the GraphQL enum
//! [`crate::AutoSyncFrequency`] (already registered by the schema). The probe
//! frequency reuses a fresh [`ProbeFrequency`] GraphQL enum defined here (the
//! `scalars::ProbeFrequency` helper is NOT an async-graphql `Enum`, so there is
//! no GraphQL type-name clash).
//!
//! ## Service wiring
//!
//! `systemChannelSettings` + `passThroughSettings` (both read + write) are
//! backed by the host-injected [`SystemChannelServices`] trait (mirrors the
//! `SystemStatusServices` / `SystemSettingsServices` DI pattern); an unwired
//! schema surfaces the Go-equivalent "system service is not available" message.
//!
//! `allScopes` is a **pure** resolver: Go's `scopes.AllScopes` reads a static
//! catalog (`internal/scopes/scopes.go`) with no service dependency. The Rust
//! port adds the Rust-only `system:admin` automation scope to that catalog;
//! there is no unwired-fallback branch.

use std::sync::Arc;

use async_graphql::{Context, Enum, InputObject, SimpleObject};

use crate::AutoSyncFrequency;

// ===========================================================================
// Enums
// ===========================================================================

/// GraphQL `ProbeFrequency` (snapshot lines 9593-9598). Mirrors Go
/// `biz.ProbeFrequency` (`system.go:509-517`). async-graphql renders each
/// variant in SCREAMING_SNAKE (`OneMinute` → `ONE_MINUTE`), matching the
/// snapshot literals exactly, so no explicit `#[graphql(name = …)]` is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum, Default)]
#[graphql(name = "ProbeFrequency")]
pub enum ProbeFrequency {
    #[default]
    OneMinute,
    FiveMinutes,
    ThirtyMinutes,
    OneHour,
}

// ===========================================================================
// Output types
// ===========================================================================

/// GraphQL `ChannelProbeSetting` (snapshot lines 9606-9609). Mirrors Go
/// `biz.ChannelProbeSetting` (`system.go:520-524`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, SimpleObject)]
#[graphql(name = "ChannelProbeSetting")]
pub struct ChannelProbeSetting {
    pub enabled: bool,
    pub frequency: ProbeFrequency,
}

/// GraphQL `ChannelModelAutoSyncSetting` (snapshot lines 9611-9613). Mirrors Go
/// `biz.ChannelModelAutoSyncSetting` (`system.go:437-439`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ChannelModelAutoSyncSetting")]
pub struct ChannelModelAutoSyncSetting {
    pub frequency: AutoSyncFrequency,
}

impl Default for ChannelModelAutoSyncSetting {
    /// Go zero-value normalization: `AutoSyncFrequency` marshals `""` and every
    /// other unknown value to `ONE_HOUR` (`system.go:449-463`), so the default
    /// sync frequency is one hour.
    fn default() -> Self {
        Self {
            frequency: AutoSyncFrequency::OneHour,
        }
    }
}

/// GraphQL `SystemChannelSettings` (snapshot lines 9615-9618). Mirrors Go
/// `biz.SystemChannelSettings` (`system.go:432-435`). Both sub-objects are
/// non-null.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, SimpleObject)]
#[graphql(name = "SystemChannelSettings")]
pub struct SystemChannelSettings {
    pub probe: ChannelProbeSetting,
    pub auto_sync: ChannelModelAutoSyncSetting,
}

/// GraphQL `PassThroughSettings` (snapshot lines 9665-9667). Mirrors the Go
/// gqlgen model `PassThroughSettings` (single non-null `enabled` boolean; the
/// resolver wraps `systemService.PassThrough`, `system.resolvers.go:555-563`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, SimpleObject)]
#[graphql(name = "PassThroughSettings")]
pub struct PassThroughSettings {
    pub enabled: bool,
}

/// GraphQL `ScopeInfo` (snapshot lines 9331-9335). Mirrors the Go gqlgen model
/// `ScopeInfo` built by `AllScopes` (`scopes.resolvers.go:31-39`) from
/// `scopes.Scope` (`internal/scopes/scopes.go:73-77`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ScopeInfo")]
pub struct ScopeInfo {
    pub scope: String,
    pub description: String,
    pub levels: Vec<String>,
}

// ===========================================================================
// Input types
// ===========================================================================

/// GraphQL `UpdateChannelProbeSettingInput` (snapshot lines 9620-9623). Both
/// fields non-null (mirrors the gqlgen binding on `biz.ChannelProbeSetting`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateChannelProbeSettingInput")]
pub struct UpdateChannelProbeSettingInput {
    pub enabled: bool,
    pub frequency: ProbeFrequency,
}

impl From<UpdateChannelProbeSettingInput> for ChannelProbeSetting {
    fn from(input: UpdateChannelProbeSettingInput) -> Self {
        Self {
            enabled: input.enabled,
            frequency: input.frequency,
        }
    }
}

/// GraphQL `UpdateChannelModelAutoSyncSettingInput` (snapshot lines 9625-9627).
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateChannelModelAutoSyncSettingInput")]
pub struct UpdateChannelModelAutoSyncSettingInput {
    pub frequency: AutoSyncFrequency,
}

impl From<UpdateChannelModelAutoSyncSettingInput> for ChannelModelAutoSyncSetting {
    fn from(input: UpdateChannelModelAutoSyncSettingInput) -> Self {
        Self {
            frequency: input.frequency,
        }
    }
}

/// GraphQL `UpdateSystemChannelSettingsInput` (snapshot lines 9629-9632). Both
/// sub-inputs nullable: the Go resolver merges each provided sub-object over the
/// current setting, leaving the other untouched (`system.resolvers.go:144-152`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, InputObject)]
#[graphql(name = "UpdateSystemChannelSettingsInput")]
pub struct UpdateSystemChannelSettingsInput {
    pub probe: Option<UpdateChannelProbeSettingInput>,
    pub auto_sync: Option<UpdateChannelModelAutoSyncSettingInput>,
}

/// GraphQL `UpdatePassThroughSettingsInput` (snapshot lines 9669-9671).
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdatePassThroughSettingsInput")]
pub struct UpdatePassThroughSettingsInput {
    pub enabled: bool,
}

// ===========================================================================
// Pure helpers
// ===========================================================================

/// Merge an `UpdateSystemChannelSettingsInput` over the current settings,
/// mirroring Go `UpdateSystemChannelSettings` (`system.resolvers.go:144-152`):
///
/// ```text
/// setting := *systemService.ChannelSettingOrDefault(ctx)
/// if input.Probe.Frequency != "" { setting.Probe = input.Probe }
/// if input.AutoSync.Frequency != "" { setting.AutoSync = input.AutoSync }
/// ```
///
/// At the GraphQL layer, a provided sub-object always carries a non-empty
/// (required) frequency, so "sub-object present" is the Rust equivalent of Go's
/// `Frequency != ""` guard: a `Some` sub-input replaces that half; a `None`
/// preserves the current half.
pub fn merge_channel_settings(
    current: SystemChannelSettings,
    input: UpdateSystemChannelSettingsInput,
) -> SystemChannelSettings {
    SystemChannelSettings {
        probe: input
            .probe
            .map(ChannelProbeSetting::from)
            .unwrap_or(current.probe),
        auto_sync: input
            .auto_sync
            .map(ChannelModelAutoSyncSetting::from)
            .unwrap_or(current.auto_sync),
    }
}

/// Port of Go `scopes.AllScopes(level)` (`internal/scopes/scopes.go:99-192`):
/// returns the static scope catalog, optionally filtered to entries that list
/// the given level. `level` is a raw string (`"system"` / `"project"`); an
/// unknown level yields an empty list, matching Go's `slices.Contains` filter.
///
/// The original entries mirror Go's `scopeConfigs`; Rust-only commercial
/// resources extend that catalog with system-level scopes.
pub fn all_scopes(level: Option<&str>) -> Vec<ScopeInfo> {
    const SYSTEM: &str = "system";
    const PROJECT: &str = "project";

    // (slug, description, levels) — Go catalog followed by Rust extensions.
    let catalog: &[(&str, &str, &[&str])] = &[
        ("read_dashboard", "View dashboard", &[SYSTEM]),
        ("read_settings", "View system settings", &[SYSTEM]),
        ("write_settings", "Manage system settings", &[SYSTEM]),
        ("read_channels", "View channel information", &[SYSTEM]),
        (
            "write_channels",
            "Manage channels/models (create, edit, delete)",
            &[SYSTEM],
        ),
        (
            "read_data_storages",
            "View data storage information",
            &[SYSTEM],
        ),
        (
            "write_data_storages",
            "Manage data storages (create, edit, delete)",
            &[SYSTEM],
        ),
        ("read_users", "View user information", &[SYSTEM, PROJECT]),
        (
            "write_users",
            "Manage users (create, edit, delete)",
            &[SYSTEM, PROJECT],
        ),
        ("read_roles", "View role information", &[SYSTEM, PROJECT]),
        (
            "write_roles",
            "Manage roles (create, edit, delete)",
            &[SYSTEM, PROJECT],
        ),
        ("read_projects", "View project information", &[SYSTEM]),
        (
            "write_projects",
            "Manage projects (create, edit, delete)",
            &[SYSTEM],
        ),
        ("read_api_keys", "View API keys", &[SYSTEM, PROJECT]),
        (
            "write_api_keys",
            "Manage API keys (create, edit, delete)",
            &[SYSTEM, PROJECT],
        ),
        ("read_requests", "View request records", &[SYSTEM, PROJECT]),
        (
            "write_requests",
            "Manage request records",
            &[SYSTEM, PROJECT],
        ),
        ("read_prompts", "View prompts", &[SYSTEM, PROJECT]),
        (
            "write_prompts",
            "Manage prompts (create, edit, delete)",
            &[SYSTEM, PROJECT],
        ),
        ("read_groups", "View groups and group policies", &[SYSTEM]),
        (
            "write_groups",
            "Manage groups and group policies",
            &[SYSTEM],
        ),
        (
            "read_subscriptions",
            "View subscription plans and assignments",
            &[SYSTEM],
        ),
        (
            "write_subscriptions",
            "Manage subscription plans and assignments",
            &[SYSTEM],
        ),
        (
            "read_billing",
            "View customer and project balances",
            &[SYSTEM],
        ),
        ("write_billing", "Manage billing accounts", &[SYSTEM]),
        (
            "grant_credit",
            "Grant customer or project credit",
            &[SYSTEM],
        ),
        (
            "read_commercialization",
            "View pricing and commercialization policies",
            &[SYSTEM],
        ),
        (
            "write_commercialization",
            "Manage pricing and commercialization policies",
            &[SYSTEM],
        ),
        // Rust extension: grants a service account access to the separate
        // `/internal/v1/graphql` automation boundary. Mutation resolvers only
        // allow an owner principal to grant this scope.
        (
            conduit_auth::scopes::slug::SYSTEM_ADMIN,
            "Full system automation through the internal administrator API",
            &[SYSTEM],
        ),
    ];

    catalog
        .iter()
        .filter(|(_, _, levels)| match level {
            Some(wanted) => levels.contains(&wanted),
            None => true,
        })
        .map(|(slug, description, levels)| ScopeInfo {
            scope: (*slug).to_string(),
            description: (*description).to_string(),
            levels: levels.iter().map(|l| (*l).to_string()).collect(),
        })
        .collect()
}

// ===========================================================================
// Service trait (host-injected)
// ===========================================================================

/// Error surface for the channel/pass-through settings resolvers. Messages
/// mirror the Go `fmt.Errorf("...: %w")` prefixes so the frontend error
/// handling stays stable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SystemChannelError {
    #[error("system service is not available")]
    ServiceUnavailable,
    #[error("failed to get channel setting: {0}")]
    ChannelSetting(String),
    #[error("failed to update channel setting: {0}")]
    UpdateChannelSetting(String),
    #[error("failed to get pass-through settings: {0}")]
    PassThrough(String),
    #[error("failed to update pass-through settings: {0}")]
    UpdatePassThrough(String),
}

/// Trait the host wires to back the channel-settings + pass-through resolvers.
/// Each method corresponds to one Go resolver branch on `systemService`.
#[async_trait::async_trait]
pub trait SystemChannelServices: Send + Sync {
    /// Mirrors Go `systemService.ChannelSetting` used by the query resolver
    /// (`system.resolvers.go:503-510`): read the persisted setting (error on
    /// failure).
    async fn channel_setting(&self) -> Result<SystemChannelSettings, SystemChannelError>;

    /// Mirrors Go `systemService.ChannelSettingOrDefault` used by the mutation
    /// resolver (`system.resolvers.go:144`): read the persisted setting or the
    /// service default. The resolver reads this, merges the input, then writes.
    async fn channel_setting_or_default(&self)
    -> Result<SystemChannelSettings, SystemChannelError>;

    /// Mirrors Go `systemService.SetChannelSetting`
    /// (`system.resolvers.go:154`): persist the merged setting.
    async fn set_channel_setting(
        &self,
        settings: SystemChannelSettings,
    ) -> Result<(), SystemChannelError>;

    /// Mirrors Go `systemService.PassThrough` (`system.resolvers.go:556`):
    /// read the persisted boolean.
    async fn pass_through(&self) -> Result<bool, SystemChannelError>;

    /// Mirrors Go `systemService.SetPassThrough` (`system.resolvers.go:318`):
    /// persist the boolean.
    async fn set_pass_through(&self, enabled: bool) -> Result<(), SystemChannelError>;
}

/// Resolves the injected [`SystemChannelServices`] from the async-graphql
/// context data bag, surfacing the Go-equivalent "service unavailable" message
/// when no service was wired.
pub(crate) fn system_channel_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn SystemChannelServices>, String> {
    match ctx.data::<Arc<dyn SystemChannelServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(SystemChannelError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Resolver wiring (for the coordinator).
//
// IMPORTANT: async-graphql's `#[Object]` macro generates the resolver trait
// impls for the root type, so a root's `#[Object] impl` block CANNOT be split
// across modules (two blocks on the same type → E0119). This slice therefore
// does NOT contribute its own `#[Object] impl QueryRoot` / `impl MutationRoot`;
// instead it exposes the typed service-lookup helper [`system_channel_services`]
// + the pure [`all_scopes`] catalog + the types, and the coordinator pastes the
// resolver methods into the single `#[Object] impl QueryRoot` in `lib.rs` and
// the single `#[Object] impl MutationRoot` in `mutation.rs`. The
// `TestQueryRoot` / `TestMutationRoot` in the test module below are
// byte-for-byte reference implementations.
//
// Query methods (paste into `#[Object] impl QueryRoot` in `lib.rs`):
//
// ```ignore
// /// Mirrors Go `Query.systemChannelSettings` (system.resolvers.go:503-510).
// async fn system_channel_settings(
//     &self,
//     ctx: &Context<'_>,
// ) -> Result<crate::system_ext::SystemChannelSettings, String> {
//     let s = crate::system_ext::system_channel_services(ctx)?;
//     s.channel_setting().await.map_err(|e| e.to_string())
// }
//
// /// Mirrors Go `Query.passThroughSettings` (system.resolvers.go:555-563).
// async fn pass_through_settings(
//     &self,
//     ctx: &Context<'_>,
// ) -> Result<crate::system_ext::PassThroughSettings, String> {
//     let s = crate::system_ext::system_channel_services(ctx)?;
//     let enabled = s.pass_through().await.map_err(|e| e.to_string())?;
//     Ok(crate::system_ext::PassThroughSettings { enabled })
// }
//
// /// Mirrors Go `Query.allScopes` (scopes.resolvers.go:15-40) — pure catalog,
// /// no service dependency.
// async fn all_scopes(
//     &self,
//     _ctx: &Context<'_>,
//     level: Option<String>,
// ) -> Vec<crate::system_ext::ScopeInfo> {
//     crate::system_ext::all_scopes(level.as_deref())
// }
// ```
//
// Mutation methods (paste into `#[Object] impl MutationRoot` in `mutation.rs`;
// both return `true` on success, mirroring the Go resolvers that return
// `false, err` on failure and `true` otherwise):
//
// ```ignore
// /// Mirrors Go `Mutation.updateSystemChannelSettings`
// /// (system.resolvers.go:143-158): read current-or-default, merge each
// /// provided sub-object, persist.
// async fn update_system_channel_settings(
//     &self,
//     ctx: &Context<'_>,
//     input: crate::system_ext::UpdateSystemChannelSettingsInput,
// ) -> Result<bool, String> {
//     let s = crate::system_ext::system_channel_services(ctx)?;
//     let current = s.channel_setting_or_default().await.map_err(|e| e.to_string())?;
//     let merged = crate::system_ext::merge_channel_settings(current, input);
//     s.set_channel_setting(merged).await.map_err(|e| e.to_string())?;
//     Ok(true)
// }
//
// /// Mirrors Go `Mutation.updatePassThroughSettings`
// /// (system.resolvers.go:317-324).
// async fn update_pass_through_settings(
//     &self,
//     ctx: &Context<'_>,
//     input: crate::system_ext::UpdatePassThroughSettingsInput,
// ) -> Result<bool, String> {
//     let s = crate::system_ext::system_channel_services(ctx)?;
//     s.set_pass_through(input.enabled).await.map_err(|e| e.to_string())?;
//     Ok(true)
// }
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

    // ---------------------------------------------------------------------
    // In-memory fake service.
    // ---------------------------------------------------------------------

    #[derive(Default)]
    struct FakeSystemChannelServices {
        channel_setting: Arc<Mutex<SystemChannelSettings>>,
        channel_setting_read_error: Option<SystemChannelError>,
        channel_setting_or_default_error: Option<SystemChannelError>,
        set_channel_setting_calls: Arc<Mutex<Vec<SystemChannelSettings>>>,
        set_channel_setting_error: Option<SystemChannelError>,
        pass_through: Arc<Mutex<bool>>,
        pass_through_read_error: Option<SystemChannelError>,
        set_pass_through_calls: Arc<Mutex<Vec<bool>>>,
        set_pass_through_error: Option<SystemChannelError>,
    }

    #[async_trait::async_trait]
    impl SystemChannelServices for FakeSystemChannelServices {
        async fn channel_setting(&self) -> Result<SystemChannelSettings, SystemChannelError> {
            match &self.channel_setting_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(*lock(&self.channel_setting)),
            }
        }

        async fn channel_setting_or_default(
            &self,
        ) -> Result<SystemChannelSettings, SystemChannelError> {
            match &self.channel_setting_or_default_error {
                Some(err) => Err(err.clone()),
                None => Ok(*lock(&self.channel_setting)),
            }
        }

        async fn set_channel_setting(
            &self,
            settings: SystemChannelSettings,
        ) -> Result<(), SystemChannelError> {
            lock(&self.set_channel_setting_calls).push(settings);
            *lock(&self.channel_setting) = settings;
            match &self.set_channel_setting_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn pass_through(&self) -> Result<bool, SystemChannelError> {
            match &self.pass_through_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(*lock(&self.pass_through)),
            }
        }

        async fn set_pass_through(&self, enabled: bool) -> Result<(), SystemChannelError> {
            lock(&self.set_pass_through_calls).push(enabled);
            *lock(&self.pass_through) = enabled;
            match &self.set_pass_through_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }
    }

    // ---------------------------------------------------------------------
    // Reference roots. `#[Object]` cannot be split across modules, so these
    // mirror the resolver bodies the coordinator pastes into the real
    // `impl QueryRoot` (lib.rs) / `impl MutationRoot` (mutation.rs).
    // ---------------------------------------------------------------------

    struct TestQueryRoot;

    #[Object]
    impl TestQueryRoot {
        async fn system_channel_settings(
            &self,
            ctx: &Context<'_>,
        ) -> Result<SystemChannelSettings, String> {
            let s = system_channel_services(ctx)?;
            s.channel_setting().await.map_err(|e| e.to_string())
        }

        async fn pass_through_settings(
            &self,
            ctx: &Context<'_>,
        ) -> Result<PassThroughSettings, String> {
            let s = system_channel_services(ctx)?;
            let enabled = s.pass_through().await.map_err(|e| e.to_string())?;
            Ok(PassThroughSettings { enabled })
        }

        async fn all_scopes(&self, _ctx: &Context<'_>, level: Option<String>) -> Vec<ScopeInfo> {
            all_scopes(level.as_deref())
        }
    }

    struct TestMutationRoot;

    #[Object]
    impl TestMutationRoot {
        async fn update_system_channel_settings(
            &self,
            ctx: &Context<'_>,
            input: UpdateSystemChannelSettingsInput,
        ) -> Result<bool, String> {
            let s = system_channel_services(ctx)?;
            let current = s
                .channel_setting_or_default()
                .await
                .map_err(|e| e.to_string())?;
            let merged = merge_channel_settings(current, input);
            s.set_channel_setting(merged)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }

        async fn update_pass_through_settings(
            &self,
            ctx: &Context<'_>,
            input: UpdatePassThroughSettingsInput,
        ) -> Result<bool, String> {
            let s = system_channel_services(ctx)?;
            s.set_pass_through(input.enabled)
                .await
                .map_err(|e| e.to_string())?;
            Ok(true)
        }
    }

    type TestSchema = Schema<TestQueryRoot, TestMutationRoot, EmptySubscription>;

    fn test_schema_builder() -> SchemaBuilder<TestQueryRoot, TestMutationRoot, EmptySubscription> {
        Schema::build(TestQueryRoot, TestMutationRoot, EmptySubscription)
    }

    fn schema_with(services: FakeSystemChannelServices) -> TestSchema {
        let arc: Arc<dyn SystemChannelServices> = Arc::new(services);
        test_schema_builder().data(arc).finish()
    }

    // ---- SDL shape parity -------------------------------------------

    #[test]
    fn sdl_contains_slice_types_and_signatures() {
        let arc: Arc<dyn SystemChannelServices> = Arc::new(FakeSystemChannelServices::default());
        let sdl = test_schema_builder().data(arc).finish().sdl();

        for expected in [
            "type SystemChannelSettings {",
            "type ChannelProbeSetting {",
            "type ChannelModelAutoSyncSetting {",
            "type PassThroughSettings {",
            "type ScopeInfo {",
            "enum ProbeFrequency {",
            "enum AutoSyncFrequency {",
            "input UpdateSystemChannelSettingsInput {",
            "input UpdateChannelProbeSettingInput {",
            "input UpdateChannelModelAutoSyncSettingInput {",
            "input UpdatePassThroughSettingsInput {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        // Queries.
        for expected in [
            "systemChannelSettings: SystemChannelSettings!",
            "passThroughSettings: PassThroughSettings!",
        ] {
            assert!(
                sdl.contains(expected),
                "SDL missing query {expected}:\n{sdl}"
            );
        }
        assert!(
            sdl.contains("allScopes(level: String): [ScopeInfo!]!"),
            "SDL missing allScopes query:\n{sdl}"
        );

        // Mutations.
        for expected in [
            "updateSystemChannelSettings(input: UpdateSystemChannelSettingsInput!): Boolean!",
            "updatePassThroughSettings(input: UpdatePassThroughSettingsInput!): Boolean!",
        ] {
            assert!(
                sdl.contains(expected),
                "SDL missing mutation {expected}:\n{sdl}"
            );
        }

        // Enum literals (SCREAMING_SNAKE parity).
        for literal in [
            "ONE_MINUTE",
            "FIVE_MINUTES",
            "THIRTY_MINUTES",
            "ONE_HOUR",
            "SIX_HOURS",
            "ONE_DAY",
        ] {
            assert!(
                sdl.contains(literal),
                "SDL missing enum literal {literal}:\n{sdl}"
            );
        }

        // camelCase field rename for the acronym-free `autoSync`.
        assert!(
            sdl.contains("autoSync: ChannelModelAutoSyncSetting!"),
            "SDL missing autoSync field:\n{sdl}"
        );
    }

    #[test]
    fn sdl_matches_snapshot_for_slice() -> Result<(), Box<dyn std::error::Error>> {
        let arc: Arc<dyn SystemChannelServices> = Arc::new(FakeSystemChannelServices::default());
        let sdl = test_schema_builder().data(arc).finish().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;

        for header in [
            "type SystemChannelSettings",
            "type ChannelProbeSetting",
            "type ChannelModelAutoSyncSetting",
            "type PassThroughSettings",
            "type ScopeInfo",
            "input UpdateSystemChannelSettingsInput",
            "input UpdateChannelProbeSettingInput",
            "input UpdateChannelModelAutoSyncSettingInput",
            "input UpdatePassThroughSettingsInput",
        ] {
            crate::sdl_parity::assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }

        Ok(())
    }

    // ---- pure merge logic --------------------------------------------

    /// Mirrors Go `TestMutationResolver_UpdateSystemChannelSettings_
    /// MergesAutoSyncWithoutOverwritingProbe` (system.resolvers_test.go:32-60).
    #[test]
    fn merge_replaces_auto_sync_only_when_probe_absent() {
        let current = SystemChannelSettings {
            probe: ChannelProbeSetting {
                enabled: true,
                frequency: ProbeFrequency::FiveMinutes,
            },
            auto_sync: ChannelModelAutoSyncSetting {
                frequency: AutoSyncFrequency::OneHour,
            },
        };
        let input = UpdateSystemChannelSettingsInput {
            probe: None,
            auto_sync: Some(UpdateChannelModelAutoSyncSettingInput {
                frequency: AutoSyncFrequency::SixHours,
            }),
        };
        let merged = merge_channel_settings(current, input);
        assert!(merged.probe.enabled);
        assert_eq!(merged.probe.frequency, ProbeFrequency::FiveMinutes);
        assert_eq!(merged.auto_sync.frequency, AutoSyncFrequency::SixHours);
    }

    /// Mirrors Go `TestMutationResolver_UpdateSystemChannelSettings_
    /// MergesProbeWithoutOverwritingAutoSync` (system.resolvers_test.go:62-90).
    #[test]
    fn merge_replaces_probe_only_when_auto_sync_absent() {
        let current = SystemChannelSettings {
            probe: ChannelProbeSetting {
                enabled: true,
                frequency: ProbeFrequency::FiveMinutes,
            },
            auto_sync: ChannelModelAutoSyncSetting {
                frequency: AutoSyncFrequency::SixHours,
            },
        };
        let input = UpdateSystemChannelSettingsInput {
            probe: Some(UpdateChannelProbeSettingInput {
                enabled: false,
                frequency: ProbeFrequency::OneHour,
            }),
            auto_sync: None,
        };
        let merged = merge_channel_settings(current, input);
        assert!(!merged.probe.enabled);
        assert_eq!(merged.probe.frequency, ProbeFrequency::OneHour);
        assert_eq!(merged.auto_sync.frequency, AutoSyncFrequency::SixHours);
    }

    // ---- pure all_scopes catalog -------------------------------------

    #[test]
    fn all_scopes_returns_full_catalog_when_unfiltered() {
        let scopes = all_scopes(None);
        assert_eq!(
            scopes.len(),
            29,
            "19 Go scopes, 9 commercialization scopes, and the internal-admin scope"
        );
        // First entry mirrors Go's declaration order.
        assert_eq!(scopes[0].scope, "read_dashboard");
        assert_eq!(scopes[0].description, "View dashboard");
        assert_eq!(scopes[0].levels, vec!["system".to_string()]);
        assert!(
            scopes
                .iter()
                .any(|scope| scope.scope == conduit_auth::scopes::slug::SYSTEM_ADMIN)
        );
    }

    #[test]
    fn all_scopes_filters_by_project_level() {
        let scopes = all_scopes(Some("project"));
        // Only the dual-level scopes carry "project".
        assert!(
            scopes
                .iter()
                .all(|s| s.levels.contains(&"project".to_string()))
        );
        assert!(scopes.iter().any(|s| s.scope == "read_users"));
        assert!(scopes.iter().all(|s| s.scope != "read_dashboard"));
        assert!(scopes.iter().all(|s| s.scope != "read_groups"));
        assert!(scopes.iter().all(|s| s.scope != "read_billing"));
    }

    #[test]
    fn all_scopes_project_levels_match_shared_role_validation() {
        for scope in all_scopes(None) {
            assert!(
                conduit_auth::scopes::is_known_scope_slug(&scope.scope),
                "catalog contains an unknown role scope: {}",
                scope.scope
            );
            assert_eq!(
                scope.levels.iter().any(|level| level == "project"),
                conduit_auth::scopes::supports_project_role(&scope.scope),
                "catalog and project-role validator disagree for {}",
                scope.scope
            );
        }
    }

    #[test]
    fn all_scopes_unknown_level_is_empty() {
        assert!(all_scopes(Some("galaxy")).is_empty());
    }

    // ---- resolver: systemChannelSettings query ----------------------

    #[tokio::test]
    async fn system_channel_settings_returns_probe_and_auto_sync() {
        let fake = FakeSystemChannelServices {
            channel_setting: Arc::new(Mutex::new(SystemChannelSettings {
                probe: ChannelProbeSetting {
                    enabled: true,
                    frequency: ProbeFrequency::ThirtyMinutes,
                },
                auto_sync: ChannelModelAutoSyncSetting {
                    frequency: AutoSyncFrequency::OneDay,
                },
            })),
            ..FakeSystemChannelServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute(
                "{ systemChannelSettings { probe { enabled frequency } autoSync { frequency } } }",
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("enabled: true"), "probe.enabled missing: {s}");
        assert!(
            s.contains("frequency: THIRTY_MINUTES"),
            "probe.frequency missing: {s}"
        );
        assert!(
            s.contains("frequency: ONE_DAY"),
            "autoSync.frequency missing: {s}"
        );
    }

    #[tokio::test]
    async fn system_channel_settings_surfaces_read_error() {
        let fake = FakeSystemChannelServices {
            channel_setting_read_error: Some(SystemChannelError::ChannelSetting("db down".into())),
            ..FakeSystemChannelServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute("{ systemChannelSettings { probe { enabled } } }")
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to get channel setting"), "msg: {msg}");
        assert!(msg.contains("db down"), "msg: {msg}");
    }

    // ---- resolver: updateSystemChannelSettings mutation -------------

    #[tokio::test]
    async fn update_system_channel_settings_merges_and_persists() {
        let fake = FakeSystemChannelServices {
            channel_setting: Arc::new(Mutex::new(SystemChannelSettings {
                probe: ChannelProbeSetting {
                    enabled: true,
                    frequency: ProbeFrequency::FiveMinutes,
                },
                auto_sync: ChannelModelAutoSyncSetting {
                    frequency: AutoSyncFrequency::OneHour,
                },
            })),
            ..FakeSystemChannelServices::default()
        };
        let set_calls = Arc::clone(&fake.set_channel_setting_calls);
        let schema = schema_with(fake);

        let resp = schema
            .execute(
                r#"mutation {
                    updateSystemChannelSettings(input: { autoSync: { frequency: SIX_HOURS } })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("updateSystemChannelSettings")) {
            Some(Value::Boolean(true)) => {}
            other => panic!("expected true, got {other:?}"),
        }
        // Probe preserved, autoSync replaced (Go merge parity).
        let calls = lock(&set_calls);
        assert_eq!(calls.len(), 1);
        assert!(calls[0].probe.enabled);
        assert_eq!(calls[0].probe.frequency, ProbeFrequency::FiveMinutes);
        assert_eq!(calls[0].auto_sync.frequency, AutoSyncFrequency::SixHours);
    }

    #[tokio::test]
    async fn update_system_channel_settings_surfaces_write_error() {
        let fake = FakeSystemChannelServices {
            set_channel_setting_error: Some(SystemChannelError::UpdateChannelSetting(
                "write failed".into(),
            )),
            ..FakeSystemChannelServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute(
                r#"mutation {
                    updateSystemChannelSettings(input: { probe: { enabled: true, frequency: ONE_HOUR } })
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to update channel setting"),
            "msg: {msg}"
        );
    }

    // ---- resolver: passThroughSettings query ------------------------

    #[tokio::test]
    async fn pass_through_settings_returns_enabled() {
        let fake = FakeSystemChannelServices {
            pass_through: Arc::new(Mutex::new(true)),
            ..FakeSystemChannelServices::default()
        };
        let schema = schema_with(fake);

        let resp = schema.execute("{ passThroughSettings { enabled } }").await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("enabled: true"), "enabled missing: {s}");
    }

    // ---- resolver: updatePassThroughSettings mutation ---------------

    #[tokio::test]
    async fn update_pass_through_settings_persists() {
        let fake = FakeSystemChannelServices::default();
        let set_calls = Arc::clone(&fake.set_pass_through_calls);
        let schema = schema_with(fake);

        let resp = schema
            .execute(r#"mutation { updatePassThroughSettings(input: { enabled: true }) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let calls = lock(&set_calls);
        assert_eq!(calls.as_slice(), &[true]);
    }

    // ---- resolver: allScopes query ----------------------------------

    #[tokio::test]
    async fn all_scopes_query_returns_catalog() {
        // No service needed — allScopes is a pure resolver.
        let schema = test_schema_builder().finish();

        let resp = schema
            .execute("{ allScopes { scope description levels } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("allScopes")) {
            Some(Value::List(items)) => assert_eq!(items.len(), 29),
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn all_scopes_query_filters_by_level() {
        let schema = test_schema_builder().finish();

        let resp = schema
            .execute(r#"{ allScopes(level: "project") { scope } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("allScopes")) {
            Some(Value::List(items)) => {
                // 10 dual-level scopes carry "project" (read/write × users,
                // roles, api_keys, requests, prompts — scopes.go:24-59).
                assert_eq!(items.len(), 10);
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    // ---- service-unavailable fallback -------------------------------

    #[tokio::test]
    async fn channel_resolvers_surface_service_unavailable_when_unwired() {
        // Schema with NO SystemChannelServices injected.
        let schema: TestSchema = test_schema_builder().finish();

        let resp = schema
            .execute("{ systemChannelSettings { probe { enabled } } }")
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("system service is not available"),
            "unexpected msg: {msg}"
        );

        let resp = schema.execute("{ passThroughSettings { enabled } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("system service is not available"),
            "unexpected msg: {msg}"
        );
    }
}
