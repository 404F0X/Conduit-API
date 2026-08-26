//! RUST-P12-001 S07 — system-settings GraphQL slice (basics).
//!
//! Ports a cohesive subset of the system-settings GraphQL operations declared
//! in `conduit/internal/server/gql/system.graphql` (Go) and resolved in
//! `conduit/internal/server/gql/system.resolvers.go`. The Rust SDL must match
//! the captured snapshot at `tests/contracts/admin_graphql_schema.graphql`.
//!
//! ## Operations ported this slice (9)
//!
//! Queries:
//!   - `Query.systemVersion: SystemVersion!` — Go resolver `SystemVersion`
//!     (`system.resolvers.go` lines 482-485) returns `build.GetBuildInfo()`.
//!   - `Query.checkForUpdate: VersionCheck!` — Go resolver `CheckForUpdate`
//!     (`system.resolvers.go` lines 487-500) delegates to
//!     `systemService.CheckForUpdate`.
//!   - `Query.proxyPresets: [ProxyPreset!]!` — Go resolver `ProxyPresets`
//!     (`system.resolvers.go` lines 532-540) delegates to
//!     `systemService.ProxyPresets` and masks passwords via `lo.ToSlicePtr`.
//!   - `Query.securitySettings: SecuritySettings!` — Go resolver
//!     `SecuritySettings` (`system.resolvers.go` lines 527-530) delegates to
//!     `systemService.SecuritySettings`.
//!   - `Query.onboardingInfo: OnboardingInfo` — Go resolver `OnboardingInfo`
//!     (`system.resolvers.go` lines 449-480) reads the typed record and maps
//!     each present sub-module to its GraphQL form.
//!
//! Mutations:
//!   - `Mutation.completeOnboarding(input: CompleteOnboardingInput!): Boolean!`
//!     — Go resolver (`system.resolvers.go` lines 112-120).
//!   - `Mutation.updateSecuritySettings(input: UpdateSecuritySettingsInput!):
//!     Boolean!` — Go resolver (`system.resolvers.go` lines 209-234) reads
//!     current settings, applies partial merge (None = keep current), writes.
//!   - `Mutation.saveProxyPreset(input: SaveProxyPresetInput!): Boolean!` —
//!     Go resolver (`system.resolvers.go` lines 286-294).
//!   - `Mutation.deleteProxyPreset(url: String!): Boolean!` — Go resolver
//!     (`system.resolvers.go` lines 296-304).
//!
//! Operations NOT ported this slice (pending — left for a follow-up slice to
//! keep the diff reviewable): brandSettings/updateBrandSettings,
//! storagePolicy/updateStoragePolicy, retryPolicy/updateRetryPolicy,
//! webhookNotifierConfig/updateWebhookNotifierConfig, systemModelSettings,
//! defaultDataStorageID/updateDefaultDataStorage, systemChannelSettings,
//! systemGeneralSettings, videoStorageSettings, userAgentPassThroughSettings,
//! passThroughSettings, completeSystemModelSettingOnboarding,
//! completeAutoDisableChannelOnboarding, GC cleanup (preview/trigger), cache
//! diagnostics/clear, checkProviderQuotas + resetChannelQuotaNow +
//! updateQuotaEnforcementSettings (these three are already ported in
//! `mutation.rs` under the quota-mutation slice).
//!
//! ## Service wiring
//!
//! The admin-graphql crate stays free of DB / HTTP concerns. The host wires a
//! concrete implementation of [`SystemSettingsServices`] into the schema data
//! bag at build time; resolver-level tests inject an in-memory fake (mirrors
//! the dependency-injection pattern used by `channel::ChannelQueryServices`
//! and `mutation::QuotaMutationServices`).

use std::sync::Arc;

use async_graphql::{Context, ID, InputObject, SimpleObject};

use crate::scalars::{DecimalInputScalar, DecimalScalar, TimeScalar};

// ===========================================================================
// Output types — mirrors of the Go GraphQL types in `system.graphql`.
// ===========================================================================

/// Build and runtime information exposed by the administration API.
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "SystemVersion")]
pub struct SystemVersion {
    pub version: String,
    pub commit: String,
    pub build_time: String,
    /// Rust toolchain used to compile the running binary.
    #[graphql(name = "rustVersion")]
    pub rust_version: String,
    pub platform: String,
    pub uptime: String,
}

/// GraphQL `VersionCheck` (snapshot lines 9737-9742).
///
/// Go resolver `CheckForUpdate` (`system.resolvers.go` lines 487-500) maps the
/// service result's four fields onto this shape verbatim.
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "VersionCheck")]
pub struct VersionCheck {
    pub current_version: String,
    pub latest_version: String,
    pub has_update: bool,
    pub release_url: String,
}

/// GraphQL `ProxyPreset` (snapshot lines 9674-9679).
///
/// Mirrors Go `biz.ProxyPreset` (`system_proxy.go` lines 18-23). All fields
/// except `url` are nullable in the GraphQL contract; the `password` field is
/// part of the public GraphQL shape but the host must mask it (the Go service
/// does so in `masked_proxy_presets`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ProxyPreset")]
pub struct ProxyPreset {
    pub name: Option<String>,
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// GraphQL `SaveProxyPresetInput` (snapshot lines 9681-9686).
///
/// All fields mirror `biz.ProxyPreset` — the Go resolver passes the input
/// straight through to `systemService.SaveProxyPreset`.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "SaveProxyPresetInput")]
pub struct SaveProxyPresetInput {
    pub name: Option<String>,
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl From<SaveProxyPresetInput> for ProxyPreset {
    fn from(input: SaveProxyPresetInput) -> Self {
        ProxyPreset {
            name: input.name,
            url: input.url,
            username: input.username,
            password: input.password,
        }
    }
}

/// GraphQL `SecuritySettings` (snapshot lines 9522-9525).
///
/// Mirrors Go `biz.SecuritySettings` (`system.go` lines 211-217). Both fields
/// are non-null in the GraphQL contract; defaults come from
/// `defaultSecuritySettings` when persisted state is absent.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "SecuritySettings")]
pub struct SecuritySettings {
    /// GraphQL field `blockedIPs` — `[String!]!`. The `IPs` acronym needs an
    /// explicit `#[graphql(name = …)]` to avoid `camelCase` rendering it as
    /// `blockedIps` (snapshot line 9523 is `blockedIPs`).
    #[graphql(name = "blockedIPs")]
    pub blocked_ips: Vec<String>,
    #[graphql(name = "showRequestLogIPBanIcon")]
    pub show_request_log_ip_ban_icon: bool,
}

impl Default for SecuritySettings {
    /// Mirrors Go `defaultSecuritySettings` (system_default.go:81-84):
    /// empty blocked list + show-ban-icon = true.
    fn default() -> Self {
        Self {
            blocked_ips: Vec::new(),
            show_request_log_ip_ban_icon: true,
        }
    }
}

/// GraphQL `UpdateSecuritySettingsInput` (snapshot lines 9527-9530).
///
/// Both fields nullable: the Go resolver applies a partial merge — `nil`
/// input fields preserve the current persisted value.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateSecuritySettingsInput")]
pub struct UpdateSecuritySettingsInput {
    #[graphql(name = "blockedIPs")]
    pub blocked_ips: Option<Vec<String>>,
    #[graphql(name = "showRequestLogIPBanIcon")]
    pub show_request_log_ip_ban_icon: Option<bool>,
}

/// GraphQL `SystemModelSettingOnboarding` (snapshot lines 9564-9567).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "SystemModelSettingOnboarding")]
pub struct SystemModelSettingOnboarding {
    pub onboarded: bool,
    pub completed_at: Option<TimeScalar>,
}

/// GraphQL `AutoDisableChannelOnboarding` (snapshot lines 9569-9572).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "AutoDisableChannelOnboarding")]
pub struct AutoDisableChannelOnboarding {
    pub onboarded: bool,
    pub completed_at: Option<TimeScalar>,
}

/// GraphQL `OnboardingInfo` (snapshot lines 9574-9579).
///
/// Go resolver `OnboardingInfo` (`system.resolvers.go` lines 449-480) returns
/// `null` when the service yields `nil`; we surface that as `Option::None` on
/// the resolver's return type.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "OnboardingInfo")]
pub struct OnboardingInfo {
    pub onboarded: bool,
    pub completed_at: Option<TimeScalar>,
    pub system_model_setting: Option<SystemModelSettingOnboarding>,
    pub auto_disable_channel: Option<AutoDisableChannelOnboarding>,
}

/// GraphQL `CompleteOnboardingInput` (snapshot lines 9581-9583).
///
/// The Go schema carries a placeholder `dummy: String` field so the input is
/// non-null at the GraphQL layer even though the resolver ignores it.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "CompleteOnboardingInput")]
pub struct CompleteOnboardingInput {
    pub dummy: Option<String>,
}

// ===========================================================================
// RUST-P12-001 S07 (continuation) — five additional settings domains.
// Each block mirrors a Go GraphQL type in `system.graphql` and the matching
// resolver in `system.resolvers.go`. Field names are taken verbatim from the
// snapshot at `tests/contracts/admin_graphql_schema.graphql`.
// ===========================================================================

/// GraphQL `BrandSettings` (snapshot lines 9361-9365). All three fields are
/// nullable — Go resolver (`system.resolvers.go:383-405`) returns pointers to
/// strings the typed service yielded.
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "BrandSettings")]
pub struct BrandSettings {
    pub brand_name: Option<String>,
    pub brand_logo: Option<String>,
    pub title: Option<String>,
}

/// GraphQL `UpdateBrandSettingsInput` (snapshot lines 9381-9385). Each `None`
/// field is a no-op in the resolver (Go: `if input.X != nil`).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateBrandSettingsInput")]
pub struct UpdateBrandSettingsInput {
    pub brand_name: Option<String>,
    pub brand_logo: Option<String>,
    pub title: Option<String>,
}

/// GraphQL `CleanupOption` (snapshot lines 9375-9379). Mirrors Go
/// `biz.CleanupOption` (`system.go:281-285`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "CleanupOption")]
pub struct CleanupOption {
    pub resource_type: String,
    pub enabled: bool,
    pub cleanup_days: i32,
}

/// GraphQL `CleanupOptionInput` (snapshot lines 9395-9399).
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "CleanupOptionInput")]
pub struct CleanupOptionInput {
    pub resource_type: String,
    pub enabled: bool,
    pub cleanup_days: i32,
}

impl From<CleanupOptionInput> for CleanupOption {
    fn from(input: CleanupOptionInput) -> Self {
        CleanupOption {
            resource_type: input.resource_type,
            enabled: input.enabled,
            cleanup_days: input.cleanup_days,
        }
    }
}

/// GraphQL `StoragePolicy` (snapshot lines 9367-9373). Mirrors Go
/// `biz.StoragePolicy` (`system.go:272-278`); all fields are non-null.
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "StoragePolicy")]
pub struct StoragePolicy {
    pub store_chunks: bool,
    pub live_preview: bool,
    pub store_request_headers: bool,
    pub store_request_body: bool,
    pub store_response_body: bool,
    pub cleanup_options: Vec<CleanupOption>,
}

/// GraphQL `UpdateStoragePolicyInput` (snapshot lines 9387-9393). All fields
/// nullable — Go resolver passes the input struct straight through to
/// `systemService.SetStoragePolicy`, but the GraphQL contract marks every
/// field optional (last-writer-wins at the service layer).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateStoragePolicyInput")]
pub struct UpdateStoragePolicyInput {
    pub store_chunks: Option<bool>,
    pub live_preview: Option<bool>,
    pub store_request_headers: Option<bool>,
    pub store_request_body: Option<bool>,
    pub store_response_body: Option<bool>,
    pub cleanup_options: Option<Vec<CleanupOptionInput>>,
}

impl From<UpdateStoragePolicyInput> for StoragePolicy {
    /// Go resolver (`system.resolvers.go:49-57`) hands the gqlgen-decoded
    /// input struct directly to `systemService.SetStoragePolicy`. The Rust
    /// service takes the typed [`StoragePolicy`]; this conversion therefore
    /// treats absent input fields as their Go defaults
    /// (`defaultStoragePolicy`, system_default.go:3-20): false for the two
    /// store flags the GraphQL caller can omit, true for the two body-store
    /// flags, and `CleanupOption::defaults()` for the option list.
    fn from(input: UpdateStoragePolicyInput) -> Self {
        StoragePolicy {
            store_chunks: input.store_chunks.unwrap_or(false),
            live_preview: input.live_preview.unwrap_or(false),
            store_request_headers: input.store_request_headers.unwrap_or(true),
            store_request_body: input.store_request_body.unwrap_or(true),
            store_response_body: input.store_response_body.unwrap_or(true),
            cleanup_options: input
                .cleanup_options
                .map(|opts| opts.into_iter().map(CleanupOption::from).collect())
                .unwrap_or_default(),
        }
    }
}

/// GraphQL `UpstreamErrorPolicy` (snapshot lines 9474-9477). Mirrors Go
/// `biz.UpstreamErrorPolicy` (`system.go:343-349`).
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "UpstreamErrorPolicy")]
pub struct UpstreamErrorPolicy {
    pub mode: String,
    pub custom_message: String,
}

/// GraphQL `UpstreamErrorPolicyInput` (snapshot lines 9479-9482).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpstreamErrorPolicyInput")]
pub struct UpstreamErrorPolicyInput {
    pub mode: Option<String>,
    pub custom_message: Option<String>,
}

/// GraphQL `AutoDisableChannelStatus` (snapshot lines 9401-9404). Mirrors Go
/// `biz.AutoDisableChannelStatus` (`system.go:359-365`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject)]
#[graphql(name = "AutoDisableChannelStatus")]
pub struct AutoDisableChannelStatus {
    pub status: i32,
    pub times: i32,
}

/// GraphQL `AutoDisableChannelStatusInput` (snapshot lines 9484-9487).
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "AutoDisableChannelStatusInput")]
pub struct AutoDisableChannelStatusInput {
    pub status: i32,
    pub times: i32,
}

/// GraphQL `AutoDisableChannel` (snapshot lines 9406-9409). Mirrors Go
/// `biz.AutoDisableChannel` (`system.go:351-357`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "AutoDisableChannel")]
pub struct AutoDisableChannel {
    pub enabled: bool,
    pub statuses: Vec<AutoDisableChannelStatus>,
}

/// GraphQL `AutoDisableChannelInput` (snapshot lines 9489-9492).
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "AutoDisableChannelInput")]
pub struct AutoDisableChannelInput {
    pub enabled: Option<bool>,
    pub statuses: Option<Vec<AutoDisableChannelStatusInput>>,
}

/// GraphQL `RetryPolicy` (snapshot lines 9461-9472). Mirrors Go
/// `biz.RetryPolicy` (`system.go:309-341`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "RetryPolicy")]
pub struct RetryPolicy {
    pub max_channel_retries: i32,
    pub max_single_channel_retries: i32,
    pub retry_delay_ms: i32,
    pub stream_first_event_timeout_seconds: i32,
    pub non_stream_response_timeout_seconds: i32,
    pub load_balancer_strategy: String,
    /// Relative influence (0..=100) of request-scoped theoretical procurement
    /// cost on routing. Zero disables the cost component.
    pub cost_score_weight: i32,
    pub enabled: bool,
    pub auto_disable_channel: AutoDisableChannel,
    pub empty_response_detection: bool,
    pub upstream_error_policy: UpstreamErrorPolicy,
}

impl Default for RetryPolicy {
    /// Mirrors Go's zero-value `biz.RetryPolicy` after
    /// `defaultRetryPolicy` (`system_default.go:22-31`) normalization where
    /// relevant; the test fake just needs a deterministic non-panicking base.
    fn default() -> Self {
        RetryPolicy {
            max_channel_retries: 0,
            max_single_channel_retries: 0,
            retry_delay_ms: 0,
            stream_first_event_timeout_seconds: 0,
            non_stream_response_timeout_seconds: 0,
            load_balancer_strategy: String::new(),
            cost_score_weight: 0,
            enabled: false,
            auto_disable_channel: AutoDisableChannel {
                enabled: false,
                statuses: Vec::new(),
            },
            empty_response_detection: false,
            upstream_error_policy: UpstreamErrorPolicy::default(),
        }
    }
}

/// GraphQL `UpdateRetryPolicyInput` (snapshot lines 9494-9505). All fields
/// nullable; the Go resolver forwards the gqlgen-decoded struct straight to
/// `systemService.SetRetryPolicy`.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateRetryPolicyInput")]
pub struct UpdateRetryPolicyInput {
    pub max_channel_retries: Option<i32>,
    pub max_single_channel_retries: Option<i32>,
    pub retry_delay_ms: Option<i32>,
    pub stream_first_event_timeout_seconds: Option<i32>,
    pub non_stream_response_timeout_seconds: Option<i32>,
    pub load_balancer_strategy: Option<String>,
    pub cost_score_weight: Option<i32>,
    pub enabled: Option<bool>,
    pub auto_disable_channel: Option<AutoDisableChannelInput>,
    pub empty_response_detection: Option<bool>,
    pub upstream_error_policy: Option<UpstreamErrorPolicyInput>,
}

impl From<UpdateRetryPolicyInput> for RetryPolicy {
    /// Go resolver (`system.resolvers.go:59-67`) forwards the gqlgen input
    /// directly. Service-side Go normalizes/clamps; this conversion only
    /// materializes a [`RetryPolicy`] from the partial input using the same
    /// zero/empty defaults Go would observe on a freshly-allocated struct.
    fn from(input: UpdateRetryPolicyInput) -> Self {
        let auto_disable_channel = input.auto_disable_channel.map(|adc| AutoDisableChannel {
            enabled: adc.enabled.unwrap_or(false),
            statuses: adc
                .statuses
                .map(|ss| {
                    ss.into_iter()
                        .map(|s| AutoDisableChannelStatus {
                            status: s.status,
                            times: s.times,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        });
        let upstream_error_policy = input.upstream_error_policy.map(|uep| UpstreamErrorPolicy {
            mode: uep.mode.unwrap_or_default(),
            custom_message: uep.custom_message.unwrap_or_default(),
        });
        RetryPolicy {
            max_channel_retries: input.max_channel_retries.unwrap_or(0),
            max_single_channel_retries: input.max_single_channel_retries.unwrap_or(0),
            retry_delay_ms: input.retry_delay_ms.unwrap_or(0),
            stream_first_event_timeout_seconds: input
                .stream_first_event_timeout_seconds
                .unwrap_or(0),
            non_stream_response_timeout_seconds: input
                .non_stream_response_timeout_seconds
                .unwrap_or(0),
            load_balancer_strategy: input.load_balancer_strategy.unwrap_or_default(),
            cost_score_weight: input.cost_score_weight.unwrap_or(0).clamp(0, 100),
            enabled: input.enabled.unwrap_or(false),
            auto_disable_channel: auto_disable_channel.unwrap_or_else(|| AutoDisableChannel {
                enabled: false,
                statuses: Vec::new(),
            }),
            empty_response_detection: input.empty_response_detection.unwrap_or(false),
            upstream_error_policy: upstream_error_policy.unwrap_or_default(),
        }
    }
}

/// GraphQL `UserAgentPassThroughSettings` (snapshot lines 9657-9659). Mirrors
/// Go resolver `UserAgentPassThroughSettings` (`system.resolvers.go:543-552`).
#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "UserAgentPassThroughSettings")]
pub struct UserAgentPassThroughSettings {
    pub enabled: bool,
}

/// GraphQL `UpdateUserAgentPassThroughSettingsInput`
/// (snapshot lines 9661-9663).
#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateUserAgentPassThroughSettingsInput")]
pub struct UpdateUserAgentPassThroughSettingsInput {
    pub enabled: bool,
}

/// GraphQL `UpdateDefaultDataStorageInput` (snapshot lines 9560-9562). The
/// `dataStorageID` is an `ID!` carrying a `gid://conduit/DataStorage/<id>`
/// wire form; the Go resolver (`system.resolvers.go:102-110`) extracts
/// `.ID` and forwards to `SetDefaultDataStorageID`.
#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateDefaultDataStorageInput")]
pub struct UpdateDefaultDataStorageInput {
    /// GraphQL field `dataStorageID` — the `ID` acronym needs an explicit
    /// rename, otherwise async-graphql mangles it to `dataStorageId`.
    #[graphql(name = "dataStorageID")]
    pub data_storage_id: ID,
}

// ===========================================================================
// SystemGeneralSettings domain (system.resolvers.go:512-515, 160-167)
// ===========================================================================

/// General settings plus the canonical accounting/credit conversion.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "SystemGeneralSettings")]
pub struct SystemGeneralSettings {
    pub accounting_currency_code: String,
    /// True once any retail or channel-procurement price record exists.
    /// Consumers use this server-derived flag to disable accounting-currency
    /// changes; display-name, credit-ratio and FX settings remain editable.
    pub accounting_currency_locked: bool,
    pub timezone: String,
    pub credit_display_name: String,
    pub credits_per_accounting_unit: DecimalScalar,
    pub exchange_rates: Vec<CurrencyExchangeRate>,
    pub accounting_rate_version: i64,
}

impl Default for SystemGeneralSettings {
    fn default() -> Self {
        Self {
            accounting_currency_code:
                conduit_core::objects::money::DEFAULT_ACCOUNTING_CURRENCY_CODE.into(),
            accounting_currency_locked: false,
            timezone: "UTC".into(),
            credit_display_name: conduit_core::objects::money::DEFAULT_CREDIT_DISPLAY_NAME.into(),
            credits_per_accounting_unit: DecimalScalar(rust_decimal::Decimal::from(10_000)),
            exchange_rates: Vec::new(),
            accounting_rate_version: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct CurrencyExchangeRate {
    pub currency_code: String,
    pub quote_per_accounting_unit: DecimalScalar,
}

#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct CurrencyExchangeRateInput {
    pub currency_code: String,
    pub quote_per_accounting_unit: DecimalInputScalar,
}

/// Optional updates to the canonical accounting, credit-display, FX, and
/// timezone settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateSystemGeneralSettingsInput")]
pub struct UpdateSystemGeneralSettingsInput {
    pub accounting_currency_code: Option<String>,
    pub timezone: Option<String>,
    pub credit_display_name: Option<String>,
    pub credits_per_accounting_unit: Option<DecimalInputScalar>,
    pub exchange_rates: Option<Vec<CurrencyExchangeRateInput>>,
}

impl From<UpdateSystemGeneralSettingsInput> for SystemGeneralSettings {
    /// Absent fields use the canonical CNY, station-credit, and UTC defaults.
    fn from(input: UpdateSystemGeneralSettingsInput) -> Self {
        let defaults = SystemGeneralSettings::default();
        SystemGeneralSettings {
            accounting_currency_code: input
                .accounting_currency_code
                .unwrap_or(defaults.accounting_currency_code),
            accounting_currency_locked: defaults.accounting_currency_locked,
            timezone: input.timezone.unwrap_or(defaults.timezone),
            credit_display_name: input
                .credit_display_name
                .unwrap_or(defaults.credit_display_name),
            credits_per_accounting_unit: DecimalScalar(
                input
                    .credits_per_accounting_unit
                    .map(|value| value.0)
                    .unwrap_or(defaults.credits_per_accounting_unit.0),
            ),
            exchange_rates: input
                .exchange_rates
                .unwrap_or_default()
                .into_iter()
                .map(|rate| CurrencyExchangeRate {
                    currency_code: rate.currency_code,
                    quote_per_accounting_unit: DecimalScalar(rate.quote_per_accounting_unit.0),
                })
                .collect(),
            accounting_rate_version: defaults.accounting_rate_version,
        }
    }
}

// ===========================================================================
// SystemModelSettings domain (system.resolvers.go:422-430, 79-100)
// ===========================================================================

/// GraphQL `DeveloperModelSettings` (snapshot lines 9550-9553). Mirrors Go
/// `biz.DeveloperModelSettings` (`system.go:426-429`): developer name +
/// association rules (reuses [`crate::model::ModelAssociation`]).
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "DeveloperModelSettings")]
pub struct DeveloperModelSettings {
    pub developer: String,
    pub associations: Vec<crate::model::ModelAssociation>,
}

/// GraphQL `DeveloperModelSettingsInput` (snapshot lines 9555-9558).
#[derive(Debug, Clone, PartialEq, InputObject)]
#[graphql(name = "DeveloperModelSettingsInput")]
pub struct DeveloperModelSettingsInput {
    pub developer: String,
    pub associations: Vec<crate::model::ModelAssociationInput>,
}

/// GraphQL `SystemModelSettings` (snapshot lines 9532-9539). Mirrors Go
/// `biz.SystemModelSettings` (`system.go:387-424`).
#[derive(Debug, Clone, Default, PartialEq, SimpleObject)]
#[graphql(name = "SystemModelSettings")]
pub struct SystemModelSettings {
    pub fallback_to_channels_on_model_not_found: bool,
    pub query_all_channel_models: bool,
    #[graphql(name = "defaultModelAPIIncludeAll")]
    pub default_model_api_include_all: bool,
    pub auto_reasoning_effort: bool,
    pub model_blacklist_regex: String,
    pub developer_settings: Vec<DeveloperModelSettings>,
}

/// GraphQL `UpdateSystemModelSettingsInput` (snapshot lines 9541-9548). All
/// fields nullable; the Go resolver preserves `DeveloperSettings` from
/// the current value when `input.DeveloperSettings == nil` (older clients).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
#[graphql(name = "UpdateSystemModelSettingsInput")]
pub struct UpdateSystemModelSettingsInput {
    pub fallback_to_channels_on_model_not_found: Option<bool>,
    pub query_all_channel_models: Option<bool>,
    #[graphql(name = "defaultModelAPIIncludeAll")]
    pub default_model_api_include_all: Option<bool>,
    pub auto_reasoning_effort: Option<bool>,
    pub model_blacklist_regex: Option<String>,
    pub developer_settings: Option<Vec<DeveloperModelSettingsInput>>,
}

impl From<DeveloperModelSettingsInput> for DeveloperModelSettings {
    fn from(input: DeveloperModelSettingsInput) -> Self {
        Self {
            developer: input.developer,
            associations: input.associations.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<UpdateSystemModelSettingsInput> for SystemModelSettings {
    fn from(input: UpdateSystemModelSettingsInput) -> Self {
        Self {
            fallback_to_channels_on_model_not_found: input
                .fallback_to_channels_on_model_not_found
                .unwrap_or(false),
            query_all_channel_models: input.query_all_channel_models.unwrap_or(false),
            default_model_api_include_all: input.default_model_api_include_all.unwrap_or(false),
            auto_reasoning_effort: input.auto_reasoning_effort.unwrap_or(false),
            model_blacklist_regex: input.model_blacklist_regex.unwrap_or_default(),
            developer_settings: input
                .developer_settings
                .map(|v| v.into_iter().map(Into::into).collect())
                .unwrap_or_default(),
        }
    }
}

// ===========================================================================
// DTOs exchanged with the service trait (admin-graphql owns these so the
// trait does not depend on a particular services-crate type).
// ===========================================================================

/// Service-layer representation of the typed onboarding record, mirroring Go
/// `biz.OnboardingRecord` / `OnboardingModule` (`system_onboarding.go`). The
/// GraphQL resolver maps this onto [`OnboardingInfo`] (and sub-modules).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OnboardingRecord {
    pub onboarded: bool,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub system_model_setting: Option<OnboardingModule>,
    pub auto_disable_channel: Option<OnboardingModule>,
}

/// Service-layer representation of one onboarding module.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OnboardingModule {
    pub onboarded: bool,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ===========================================================================
// Service trait (host-injected)
// ===========================================================================

/// Error surface for the system-settings slice. The GraphQL layer surfaces
/// these as field errors; messages mirror the Go `fmt.Errorf("...: %w")`
/// prefixes so frontend error handling stays stable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SystemSettingsError {
    #[error("system service is not available")]
    ServiceUnavailable,
    #[error("failed to get system version: {0}")]
    Version(String),
    #[error("failed to check for update: {0}")]
    CheckUpdate(String),
    #[error("failed to get proxy presets: {0}")]
    ProxyPresets(String),
    #[error("failed to save proxy preset: {0}")]
    SaveProxyPreset(String),
    #[error("failed to delete proxy preset: {0}")]
    DeleteProxyPreset(String),
    #[error("failed to read current security settings: {0}")]
    ReadSecurity(String),
    #[error("failed to update security settings: {0}")]
    UpdateSecurity(String),
    #[error("failed to get onboarding info: {0}")]
    OnboardingInfo(String),
    #[error("failed to complete onboarding: {0}")]
    CompleteOnboarding(String),
    #[error("failed to get brand name: {0}")]
    BrandName(String),
    #[error("failed to get brand logo: {0}")]
    BrandLogo(String),
    #[error("failed to get title: {0}")]
    Title(String),
    #[error("failed to update brand name setting: {0}")]
    UpdateBrandName(String),
    #[error("failed to update brand logo setting: {0}")]
    UpdateBrandLogo(String),
    #[error("failed to update title setting: {0}")]
    UpdateTitle(String),
    #[error("failed to get storage policy: {0}")]
    StoragePolicy(String),
    #[error("failed to update storage policy: {0}")]
    UpdateStoragePolicy(String),
    #[error("failed to get retry policy: {0}")]
    RetryPolicy(String),
    #[error("failed to update retry policy: {0}")]
    UpdateRetryPolicy(String),
    #[error("failed to get user-agent pass-through settings: {0}")]
    UserAgentPassThrough(String),
    #[error("failed to update user-agent pass-through settings: {0}")]
    UpdateUserAgentPassThrough(String),
    #[error("failed to get default data storage ID: {0}")]
    DefaultDataStorageID(String),
    #[error("failed to update default data storage: {0}")]
    UpdateDefaultDataStorage(String),
    #[error("failed to get general settings: {0}")]
    GeneralSettings(String),
    #[error("failed to update general settings: {0}")]
    UpdateGeneralSettings(String),
    #[error("failed to get system model settings: {0}")]
    ModelSettings(String),
    #[error("failed to update system model settings: {0}")]
    UpdateModelSettings(String),
}

/// Trait the host wires to back the system-settings operations. Each method
/// corresponds to one Go resolver branch; the signatures stay synchronous to
/// the resolver layer (the trait itself is async-trait-shaped via
/// async-graphql's `async` resolver methods).
///
/// This is intentionally minimal: it captures only the operations this slice
/// needs. A future task will fold the existing
/// `mutation::QuotaMutationServices` into a wider `SystemService` trait.
#[async_trait::async_trait]
pub trait SystemSettingsServices: Send + Sync {
    /// Mirrors Go resolver `SystemVersion` (system.resolvers.go:482-485):
    /// return the build/version info object. Never fails in Go (the resolver
    /// is a pure read of compile-time data); failures here surface as
    /// [`SystemSettingsError::Version`].
    async fn system_version(&self) -> Result<SystemVersion, SystemSettingsError>;

    /// Mirrors Go resolver `CheckForUpdate` (system.resolvers.go:487-500):
    /// HTTP check against the upstream release source. Returns the four-field
    /// [`VersionCheck`] object.
    async fn check_for_update(&self) -> Result<VersionCheck, SystemSettingsError>;

    /// Mirrors Go resolver `ProxyPresets` (system.resolvers.go:532-540):
    /// return the persisted preset list with passwords already masked (Go
    /// does the masking inside the service / via `ProxyPreset.masked()`).
    async fn proxy_presets(&self) -> Result<Vec<ProxyPreset>, SystemSettingsError>;

    /// Mirrors Go resolver `SecuritySettings` (system.resolvers.go:527-530):
    /// read the persisted settings (or the default on not-found, Go
    /// `defaultSecuritySettings`).
    async fn security_settings(&self) -> Result<SecuritySettings, SystemSettingsError>;

    /// Mirrors Go resolver `OnboardingInfo` (system.resolvers.go:449-480):
    /// return the typed onboarding record, or `None` if the service yields
    /// `nil` (the Go resolver surfaces this as a null GraphQL value).
    async fn onboarding_record(&self) -> Result<Option<OnboardingRecord>, SystemSettingsError>;

    /// Mirrors Go resolver `CompleteOnboarding` (system.resolvers.go:112-120):
    /// mark the system-level onboarding as completed.
    async fn complete_onboarding(&self) -> Result<(), SystemSettingsError>;

    /// Mirrors Go resolver `UpdateSecuritySettings` write half
    /// (system.resolvers.go:228): persist the merged settings. The resolver
    /// performs the read+merge; this method only writes.
    async fn set_security_settings(
        &self,
        settings: SecuritySettings,
    ) -> Result<(), SystemSettingsError>;

    /// Mirrors Go resolver `SaveProxyPreset` (system.resolvers.go:286-294):
    /// upsert the preset keyed by URL.
    async fn save_proxy_preset(&self, preset: ProxyPreset) -> Result<(), SystemSettingsError>;

    /// Mirrors Go resolver `DeleteProxyPreset` (system.resolvers.go:296-304):
    /// remove the preset with the given URL.
    async fn delete_proxy_preset(&self, url: &str) -> Result<(), SystemSettingsError>;

    // ----- brand domain (system.resolvers.go:23-47, 383-405) -----

    /// Mirrors Go `SystemService.BrandName` (system.go:812-826) — read the
    /// persisted brand name string. Used by the [`BrandSettings`] query
    /// resolver.
    async fn brand_name(&self) -> Result<String, SystemSettingsError>;

    /// Mirrors Go `SystemService.BrandLogo` (system.go:834-848) — read the
    /// persisted brand logo (base64-encoded).
    async fn brand_logo(&self) -> Result<String, SystemSettingsError>;

    /// Mirrors Go `SystemService.Title` (used by resolver
    /// `system.resolvers.go:395-398`) — read the persisted browser title.
    async fn title(&self) -> Result<String, SystemSettingsError>;

    /// Mirrors Go `SetBrandName` (system.go:829-831). The resolver only calls
    /// this when `input.brandName` is `Some`.
    async fn set_brand_name(&self, name: &str) -> Result<(), SystemSettingsError>;

    /// Mirrors Go `SetBrandLogo` (system.go:851-853).
    async fn set_brand_logo(&self, logo: &str) -> Result<(), SystemSettingsError>;

    /// Mirrors Go `SetTitle`. The Go service stores under the system_title
    /// key (Rust `system_key::TITLE`).
    async fn set_title(&self, title: &str) -> Result<(), SystemSettingsError>;

    // ----- storagePolicy domain (system.resolvers.go:49-57, 407-410) -----

    /// Mirrors Go resolver `StoragePolicy` (system.resolvers.go:407-410):
    /// return the typed policy or its default.
    async fn storage_policy(&self) -> Result<StoragePolicy, SystemSettingsError>;

    /// Mirrors Go resolver `UpdateStoragePolicy` write half
    /// (system.resolvers.go:50-57): persist the typed policy.
    async fn set_storage_policy(&self, policy: StoragePolicy) -> Result<(), SystemSettingsError>;

    // ----- retryPolicy domain (system.resolvers.go:59-67, 412-415) -----

    /// Mirrors Go resolver `RetryPolicy` (system.resolvers.go:412-415).
    async fn retry_policy(&self) -> Result<RetryPolicy, SystemSettingsError>;

    /// Mirrors Go resolver `UpdateRetryPolicy` write half
    /// (system.resolvers.go:60-67).
    async fn set_retry_policy(&self, policy: RetryPolicy) -> Result<(), SystemSettingsError>;

    // ----- userAgentPassThrough domain (system.resolvers.go:306-309, 542-552) -----

    /// Mirrors Go resolver `UserAgentPassThroughSettings`
    /// (system.resolvers.go:542-552): read the persisted boolean.
    async fn user_agent_pass_through(&self) -> Result<bool, SystemSettingsError>;

    /// Mirrors Go resolver `UpdateUserAgentPassThroughSettings`
    /// (system.resolvers.go:306-309): persist the boolean.
    async fn set_user_agent_pass_through(&self, enabled: bool) -> Result<(), SystemSettingsError>;

    // ----- defaultDataStorage domain (system.resolvers.go:102-110, 432-447) -----

    /// Mirrors Go resolver `DefaultDataStorageID` (system.resolvers.go:432-447):
    /// return the raw numeric id (`0` when unset). The GraphQL resolver wraps
    /// it in the GUID wire form (`gid://conduit/DataStorage/<id>`), or returns
    /// `null` when the service yields `0`.
    async fn default_data_storage_id(&self) -> Result<i64, SystemSettingsError>;

    /// Mirrors Go resolver `UpdateDefaultDataStorage`
    /// (system.resolvers.go:102-110): persist the numeric id.
    async fn set_default_data_storage_id(&self, id: i64) -> Result<(), SystemSettingsError>;

    // ----- generalSettings domain (system.resolvers.go:160-167, 512-515) -----

    /// Mirrors Go resolver `SystemGeneralSettings` (system.resolvers.go:512-515):
    /// return the persisted general settings (or the default on not-found).
    async fn general_settings(&self) -> Result<SystemGeneralSettings, SystemSettingsError>;

    /// Mirrors Go resolver `UpdateSystemGeneralSettings`
    /// (system.resolvers.go:160-167): persist the general settings.
    async fn set_general_settings(
        &self,
        actor_user_id: Option<i64>,
        settings: SystemGeneralSettings,
    ) -> Result<(), SystemSettingsError>;

    // ----- systemModelSettings domain (system.resolvers.go:79-100, 422-430) -----

    /// Mirrors Go resolver `SystemModelSettings` (system.resolvers.go:422-430):
    /// return the persisted model settings (or the default on not-found).
    async fn model_settings(&self) -> Result<SystemModelSettings, SystemSettingsError>;

    /// Mirrors Go resolver `UpdateSystemModelSettings`
    /// (system.resolvers.go:79-100): persist the model settings. The resolver
    /// preserves `developer_settings` from the current value when the input
    /// field is `None` (older clients).
    async fn set_model_settings(
        &self,
        settings: SystemModelSettings,
    ) -> Result<(), SystemSettingsError>;
}

/// Pure helper applying the Go resolver's partial-merge semantics
/// (system.resolvers.go:216-227): each `Some` input field overrides the
/// current value; `None` fields are left untouched.
pub fn merge_security_settings(
    current: &SecuritySettings,
    input: &UpdateSecuritySettingsInput,
) -> SecuritySettings {
    SecuritySettings {
        blocked_ips: input
            .blocked_ips
            .clone()
            .unwrap_or_else(|| current.blocked_ips.clone()),
        show_request_log_ip_ban_icon: input
            .show_request_log_ip_ban_icon
            .unwrap_or(current.show_request_log_ip_ban_icon),
    }
}

/// Maps a typed [`OnboardingRecord`] onto the GraphQL [`OnboardingInfo`] shape.
/// Mirrors Go resolver `OnboardingInfo` (system.resolvers.go:460-479).
pub fn onboarding_info_from_record(record: OnboardingRecord) -> OnboardingInfo {
    let map_module = |m: OnboardingModule| -> (bool, Option<TimeScalar>) {
        (m.onboarded, m.completed_at.map(TimeScalar))
    };
    let system_model_setting = record.system_model_setting.map(|m| {
        let (onboarded, completed_at) = map_module(m);
        SystemModelSettingOnboarding {
            onboarded,
            completed_at,
        }
    });
    let auto_disable_channel = record.auto_disable_channel.map(|m| {
        let (onboarded, completed_at) = map_module(m);
        AutoDisableChannelOnboarding {
            onboarded,
            completed_at,
        }
    });
    OnboardingInfo {
        onboarded: record.onboarded,
        completed_at: record.completed_at.map(TimeScalar),
        system_model_setting,
        auto_disable_channel,
    }
}

// ===========================================================================
// Resolver wiring — Query methods live on `QueryRoot` (lib.rs). Mutation
// methods live on `MutationRoot` (mutation.rs). The Query methods are
// contributed by a free-standing `#[Object] impl crate::QueryRoot` block
// here so the slice stays self-contained.
// ===========================================================================

/// Resolves the injected [`SystemSettingsServices`] from the async-graphql
/// context data bag. If no service was wired (e.g. the bare SDL-smoke
/// schema), returns the Go-equivalent "service unavailable" error message so
/// callers surface the familiar failure mode rather than panicking.
pub(crate) fn system_settings_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn SystemSettingsServices>, String> {
    match ctx.data::<Arc<dyn SystemSettingsServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(SystemSettingsError::ServiceUnavailable.to_string()),
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

    use super::*;
    use crate::mutation::MutationRoot;

    /// Build a `DateTime<Utc>` from a known-good timestamp, returning a
    /// typed error instead of panicking. Used in place of `.unwrap()` (which
    /// the workspace denies).
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
    // In-memory fake service. Mirrors the Go resolver call sequence without
    // touching any DB / HTTP.
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct FakeSystemServices {
        version: SystemVersion,
        version_error: Option<SystemSettingsError>,
        check_update_error: Option<SystemSettingsError>,
        check_update_result: VersionCheck,
        proxy_presets: Vec<ProxyPreset>,
        proxy_presets_error: Option<SystemSettingsError>,
        security: Arc<Mutex<SecuritySettings>>,
        security_read_error: Option<SystemSettingsError>,
        save_calls: Arc<Mutex<Vec<ProxyPreset>>>,
        save_error: Option<SystemSettingsError>,
        delete_calls: Arc<Mutex<Vec<String>>>,
        delete_error: Option<SystemSettingsError>,
        onboarding_record: Arc<Mutex<Option<OnboardingRecord>>>,
        onboarding_read_error: Option<SystemSettingsError>,
        complete_calls: Arc<Mutex<u32>>,
        complete_error: Option<SystemSettingsError>,
        brand: Arc<Mutex<(String, String, String)>>,
        brand_read_error: Option<SystemSettingsError>,
        set_brand_name_calls: Arc<Mutex<Vec<String>>>,
        set_brand_logo_calls: Arc<Mutex<Vec<String>>>,
        set_title_calls: Arc<Mutex<Vec<String>>>,
        set_brand_error: Option<SystemSettingsError>,
        storage_policy: Arc<Mutex<StoragePolicy>>,
        storage_policy_read_error: Option<SystemSettingsError>,
        set_storage_policy_calls: Arc<Mutex<Vec<StoragePolicy>>>,
        set_storage_policy_error: Option<SystemSettingsError>,
        retry_policy: Arc<Mutex<RetryPolicy>>,
        retry_policy_read_error: Option<SystemSettingsError>,
        set_retry_policy_calls: Arc<Mutex<Vec<RetryPolicy>>>,
        set_retry_policy_error: Option<SystemSettingsError>,
        user_agent_pass_through: Arc<Mutex<bool>>,
        user_agent_pass_through_read_error: Option<SystemSettingsError>,
        set_user_agent_pass_through_calls: Arc<Mutex<Vec<bool>>>,
        set_user_agent_pass_through_error: Option<SystemSettingsError>,
        default_data_storage_id: Arc<Mutex<i64>>,
        default_data_storage_read_error: Option<SystemSettingsError>,
        set_default_data_storage_id_calls: Arc<Mutex<Vec<i64>>>,
        set_default_data_storage_error: Option<SystemSettingsError>,
        general_settings: Arc<Mutex<SystemGeneralSettings>>,
        general_settings_read_error: Option<SystemSettingsError>,
        set_general_settings_calls: Arc<Mutex<Vec<SystemGeneralSettings>>>,
        set_general_settings_error: Option<SystemSettingsError>,
        model_settings: Arc<Mutex<SystemModelSettings>>,
        model_settings_read_error: Option<SystemSettingsError>,
        set_model_settings_calls: Arc<Mutex<Vec<SystemModelSettings>>>,
        set_model_settings_error: Option<SystemSettingsError>,
    }

    #[async_trait::async_trait]
    impl SystemSettingsServices for FakeSystemServices {
        async fn system_version(&self) -> Result<SystemVersion, SystemSettingsError> {
            match &self.version_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.version.clone()),
            }
        }

        async fn check_for_update(&self) -> Result<VersionCheck, SystemSettingsError> {
            match &self.check_update_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.check_update_result.clone()),
            }
        }

        async fn proxy_presets(&self) -> Result<Vec<ProxyPreset>, SystemSettingsError> {
            match &self.proxy_presets_error {
                Some(err) => Err(err.clone()),
                None => Ok(self.proxy_presets.clone()),
            }
        }

        async fn security_settings(&self) -> Result<SecuritySettings, SystemSettingsError> {
            match &self.security_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.security).clone()),
            }
        }

        async fn onboarding_record(&self) -> Result<Option<OnboardingRecord>, SystemSettingsError> {
            match &self.onboarding_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.onboarding_record).clone()),
            }
        }

        async fn complete_onboarding(&self) -> Result<(), SystemSettingsError> {
            *lock(&self.complete_calls) += 1;
            match &self.complete_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn set_security_settings(
            &self,
            settings: SecuritySettings,
        ) -> Result<(), SystemSettingsError> {
            *lock(&self.security) = settings;
            Ok(())
        }

        async fn save_proxy_preset(&self, preset: ProxyPreset) -> Result<(), SystemSettingsError> {
            lock(&self.save_calls).push(preset);
            match &self.save_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn delete_proxy_preset(&self, url: &str) -> Result<(), SystemSettingsError> {
            lock(&self.delete_calls).push(url.to_string());
            match &self.delete_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn brand_name(&self) -> Result<String, SystemSettingsError> {
            match &self.brand_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.brand).0.clone()),
            }
        }

        async fn brand_logo(&self) -> Result<String, SystemSettingsError> {
            match &self.brand_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.brand).1.clone()),
            }
        }

        async fn title(&self) -> Result<String, SystemSettingsError> {
            match &self.brand_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.brand).2.clone()),
            }
        }

        async fn set_brand_name(&self, name: &str) -> Result<(), SystemSettingsError> {
            lock(&self.set_brand_name_calls).push(name.to_string());
            match &self.set_brand_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn set_brand_logo(&self, logo: &str) -> Result<(), SystemSettingsError> {
            lock(&self.set_brand_logo_calls).push(logo.to_string());
            match &self.set_brand_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn set_title(&self, title: &str) -> Result<(), SystemSettingsError> {
            lock(&self.set_title_calls).push(title.to_string());
            match &self.set_brand_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn storage_policy(&self) -> Result<StoragePolicy, SystemSettingsError> {
            match &self.storage_policy_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.storage_policy).clone()),
            }
        }

        async fn set_storage_policy(
            &self,
            policy: StoragePolicy,
        ) -> Result<(), SystemSettingsError> {
            lock(&self.set_storage_policy_calls).push(policy);
            match &self.set_storage_policy_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn retry_policy(&self) -> Result<RetryPolicy, SystemSettingsError> {
            match &self.retry_policy_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.retry_policy).clone()),
            }
        }

        async fn set_retry_policy(&self, policy: RetryPolicy) -> Result<(), SystemSettingsError> {
            lock(&self.set_retry_policy_calls).push(policy);
            match &self.set_retry_policy_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn user_agent_pass_through(&self) -> Result<bool, SystemSettingsError> {
            match &self.user_agent_pass_through_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(*lock(&self.user_agent_pass_through)),
            }
        }

        async fn set_user_agent_pass_through(
            &self,
            enabled: bool,
        ) -> Result<(), SystemSettingsError> {
            lock(&self.set_user_agent_pass_through_calls).push(enabled);
            match &self.set_user_agent_pass_through_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn default_data_storage_id(&self) -> Result<i64, SystemSettingsError> {
            match &self.default_data_storage_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(*lock(&self.default_data_storage_id)),
            }
        }

        async fn set_default_data_storage_id(&self, id: i64) -> Result<(), SystemSettingsError> {
            lock(&self.set_default_data_storage_id_calls).push(id);
            match &self.set_default_data_storage_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn general_settings(&self) -> Result<SystemGeneralSettings, SystemSettingsError> {
            match &self.general_settings_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.general_settings).clone()),
            }
        }

        async fn set_general_settings(
            &self,
            _actor_user_id: Option<i64>,
            settings: SystemGeneralSettings,
        ) -> Result<(), SystemSettingsError> {
            lock(&self.set_general_settings_calls).push(settings);
            match &self.set_general_settings_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn model_settings(&self) -> Result<SystemModelSettings, SystemSettingsError> {
            match &self.model_settings_read_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.model_settings).clone()),
            }
        }

        async fn set_model_settings(
            &self,
            settings: SystemModelSettings,
        ) -> Result<(), SystemSettingsError> {
            lock(&self.set_model_settings_calls).push(settings);
            match &self.set_model_settings_error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }
    }

    type TestSchema = Schema<crate::QueryRoot, MutationRoot, EmptySubscription>;

    fn schema_with_services(services: FakeSystemServices) -> TestSchema {
        let arc: Arc<dyn SystemSettingsServices> = Arc::new(services);
        crate::admin_schema_builder().data(arc).finish()
    }

    fn data_object<const N: usize>(fields: [(&'static str, Value); N]) -> Value {
        let mut map = async_graphql::indexmap::IndexMap::new();
        for (name, value) in fields {
            map.insert(Name::new(name), value);
        }
        Value::Object(map)
    }

    // ---- pure merge logic --------------------------------------------

    #[test]
    fn merge_security_settings_keeps_current_when_input_is_none() {
        // Go resolver (system.resolvers.go:216-227): each `nil` field keeps
        // the current value.
        let current = SecuritySettings {
            blocked_ips: vec!["1.2.3.4".to_string()],
            show_request_log_ip_ban_icon: true,
        };
        let input = UpdateSecuritySettingsInput {
            blocked_ips: None,
            show_request_log_ip_ban_icon: None,
        };
        let merged = merge_security_settings(&current, &input);
        assert_eq!(merged.blocked_ips, current.blocked_ips);
        assert_eq!(
            merged.show_request_log_ip_ban_icon,
            current.show_request_log_ip_ban_icon
        );
    }

    #[test]
    fn merge_security_settings_overrides_only_provided_fields() {
        let current = SecuritySettings {
            blocked_ips: vec!["1.2.3.4".to_string()],
            show_request_log_ip_ban_icon: true,
        };
        let input = UpdateSecuritySettingsInput {
            blocked_ips: Some(vec![]),
            show_request_log_ip_ban_icon: None,
        };
        let merged = merge_security_settings(&current, &input);
        assert!(merged.blocked_ips.is_empty());
        assert!(merged.show_request_log_ip_ban_icon);
    }

    // ---- onboarding mapping ------------------------------------------

    #[test]
    fn onboarding_info_from_record_maps_all_modules() -> Result<(), String> {
        // Go resolver (system.resolvers.go:460-479): each present sub-module
        // is mapped to its GraphQL type; absent sub-modules stay null.
        let t = ts(2024, 1, 2, 3, 4, 5)?;
        let record = OnboardingRecord {
            onboarded: true,
            completed_at: Some(t),
            system_model_setting: Some(OnboardingModule {
                onboarded: true,
                completed_at: Some(t),
            }),
            auto_disable_channel: None,
        };
        let info = onboarding_info_from_record(record);
        assert!(info.onboarded);
        assert_eq!(info.completed_at.map(|x| x.0), Some(t));
        match info.system_model_setting {
            Some(ref sms) => {
                assert!(sms.onboarded);
                assert_eq!(sms.completed_at.as_ref().map(|x| x.0), Some(t));
            }
            None => panic!("system_model_setting should be present"),
        }
        assert!(info.auto_disable_channel.is_none());
        Ok(())
    }

    #[test]
    fn onboarding_info_from_default_record_has_no_modules() {
        // Go resolver returns null only when the entire record is nil; a
        // default-struct record still serializes to a non-null GraphQL
        // object with all sub-modules null.
        let record = OnboardingRecord::default();
        let info = onboarding_info_from_record(record);
        assert!(!info.onboarded);
        assert!(info.completed_at.is_none());
        assert!(info.system_model_setting.is_none());
        assert!(info.auto_disable_channel.is_none());
    }

    // ---- resolver: system_version -----------------------------------

    #[tokio::test]
    async fn system_version_returns_service_data() {
        let fake = FakeSystemServices {
            version: SystemVersion {
                version: "9.9.9".to_string(),
                commit: "abc".to_string(),
                build_time: "2024-01-01".to_string(),
                rust_version: "rustc 1.96.0".to_string(),
                platform: "linux/amd64".to_string(),
                uptime: "1h".to_string(),
            },
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute("{ systemVersion { version commit buildTime rustVersion platform uptime } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let sv = match obj.get(&Name::new("systemVersion")) {
            Some(v) => v,
            None => panic!("systemVersion field missing in {obj:?}"),
        };
        let fields = as_object(sv);
        match fields.get(&Name::new("version")) {
            Some(Value::String(s)) => assert_eq!(s, "9.9.9"),
            other => panic!("version field unexpected: {other:?}"),
        }
        // Public camelCase field name remains stable.
        assert!(
            fields.contains_key(&Name::new("rustVersion")),
            "rustVersion acronym field missing in {fields:?}"
        );
    }

    // ---- resolver: check_for_update ---------------------------------

    #[tokio::test]
    async fn check_for_update_returns_version_check_fields() {
        let fake = FakeSystemServices {
            check_update_result: VersionCheck {
                current_version: "1.0.0".to_string(),
                latest_version: "1.1.0".to_string(),
                has_update: true,
                release_url: "https://example.com/release".to_string(),
            },
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute("{ checkForUpdate { currentVersion latestVersion hasUpdate releaseUrl } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let vc = match obj.get(&Name::new("checkForUpdate")) {
            Some(v) => v,
            None => panic!("checkForUpdate field missing"),
        };
        let fields = as_object(vc);
        match fields.get(&Name::new("hasUpdate")) {
            Some(Value::Boolean(true)) => {}
            other => panic!("hasUpdate field unexpected: {other:?}"),
        }
    }

    // ---- resolver: proxy_presets ------------------------------------

    #[tokio::test]
    async fn proxy_presets_returns_list() {
        let fake = FakeSystemServices {
            proxy_presets: vec![
                ProxyPreset {
                    name: Some("a".to_string()),
                    url: "http://a".to_string(),
                    username: Some("u".to_string()),
                    password: Some("****".to_string()),
                },
                ProxyPreset {
                    name: None,
                    url: "http://b".to_string(),
                    username: None,
                    password: None,
                },
            ],
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute("{ proxyPresets { name url username password } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let list = match obj.get(&Name::new("proxyPresets")) {
            Some(v) => v,
            None => panic!("proxyPresets field missing"),
        };
        match list {
            Value::List(items) => assert_eq!(items.len(), 2),
            other => panic!("expected list, got {other:?}"),
        }
    }

    // ---- resolver: security_settings --------------------------------

    #[tokio::test]
    async fn security_settings_returns_blocked_ips_acronym_field() {
        let fake = FakeSystemServices {
            security: Arc::new(Mutex::new(SecuritySettings {
                blocked_ips: vec!["10.0.0.1".to_string()],
                show_request_log_ip_ban_icon: false,
            })),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute("{ securitySettings { blockedIPs showRequestLogIPBanIcon } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let sdl_str = resp.data.to_string();
        assert!(
            sdl_str.contains("blockedIPs:"),
            "acronym field blockedIPs must appear verbatim: {sdl_str:?}"
        );
        assert!(
            sdl_str.contains("showRequestLogIPBanIcon:"),
            "acronym field showRequestLogIPBanIcon must appear verbatim: {sdl_str:?}"
        );
    }

    // ---- resolver: onboarding_info ----------------------------------

    #[tokio::test]
    async fn onboarding_info_returns_null_when_record_absent() {
        // Go resolver (system.resolvers.go:456-458): info == nil -> null.
        let fake = FakeSystemServices {
            onboarding_record: Arc::new(Mutex::new(None)),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema.execute("{ onboardingInfo { onboarded } }").await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("onboardingInfo")) {
            Some(Value::Null) => {}
            Some(other) => panic!("expected null when record is absent, got {other:?}"),
            None => panic!("onboardingInfo field missing"),
        }
    }

    #[tokio::test]
    async fn onboarding_info_returns_typed_payload_when_record_present() -> Result<(), String> {
        let t = ts(2024, 5, 6, 7, 8, 9)?;
        let fake = FakeSystemServices {
            onboarding_record: Arc::new(Mutex::new(Some(OnboardingRecord {
                onboarded: true,
                completed_at: Some(t),
                system_model_setting: Some(OnboardingModule {
                    onboarded: true,
                    completed_at: Some(t),
                }),
                auto_disable_channel: None,
            }))),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(
                "{ onboardingInfo { onboarded completedAt systemModelSetting { onboarded completedAt } autoDisableChannel { onboarded } } }",
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        Ok(())
    }

    // ---- resolver: complete_onboarding mutation ---------------------

    #[tokio::test]
    async fn complete_onboarding_returns_true_and_invokes_service() {
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(r#"mutation { completeOnboarding(input: { dummy: "x" }) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data,
            data_object([("completeOnboarding", Value::Boolean(true))])
        );
        assert_eq!(*lock(&fake.complete_calls), 1);
    }

    #[tokio::test]
    async fn complete_onboarding_surfaces_error() {
        let fake = FakeSystemServices {
            complete_error: Some(SystemSettingsError::CompleteOnboarding(
                "denied".to_string(),
            )),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(r#"mutation { completeOnboarding(input: {}) }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to complete onboarding"), "msg: {msg}");
        assert!(msg.contains("denied"), "msg: {msg}");
    }

    // ---- resolver: save_proxy_preset mutation -----------------------

    #[tokio::test]
    async fn save_proxy_preset_returns_true_and_forwards_input() {
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    saveProxyPreset(input: {
                        name: "p1",
                        url: "http://proxy",
                        username: "u",
                        password: "secret"
                    })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data,
            data_object([("saveProxyPreset", Value::Boolean(true))])
        );
        let calls = lock(&fake.save_calls).clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].url, "http://proxy");
        assert_eq!(calls[0].password.as_deref(), Some("secret"));
    }

    // ---- resolver: delete_proxy_preset mutation ---------------------

    #[tokio::test]
    async fn delete_proxy_preset_returns_true_and_forwards_url() {
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(r#"mutation { deleteProxyPreset(url: "http://p") }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data,
            data_object([("deleteProxyPreset", Value::Boolean(true))])
        );
        let calls = lock(&fake.delete_calls).clone();
        assert_eq!(calls, vec!["http://p".to_string()]);
    }

    // ---- resolver: update_security_settings mutation ----------------

    #[tokio::test]
    async fn update_security_settings_writes_merged_value() {
        // Go resolver (system.resolvers.go:209-234): read default, merge the
        // provided `blockedIPs` override, write back, return true.
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    updateSecuritySettings(input: { blockedIPs: ["9.9.9.9"] })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data,
            data_object([("updateSecuritySettings", Value::Boolean(true))])
        );
        let stored = lock(&fake.security).clone();
        assert_eq!(stored.blocked_ips, vec!["9.9.9.9".to_string()]);
        // Default is `show_request_log_ip_ban_icon = true`; the input didn't
        // override it so the merged value must be preserved.
        assert!(stored.show_request_log_ip_ban_icon);
    }

    #[tokio::test]
    async fn update_security_settings_preserves_unset_fields() {
        let fake = FakeSystemServices {
            security: Arc::new(Mutex::new(SecuritySettings {
                blocked_ips: vec!["1.1.1.1".to_string()],
                show_request_log_ip_ban_icon: false,
            })),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    updateSecuritySettings(input: { showRequestLogIPBanIcon: true })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let stored = lock(&fake.security).clone();
        // blockedIPs not provided in input -> preserved from current.
        assert_eq!(stored.blocked_ips, vec!["1.1.1.1".to_string()]);
        assert!(stored.show_request_log_ip_ban_icon);
    }

    // ---- SDL shape parity -------------------------------------------

    /// Builds the real admin schema and asserts every type/field the slice
    /// contributes appears in the SDL exactly as the Go contract declares.
    #[test]
    fn sdl_contains_system_slice_types_and_signatures() {
        let arc: Arc<dyn SystemSettingsServices> = Arc::new(FakeSystemServices::default());
        let sdl = crate::admin_schema_builder().data(arc).finish().sdl();

        // Output types.
        for expected in [
            "type SystemVersion {",
            "type VersionCheck {",
            "type ProxyPreset {",
            "type SecuritySettings {",
            "type SystemModelSettingOnboarding {",
            "type AutoDisableChannelOnboarding {",
            "type OnboardingInfo {",
            "type BrandSettings {",
            "type StoragePolicy {",
            "type CleanupOption {",
            "type RetryPolicy {",
            "type UpstreamErrorPolicy {",
            "type AutoDisableChannelStatus {",
            "type AutoDisableChannel {",
            "type UserAgentPassThroughSettings {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        // Input types.
        for expected in [
            "input SaveProxyPresetInput {",
            "input UpdateSecuritySettingsInput {",
            "input CompleteOnboardingInput {",
            "input UpdateBrandSettingsInput {",
            "input UpdateStoragePolicyInput {",
            "input CleanupOptionInput {",
            "input UpdateRetryPolicyInput {",
            "input UpstreamErrorPolicyInput {",
            "input AutoDisableChannelStatusInput {",
            "input AutoDisableChannelInput {",
            "input UpdateUserAgentPassThroughSettingsInput {",
            "input UpdateDefaultDataStorageInput {",
        ] {
            assert!(sdl.contains(expected), "SDL missing {expected}:\n{sdl}");
        }

        // Queries.
        for expected in [
            "systemVersion: SystemVersion!",
            "checkForUpdate: VersionCheck!",
            "proxyPresets: [ProxyPreset!]!",
            "securitySettings: SecuritySettings!",
            "onboardingInfo: OnboardingInfo",
            "brandSettings: BrandSettings!",
            "storagePolicy: StoragePolicy!",
            "retryPolicy: RetryPolicy!",
            "userAgentPassThroughSettings: UserAgentPassThroughSettings!",
            "defaultDataStorageID: ID",
        ] {
            assert!(
                sdl.contains(expected),
                "SDL missing query {expected}:\n{sdl}"
            );
        }

        // Mutations.
        for expected in [
            "completeOnboarding(input: CompleteOnboardingInput!): Boolean!",
            "saveProxyPreset(input: SaveProxyPresetInput!): Boolean!",
            "deleteProxyPreset(url: String!): Boolean!",
            "updateSecuritySettings(input: UpdateSecuritySettingsInput!): Boolean!",
            "updateBrandSettings(input: UpdateBrandSettingsInput!): Boolean!",
            "updateStoragePolicy(input: UpdateStoragePolicyInput!): Boolean!",
            "updateRetryPolicy(input: UpdateRetryPolicyInput!): Boolean!",
            "updateUserAgentPassThroughSettings(input: UpdateUserAgentPassThroughSettingsInput!): Boolean!",
            "updateDefaultDataStorage(input: UpdateDefaultDataStorageInput!): Boolean!",
        ] {
            assert!(
                sdl.contains(expected),
                "SDL missing mutation {expected}:\n{sdl}"
            );
        }

        // Public camelCase field names.
        assert!(
            sdl.contains("rustVersion: String!"),
            "SDL missing rustVersion field: {sdl}"
        );
        assert!(
            sdl.contains("blockedIPs: [String!]!"),
            "SDL missing blockedIPs field: {sdl}"
        );
        assert!(
            sdl.contains("showRequestLogIPBanIcon: Boolean!"),
            "SDL missing showRequestLogIPBanIcon field: {sdl}"
        );
        assert!(
            sdl.contains("defaultDataStorageID: ID"),
            "SDL missing defaultDataStorageID acronym field: {sdl}"
        );
    }

    /// Cross-check: the SDL the resolver emits must agree with the captured
    /// snapshot at `tests/contracts/admin_graphql_schema.graphql` for the
    /// system-settings slice. Uses `sdl_parity::assert_block_parity` so the
    /// comparison ignores field ordering, directives, and default values.
    #[test]
    fn sdl_matches_snapshot_for_system_slice() -> Result<(), Box<dyn std::error::Error>> {
        let arc: Arc<dyn SystemSettingsServices> = Arc::new(FakeSystemServices::default());
        let sdl = crate::admin_schema_builder().data(arc).finish().sdl();
        let snapshot = crate::sdl_parity::snapshot_text()?;

        // Every type in this slice — full field set must match exactly.
        for header in [
            "type SystemVersion",
            "type VersionCheck",
            "type ProxyPreset",
            "type SecuritySettings",
            "type SystemModelSettingOnboarding",
            "type AutoDisableChannelOnboarding",
            "type OnboardingInfo",
            "input SaveProxyPresetInput",
            "input UpdateSecuritySettingsInput",
            "input CompleteOnboardingInput",
            // New domains.
            "type BrandSettings",
            "type StoragePolicy",
            "type CleanupOption",
            "type RetryPolicy",
            "type UpstreamErrorPolicy",
            "type AutoDisableChannelStatus",
            "type AutoDisableChannel",
            "type UserAgentPassThroughSettings",
            "input UpdateBrandSettingsInput",
            "input UpdateStoragePolicyInput",
            "input CleanupOptionInput",
            "input UpdateRetryPolicyInput",
            "input UpstreamErrorPolicyInput",
            "input AutoDisableChannelStatusInput",
            "input AutoDisableChannelInput",
            "input UpdateUserAgentPassThroughSettingsInput",
            "input UpdateDefaultDataStorageInput",
        ] {
            let extensions = match header {
                "type RetryPolicy" => &["costScoreWeight: Int!"][..],
                "input UpdateRetryPolicyInput" => &["costScoreWeight: Int"][..],
                _ => &[],
            };
            crate::sdl_parity::assert_block_parity_with_extensions(
                &sdl,
                &snapshot,
                header,
                header,
                &[],
                extensions,
            )?;
        }

        Ok(())
    }

    // ---- resolver: brand_settings query -----------------------------

    #[tokio::test]
    async fn brand_settings_returns_three_fields() {
        let fake = FakeSystemServices {
            brand: Arc::new(Mutex::new((
                "Acme".to_string(),
                "data:image/png;base64,XYZ".to_string(),
                "Acme Portal".to_string(),
            ))),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute("{ brandSettings { brandName brandLogo title } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("brandName: \""), "brandName missing: {s}");
        assert!(s.contains("brandLogo: \""), "brandLogo missing: {s}");
        assert!(s.contains("title: \""), "title missing: {s}");
    }

    // ---- resolver: update_brand_settings mutation -------------------

    #[tokio::test]
    async fn update_brand_settings_forwards_each_provided_field() {
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    updateBrandSettings(input: {
                        brandName: "N",
                        brandLogo: "L",
                        title: "T"
                    })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data,
            data_object([("updateBrandSettings", Value::Boolean(true))])
        );
        assert_eq!(
            lock(&fake.set_brand_name_calls).clone(),
            vec!["N".to_string()]
        );
        assert_eq!(
            lock(&fake.set_brand_logo_calls).clone(),
            vec!["L".to_string()]
        );
        assert_eq!(lock(&fake.set_title_calls).clone(), vec!["T".to_string()]);
    }

    #[tokio::test]
    async fn update_brand_settings_skips_none_fields() {
        // Go resolver (system.resolvers.go:24-46): only `Some` inputs trigger
        // the matching setter; `None` fields are no-ops.
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(r#"mutation { updateBrandSettings(input: { title: "T" }) }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert!(lock(&fake.set_brand_name_calls).is_empty());
        assert!(lock(&fake.set_brand_logo_calls).is_empty());
        assert_eq!(lock(&fake.set_title_calls).clone(), vec!["T".to_string()]);
    }

    // ---- resolver: storage_policy query -----------------------------

    #[tokio::test]
    async fn storage_policy_returns_typed_fields() {
        let policy = StoragePolicy {
            store_chunks: true,
            live_preview: false,
            store_request_headers: true,
            store_request_body: true,
            store_response_body: false,
            cleanup_options: vec![CleanupOption {
                resource_type: "requests".to_string(),
                enabled: true,
                cleanup_days: 7,
            }],
        };
        let fake = FakeSystemServices {
            storage_policy: Arc::new(Mutex::new(policy)),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(
                "{ storagePolicy { storeChunks livePreview storeRequestBody storeResponseBody cleanupOptions { resourceType enabled cleanupDays } } }",
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("storeChunks: true"), "missing storeChunks: {s}");
        assert!(
            s.contains("storeResponseBody: false"),
            "missing storeResponseBody: {s}"
        );
        assert!(
            s.contains("resourceType: \"requests\""),
            "missing nested CleanupOption.resourceType: {s}"
        );
    }

    // ---- resolver: update_storage_policy mutation -------------------

    #[tokio::test]
    async fn update_storage_policy_forwards_typed_policy() {
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    updateStoragePolicy(input: {
                        storeChunks: true,
                        cleanupOptions: [
                            { resourceType: "usage", enabled: true, cleanupDays: 14 }
                        ]
                    })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let calls = lock(&fake.set_storage_policy_calls).clone();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].store_chunks);
        // Unset fields fall back to Go `defaultStoragePolicy` body-store flags.
        assert!(calls[0].store_request_body);
        assert!(calls[0].store_response_body);
        assert_eq!(calls[0].cleanup_options.len(), 1);
        assert_eq!(calls[0].cleanup_options[0].cleanup_days, 14);
    }

    // ---- resolver: retry_policy query -------------------------------

    #[tokio::test]
    async fn retry_policy_returns_typed_fields() {
        let policy = RetryPolicy {
            max_channel_retries: 3,
            max_single_channel_retries: 2,
            retry_delay_ms: 100,
            stream_first_event_timeout_seconds: 30,
            non_stream_response_timeout_seconds: 0,
            load_balancer_strategy: "adaptive".to_string(),
            cost_score_weight: 0,
            enabled: true,
            auto_disable_channel: AutoDisableChannel {
                enabled: true,
                statuses: vec![AutoDisableChannelStatus {
                    status: 500,
                    times: 5,
                }],
            },
            empty_response_detection: true,
            upstream_error_policy: UpstreamErrorPolicy {
                mode: "passthrough".to_string(),
                custom_message: String::new(),
            },
        };
        let fake = FakeSystemServices {
            retry_policy: Arc::new(Mutex::new(policy)),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(
                "{ retryPolicy { maxChannelRetries enabled loadBalancerStrategy autoDisableChannel { enabled statuses { status times } } upstreamErrorPolicy { mode } } }",
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(
            s.contains("loadBalancerStrategy: \"adaptive\""),
            "missing strategy: {s}"
        );
        assert!(s.contains("maxChannelRetries: 3"), "missing max: {s}");
    }

    // ---- resolver: update_retry_policy mutation ---------------------

    #[tokio::test]
    async fn update_retry_policy_forwards_typed_policy() {
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    updateRetryPolicy(input: {
                        enabled: true,
                        loadBalancerStrategy: "failover",
                        autoDisableChannel: {
                            enabled: true,
                            statuses: [{ status: 503, times: 3 }]
                        }
                    })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let calls = lock(&fake.set_retry_policy_calls).clone();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].enabled);
        assert_eq!(calls[0].load_balancer_strategy, "failover");
        assert!(calls[0].auto_disable_channel.enabled);
        assert_eq!(calls[0].auto_disable_channel.statuses.len(), 1);
        assert_eq!(calls[0].auto_disable_channel.statuses[0].status, 503);
    }

    // ---- resolver: user_agent_pass_through_settings query -----------

    #[tokio::test]
    async fn user_agent_pass_through_settings_returns_bool() {
        let fake = FakeSystemServices {
            user_agent_pass_through: Arc::new(Mutex::new(true)),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute("{ userAgentPassThroughSettings { enabled } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let s = resp.data.to_string();
        assert!(s.contains("enabled: true"), "missing enabled: {s}");
    }

    // ---- resolver: update_user_agent_pass_through_settings mutation -

    #[tokio::test]
    async fn update_user_agent_pass_through_settings_forwards_bool() {
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    updateUserAgentPassThroughSettings(input: { enabled: true })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            lock(&fake.set_user_agent_pass_through_calls).clone(),
            vec![true]
        );
    }

    // ---- resolver: default_data_storage_id query --------------------

    #[tokio::test]
    async fn default_data_storage_id_returns_null_when_zero() {
        // Go resolver (system.resolvers.go:439-441): `id == 0` -> null.
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake);

        let resp = schema.execute("{ defaultDataStorageID }").await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("defaultDataStorageID")) {
            Some(Value::Null) => {}
            other => panic!("expected null when id is 0, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_data_storage_id_returns_guid_wire_form_when_set() {
        // Go resolver (system.resolvers.go:443-446): wraps the numeric id in
        // `objects.GUID{Type: "DataStorage", ID: id}` which serializes to
        // `gid://conduit/DataStorage/<id>`.
        let fake = FakeSystemServices {
            default_data_storage_id: Arc::new(Mutex::new(42)),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema.execute("{ defaultDataStorageID }").await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("defaultDataStorageID")) {
            Some(Value::String(s)) => assert_eq!(s, "gid://conduit/DataStorage/42"),
            other => panic!("expected GUID wire form, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn system_general_settings_reports_unlocked_accounting_currency() {
        let fake = FakeSystemServices {
            general_settings: Arc::new(Mutex::new(SystemGeneralSettings {
                accounting_currency_locked: false,
                ..SystemGeneralSettings::default()
            })),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute("{ systemGeneralSettings { accountingCurrencyLocked } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let settings = as_object(
            as_object(&resp.data)
                .get(&Name::new("systemGeneralSettings"))
                .expect("systemGeneralSettings field"),
        );
        assert_eq!(
            settings.get(&Name::new("accountingCurrencyLocked")),
            Some(&Value::Boolean(false))
        );
    }

    #[tokio::test]
    async fn system_general_settings_reports_locked_accounting_currency() {
        let fake = FakeSystemServices {
            general_settings: Arc::new(Mutex::new(SystemGeneralSettings {
                accounting_currency_locked: true,
                ..SystemGeneralSettings::default()
            })),
            ..FakeSystemServices::default()
        };
        let schema = schema_with_services(fake);

        let resp = schema
            .execute("{ systemGeneralSettings { accountingCurrencyLocked } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let settings = as_object(
            as_object(&resp.data)
                .get(&Name::new("systemGeneralSettings"))
                .expect("systemGeneralSettings field"),
        );
        assert_eq!(
            settings.get(&Name::new("accountingCurrencyLocked")),
            Some(&Value::Boolean(true))
        );
    }

    // ---- resolver: update_default_data_storage mutation -------------

    #[tokio::test]
    async fn update_default_data_storage_parses_guid_and_forwards_id() {
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake.clone());

        let resp = schema
            .execute(
                r#"mutation {
                    updateDefaultDataStorage(input: { dataStorageID: "gid://conduit/DataStorage/7" })
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert_eq!(
            resp.data,
            data_object([("updateDefaultDataStorage", Value::Boolean(true))])
        );
        assert_eq!(
            lock(&fake.set_default_data_storage_id_calls).clone(),
            vec![7]
        );
    }

    #[tokio::test]
    async fn update_default_data_storage_rejects_bad_guid() {
        let fake = FakeSystemServices::default();
        let schema = schema_with_services(fake);

        let resp = schema
            .execute(
                r#"mutation {
                    updateDefaultDataStorage(input: { dataStorageID: "not-a-guid" })
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("guid"), "unexpected msg: {msg}");
    }

    // ---- service-unavailable fallback -------------------------------

    #[tokio::test]
    async fn resolvers_surface_service_unavailable_when_unwired() {
        // Schema with NO system-settings service injected — every resolver
        // must surface the "service unavailable" failure mode.
        let schema: TestSchema = crate::admin_schema_builder().finish();

        let resp = schema.execute("{ systemVersion { version } }").await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("system service is not available"),
            "unexpected msg: {msg}"
        );
    }

    // ---- TestQueryRoot placeholder (unused but keeps schema builder happy)
    // ----------------------------------------------------------------
    // `mutation::MutationRoot` is the real one we use; the test schema needs
    // a Query type as well, but `crate::QueryRoot` is what carries the
    // system resolvers under test. This empty marker silences the unused
    // import warning if `Object` is otherwise only referenced via the
    // `MutationRoot` re-export.

    #[allow(dead_code)]
    struct _MarkerQueryRoot;
    #[Object]
    impl _MarkerQueryRoot {
        async fn _marker(&self) -> &'static str {
            "marker"
        }
    }
}
