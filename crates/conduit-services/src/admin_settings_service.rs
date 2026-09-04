//! SVC-01 — real DB backend for the admin-graphql `SystemSettingsServices`
//! trait (handle: `Guan`).
//!
//! ## What this file is
//!
//! `crates/conduit-admin-graphql/src/system.rs` declares the
//! [`conduit_admin_graphql::system::SystemSettingsServices`] trait (~26 async
//! methods) plus the GraphQL DTO types (`SystemVersion`, `VersionCheck`,
//! `ProxyPreset`, `SecuritySettings`, `StoragePolicy`, `RetryPolicy`,
//! `SystemGeneralSettings`, `SystemModelSettings`, `OnboardingRecord`, …). The
//! admin-graphql crate keeps itself free of DB/HTTP concerns and expects the
//! host to inject an `Arc<dyn SystemSettingsServices>` into the schema data bag.
//!
//! Until now the only implementation was the in-memory `FakeSystemServices`
//! used by admin-graphql's own resolver tests. This file provides the **first
//! real, DB-backed implementation**, wrapping the already-ported
//! [`crate::system_service::SystemService`] (the faithful Rust port of Go
//! `biz.SystemService`, backed in production by the PostgreSQL system repo via the blanket
//! `SystemSettingsRepo for T: SystemRepo` impl). Each trait method here is the
//! Rust counterpart of the matching branch in Go
//! `internal/server/gql/system.resolvers.go` — it performs the same DTO ↔ typed
//! conversion the Go gqlgen resolvers do before/after calling the biz service.
//!
//! ## Dependency direction (verified — no cycle)
//!
//! `conduit-admin-graphql` depends only on `conduit-auth` + `conduit-core`
//! (both sit *below* `conduit-services`); it does **not** depend on
//! `conduit-services` or `conduit-db`. Therefore `conduit-services →
//! conduit-admin-graphql` is acyclic, and this crate can `impl
//! SystemSettingsServices` directly. `cargo tree` / the two Cargo.toml files
//! confirm the arrow only points services → admin-graphql. If a future refactor
//! ever adds admin-graphql → services, this impl would have to move to the host
//! (conduit-bin) instead; that is the only thing keeping it here.
//!
//! ## Host wiring (Leader / conduit-bin `wiring.rs`)
//!
//! ```ignore
//! use std::sync::Arc;
//! use conduit_admin_graphql::system::SystemSettingsServices;
//! use conduit_services::admin_settings_service::DbSystemSettingsBackend;
//!
//! // `system` is the already-built Arc<SystemService> the host wires for the
//! // REST /admin/system/status + /initialize handlers (see AppServices).
//! let backend = Arc::new(DbSystemSettingsBackend::new(system.clone()));
//! let services: Arc<dyn SystemSettingsServices> = backend;
//! let schema = conduit_admin_graphql::admin_schema_builder()
//!     .data(services)
//!     .finish();
//! ```
//!
//! The backend needs a [`RequestContext`] to call the underlying repo (the Go
//! biz methods take `ctx context.Context`; the Rust repo layer takes a
//! `RequestContext` carrying the policy principal). GraphQL admin operations
//! run as the authenticated admin/owner, which maps to the system principal at
//! the repo-policy layer, so the backend builds a [`Principal::system`] context
//! per call — same trust level the Go admin resolvers run under. If the host
//! later threads a per-request principal into the schema data bag, swap
//! [`DbSystemSettingsBackend::context`] to read it from there.

use std::sync::Arc;

use async_trait::async_trait;

use conduit_admin_graphql::system::{
    AutoDisableChannel, AutoDisableChannelStatus, CurrencyExchangeRate, ProxyPreset, RetryPolicy,
    SecuritySettings, StoragePolicy, SystemGeneralSettings, SystemModelSettings,
    SystemSettingsError, SystemSettingsServices, SystemVersion, UpstreamErrorPolicy, VersionCheck,
};
use conduit_admin_graphql::system::{
    CleanupOption as GqlCleanupOption, DeveloperModelSettings as GqlDeveloperModelSettings,
    OnboardingModule as GqlOnboardingModule, OnboardingRecord as GqlOnboardingRecord,
};
// Admin-graphql model-association GraphQL DTOs (SimpleObjects, no serde) and the
// `Any` scalar used by `FilterCondition.value`.
use conduit_admin_graphql::model as gqlmodel;
use conduit_admin_graphql::scalars::AnyScalar;
use conduit_admin_graphql::scalars::DecimalScalar;
// Core objects (serde-typed) that the SystemService returns/consumes.
use conduit_core::objects as coreobj;
use conduit_db::{PolicyContext, Principal, RequestContext};

use crate::gc_service::CleanupOption as ServiceCleanupOption;
use crate::system_service::{
    ProxyPreset as ServiceProxyPreset, RetryPolicy as ServiceRetryPolicy,
    SecuritySettings as ServiceSecuritySettings, StoragePolicy as ServiceStoragePolicy,
    SystemService,
};

// ===========================================================================
// Backend struct
// ===========================================================================

/// Real, DB-backed implementation of the admin-graphql
/// [`SystemSettingsServices`] trait.
///
/// Wraps an `Arc<SystemService>` (the ported Go `biz.SystemService`, itself
/// backed by the PostgreSQL system repo through the `SystemSettingsRepo for T:
/// SystemRepo` blanket impl). Each method mirrors the matching Go resolver in
/// `system.resolvers.go`, converting between the admin-graphql GraphQL DTOs and
/// the `SystemService` typed values.
///
/// `SystemVersion` / `CheckForUpdate` are *not* persisted settings in Go — the
/// resolvers read compile-time build info and hit GitHub, respectively. Those
/// depend on host-only inputs (build metadata, an HTTP client), so the backend
/// takes an optional [`VersionProvider`]; when absent it surfaces the
/// Go-equivalent error, matching the "service unavailable" degrade mode.
#[derive(Clone)]
pub struct DbSystemSettingsBackend {
    system: Arc<SystemService>,
    version: Option<Arc<dyn VersionProvider>>,
}

/// Host-supplied source for the two non-persisted operations: `systemVersion`
/// (Go `build.GetBuildInfo()`) and `checkForUpdate` (Go
/// `fetchLatestGitHubRelease` + semver compare). Kept as a trait so the host
/// wires build metadata + an HTTP client without this crate depending on
/// either. See Go `version.go:19-60` and `system.resolvers.go:482-500`.
#[async_trait]
pub trait VersionProvider: Send + Sync {
    /// Mirrors Go resolver `SystemVersion` (`system.resolvers.go:482-485`):
    /// returns `build.GetBuildInfo()` (compile-time constants).
    async fn system_version(&self) -> Result<SystemVersion, String>;

    /// Mirrors Go resolver `CheckForUpdate` (`system.resolvers.go:487-500` →
    /// `version.go:46-60`): fetches the latest GitHub release and compares.
    async fn check_for_update(&self) -> Result<VersionCheck, String>;
}

impl DbSystemSettingsBackend {
    /// Wrap a `SystemService`. `systemVersion` / `checkForUpdate` will surface
    /// [`SystemSettingsError::Version`] / [`SystemSettingsError::CheckUpdate`]
    /// until a [`VersionProvider`] is attached via [`Self::with_version_provider`].
    pub fn new(system: Arc<SystemService>) -> Self {
        Self {
            system,
            version: None,
        }
    }

    /// Attach the host build-info / update-check provider.
    pub fn with_version_provider(mut self, provider: Arc<dyn VersionProvider>) -> Self {
        self.version = Some(provider);
        self
    }

    /// Build the per-call [`RequestContext`]. Admin GraphQL operations run at
    /// admin/owner trust, which maps to the repo-layer system principal (Go
    /// admin resolvers run with a fully-authorized context). See the module
    /// doc for how to thread a real per-request principal later.
    fn context() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::system()))
    }
}

// ===========================================================================
// DTO conversions: admin-graphql GraphQL types <-> SystemService typed values
// ===========================================================================

// --- SecuritySettings -----------------------------------------------------

/// Go `system.resolvers.go:527-530` returns `biz.SecuritySettings` mapped 1:1
/// onto the GraphQL `SecuritySettings`. The `extra` passthrough map is service
/// -internal (upgrade safety) and has no GraphQL surface, so it is dropped on
/// the way out and defaulted on the way in.
fn security_to_gql(s: ServiceSecuritySettings) -> SecuritySettings {
    SecuritySettings {
        blocked_ips: s.blocked_ips,
        show_request_log_ip_ban_icon: s.show_request_log_ip_ban_icon,
    }
}

fn security_from_gql(s: SecuritySettings) -> ServiceSecuritySettings {
    ServiceSecuritySettings {
        blocked_ips: s.blocked_ips,
        show_request_log_ip_ban_icon: s.show_request_log_ip_ban_icon,
        extra: Default::default(),
    }
}

// --- ProxyPreset ----------------------------------------------------------

/// Go `ProxyPresets` resolver (`system.resolvers.go:532-540`) returns the
/// masked list — but the service's [`SystemService::masked_proxy_presets`]
/// already applies the masking, so this conversion is a straight field copy.
fn proxy_to_gql(p: ServiceProxyPreset) -> ProxyPreset {
    ProxyPreset {
        name: p.name,
        url: p.url,
        username: p.username,
        password: p.password,
    }
}

fn proxy_from_gql(p: ProxyPreset) -> ServiceProxyPreset {
    ServiceProxyPreset {
        name: p.name,
        url: p.url,
        username: p.username,
        password: p.password,
        extra: Default::default(),
    }
}

// --- StoragePolicy --------------------------------------------------------

fn cleanup_to_gql(c: ServiceCleanupOption) -> GqlCleanupOption {
    GqlCleanupOption {
        resource_type: c.resource_type,
        enabled: c.enabled,
        // Go `CleanupOption.CleanupDays` is `int`; the GraphQL field is `Int`
        // (i32). Service side stores i64; clamp on the narrowing cast (values
        // are day-counts, always small).
        cleanup_days: c.cleanup_days as i32,
    }
}

fn cleanup_from_gql(c: GqlCleanupOption) -> ServiceCleanupOption {
    ServiceCleanupOption {
        resource_type: c.resource_type,
        enabled: c.enabled,
        cleanup_days: c.cleanup_days as i64,
    }
}

fn storage_to_gql(p: ServiceStoragePolicy) -> StoragePolicy {
    StoragePolicy {
        store_chunks: p.store_chunks,
        live_preview: p.live_preview,
        store_request_headers: p.store_request_headers,
        store_request_body: p.store_request_body,
        store_response_body: p.store_response_body,
        cleanup_options: p.cleanup_options.into_iter().map(cleanup_to_gql).collect(),
    }
}

fn storage_from_gql(p: StoragePolicy) -> ServiceStoragePolicy {
    ServiceStoragePolicy {
        store_chunks: p.store_chunks,
        live_preview: p.live_preview,
        store_request_headers: p.store_request_headers,
        store_request_body: p.store_request_body,
        store_response_body: p.store_response_body,
        cleanup_options: p
            .cleanup_options
            .into_iter()
            .map(cleanup_from_gql)
            .collect(),
    }
}

// --- RetryPolicy ----------------------------------------------------------
//
// The GraphQL `RetryPolicy` (admin-graphql) is fully typed (max_channel_retries
// / auto_disable_channel / upstream_error_policy / …). The SystemService
// `RetryPolicy` only types the two response-timeout fields it clamps and keeps
// every other Go field in a `#[serde(flatten)] extra: BTreeMap<String, Value>`.
// To convert without losing fields, round-trip through the JSON wire form using
// the Go json tags (snake_case, `system.go:283-349`). This keeps the whole
// policy — including fields not individually typed on the service struct — in
// sync with the stored representation.

/// The GraphQL `RetryPolicy` mirrors Go `biz.RetryPolicy` field-for-field.
/// Serialize it into the Go wire JSON (snake_case tags) so it round-trips
/// through the service's partially-typed [`ServiceRetryPolicy`] and the KV
/// store byte-identically to what Go writes.
fn retry_gql_to_json(p: &RetryPolicy) -> serde_json::Value {
    serde_json::json!({
        "enabled": p.enabled,
        "max_channel_retries": p.max_channel_retries,
        "max_single_channel_retries": p.max_single_channel_retries,
        "retry_delay_ms": p.retry_delay_ms,
        "stream_first_event_timeout_seconds": p.stream_first_event_timeout_seconds,
        "non_stream_response_timeout_seconds": p.non_stream_response_timeout_seconds,
        "load_balancer_strategy": p.load_balancer_strategy,
        "auto_disable_channel": {
            "enabled": p.auto_disable_channel.enabled,
            "statuses": p.auto_disable_channel.statuses.iter().map(|s| serde_json::json!({
                "status": s.status,
                "times": s.times,
            })).collect::<Vec<_>>(),
        },
        "empty_response_detection": p.empty_response_detection,
        "upstream_error_policy": {
            "mode": p.upstream_error_policy.mode,
            "custom_message": p.upstream_error_policy.custom_message,
        },
    })
}

/// Decode the Go wire JSON of a `biz.RetryPolicy` into the GraphQL DTO. Missing
/// fields fall back to the Go defaults (`defaultRetryPolicy`,
/// `system_default.go:22-31`) — this is what the service returns after
/// `RetryPolicy::clamped` for the response-timeout fields; the rest of the Go
/// defaults are applied here for the unset structural fields.
fn retry_json_to_gql(v: &serde_json::Value) -> RetryPolicy {
    let get_bool = |k: &str| v.get(k).and_then(serde_json::Value::as_bool);
    let get_i64 = |k: &str| v.get(k).and_then(serde_json::Value::as_i64);
    let get_str = |k: &str| {
        v.get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };

    let auto = v.get("auto_disable_channel");
    let auto_enabled = auto
        .and_then(|a| a.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let statuses = auto
        .and_then(|a| a.get("statuses"))
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|s| AutoDisableChannelStatus {
                    status: s
                        .get("status")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0) as i32,
                    times: s
                        .get("times")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0) as i32,
                })
                .collect()
        })
        .unwrap_or_default();

    let uep = v.get("upstream_error_policy");
    let uep_mode = uep
        .and_then(|u| u.get("mode"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        // Go default mode is "passthrough" (defaultRetryPolicy, system_default.go:28-30).
        .unwrap_or_else(|| "passthrough".to_string());
    let uep_msg = uep
        .and_then(|u| u.get("custom_message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();

    RetryPolicy {
        // Defaults mirror Go `defaultRetryPolicy` (system_default.go:22-31).
        max_channel_retries: get_i64("max_channel_retries").unwrap_or(3) as i32,
        max_single_channel_retries: get_i64("max_single_channel_retries").unwrap_or(2) as i32,
        retry_delay_ms: get_i64("retry_delay_ms").unwrap_or(1000) as i32,
        stream_first_event_timeout_seconds: get_i64("stream_first_event_timeout_seconds")
            .unwrap_or(30) as i32,
        non_stream_response_timeout_seconds: get_i64("non_stream_response_timeout_seconds")
            .unwrap_or(0) as i32,
        load_balancer_strategy: get_str("load_balancer_strategy")
            .unwrap_or_else(|| "adaptive".to_string()),
        enabled: get_bool("enabled").unwrap_or(true),
        auto_disable_channel: AutoDisableChannel {
            enabled: auto_enabled,
            statuses,
        },
        empty_response_detection: get_bool("empty_response_detection").unwrap_or(false),
        upstream_error_policy: UpstreamErrorPolicy {
            mode: uep_mode,
            custom_message: uep_msg,
        },
    }
}

// --- SystemGeneralSettings ------------------------------------------------
//
// SystemService does not (yet) expose typed general-settings getters/setters,
// so this backend reads/writes the JSON directly through the generic
// get_json/set_json + the system_key::GENERAL_SETTINGS constant. Go source:
// `system.go:1290-1324` (GeneralSettings / SetGeneralSettings), adapted to the
// canonical accounting defaults (CNY / station credits / UTC).

fn default_accounting_currency() -> String {
    conduit_core::objects::money::DEFAULT_ACCOUNTING_CURRENCY_CODE.into()
}
fn default_credit_display_name() -> String {
    conduit_core::objects::money::DEFAULT_CREDIT_DISPLAY_NAME.into()
}
fn default_credits_per_accounting_unit() -> rust_decimal::Decimal {
    rust_decimal::Decimal::from(10_000)
}
fn default_accounting_settings_version() -> u64 {
    1
}

#[derive(serde::Serialize, serde::Deserialize)]
struct GeneralSettingsWire {
    #[serde(default = "default_accounting_currency")]
    accounting_currency_code: String,
    #[serde(default, rename = "timezone")]
    timezone: String,
    #[serde(default = "default_credit_display_name")]
    credit_display_name: String,
    #[serde(default = "default_credits_per_accounting_unit")]
    credits_per_accounting_unit: rust_decimal::Decimal,
    #[serde(default)]
    exchange_rates: Vec<conduit_core::objects::money::CurrencyExchangeRate>,
    #[serde(default = "default_accounting_settings_version")]
    accounting_rate_version: u64,
}

impl Default for GeneralSettingsWire {
    fn default() -> Self {
        Self {
            accounting_currency_code: default_accounting_currency(),
            timezone: "UTC".into(),
            credit_display_name: default_credit_display_name(),
            credits_per_accounting_unit: default_credits_per_accounting_unit(),
            exchange_rates: Vec::new(),
            accounting_rate_version: default_accounting_settings_version(),
        }
    }
}

// --- SystemModelSettings --------------------------------------------------
//
// The GraphQL `SystemModelSettings` (admin-graphql) and the service's
// `conduit_core::objects::SystemModelSettings` bind to the same Go
// `biz.SystemModelSettings` (`system.go:388-425`), so the scalar fields map
// 1:1. The nested `developer_settings[].associations` need explicit branch
// conversion (below) because the admin DTOs carry no serde derives.
//
// The admin-graphql `ModelAssociation` family are gqlgen `SimpleObject`s with
// NO serde derives, so a JSON round-trip cannot bridge them to core's
// serde-typed `ModelAssociation`. Convert each of the seven association
// branches explicitly. Field shapes mirror Go `objects.ModelAssociation`
// (`internal/objects/model.go`) — the same struct both sides bind to.

/// core `Condition` -> admin `FilterCondition`. Go `objects.Condition`
/// (condition.go:24) binds to GraphQL `FilterCondition`; `Logic`/`Field`/
/// `Operator` are plain strings (always emitted), `value` is `any` (nullable).
fn condition_to_gql(c: &coreobj::Condition) -> gqlmodel::FilterCondition {
    gqlmodel::FilterCondition {
        condition_type: match c.r#type {
            coreobj::ConditionType::Group => gqlmodel::FilterConditionType::Group,
            // Leaf `Condition` and positionally-omitted both surface as the
            // leaf `condition` kind on the wire.
            coreobj::ConditionType::Condition | coreobj::ConditionType::Omitted => {
                gqlmodel::FilterConditionType::Condition
            }
        },
        logic: Some(c.logic.clone()),
        conditions: if c.conditions.is_empty() {
            None
        } else {
            Some(c.conditions.iter().map(condition_to_gql).collect())
        },
        field: Some(c.field.clone()),
        operator: Some(c.operator.clone()),
        value: c.value.clone().map(AnyScalar),
    }
}

/// admin `FilterCondition` -> core `Condition`.
fn condition_from_gql(c: &gqlmodel::FilterCondition) -> coreobj::Condition {
    coreobj::Condition {
        r#type: match c.condition_type {
            gqlmodel::FilterConditionType::Group => coreobj::ConditionType::Group,
            gqlmodel::FilterConditionType::Condition => coreobj::ConditionType::Condition,
        },
        logic: c.logic.clone().unwrap_or_default(),
        conditions: c
            .conditions
            .as_ref()
            .map(|v| v.iter().map(condition_from_gql).collect())
            .unwrap_or_default(),
        field: c.field.clone().unwrap_or_default(),
        operator: c.operator.clone().unwrap_or_default(),
        value: c.value.as_ref().map(|a| a.0.clone()),
    }
}

/// core `ExcludeAssociation` -> admin. Go `ExcludeAssociation` (model.go:79):
/// `ChannelNamePattern` zero-fills to "" (always emitted); the two slices are
/// nil-able (empty -> null on the wire).
fn exclude_to_gql(e: &coreobj::ExcludeAssociation) -> gqlmodel::ExcludeAssociation {
    gqlmodel::ExcludeAssociation {
        channel_name_pattern: Some(e.channel_name_pattern.clone()),
        channel_ids: if e.channel_ids.is_empty() {
            None
        } else {
            Some(e.channel_ids.clone())
        },
        channel_tags: if e.channel_tags.is_empty() {
            None
        } else {
            Some(e.channel_tags.clone())
        },
    }
}

fn exclude_from_gql(e: &gqlmodel::ExcludeAssociation) -> coreobj::ExcludeAssociation {
    coreobj::ExcludeAssociation {
        channel_name_pattern: e.channel_name_pattern.clone().unwrap_or_default(),
        channel_ids: e.channel_ids.clone().unwrap_or_default(),
        channel_tags: e.channel_tags.clone().unwrap_or_default(),
    }
}

/// core `ModelAssociation` -> admin `ModelAssociation` (all seven branches).
fn assoc_to_gql(a: &coreobj::ModelAssociation) -> gqlmodel::ModelAssociation {
    gqlmodel::ModelAssociation {
        association_type: a.kind.clone(),
        priority: a.priority,
        disabled: a.disabled,
        when: a.when.as_ref().map(|w| gqlmodel::ModelAssociationWhen {
            enabled: w.enabled,
            condition: w.condition.as_ref().map(condition_to_gql),
        }),
        channel_model: a
            .channel_model
            .as_ref()
            .map(|c| gqlmodel::ChannelModelAssociation {
                channel_id: c.channel_id,
                model_id: c.model_id.clone(),
            }),
        channel_regex: a
            .channel_regex
            .as_ref()
            .map(|c| gqlmodel::ChannelRegexAssociation {
                channel_id: c.channel_id,
                pattern: c.pattern.clone(),
            }),
        regex: a.regex.as_ref().map(|r| gqlmodel::RegexAssociation {
            pattern: r.pattern.clone(),
            exclude: if r.exclude.is_empty() {
                None
            } else {
                Some(r.exclude.iter().map(exclude_to_gql).collect())
            },
        }),
        model_id: a.model_id.as_ref().map(|m| gqlmodel::ModelIdAssociation {
            model_id: m.model_id.clone(),
            exclude: if m.exclude.is_empty() {
                None
            } else {
                Some(m.exclude.iter().map(exclude_to_gql).collect())
            },
        }),
        channel_tags_model: a.channel_tags_model.as_ref().map(|c| {
            gqlmodel::ChannelTagsModelAssociation {
                channel_tags: c.channel_tags.clone(),
                model_id: c.model_id.clone(),
            }
        }),
        channel_tags_regex: a.channel_tags_regex.as_ref().map(|c| {
            gqlmodel::ChannelTagsRegexAssociation {
                channel_tags: c.channel_tags.clone(),
                pattern: c.pattern.clone(),
            }
        }),
    }
}

/// admin `ModelAssociation` -> core `ModelAssociation` (all seven branches).
fn assoc_from_gql(a: gqlmodel::ModelAssociation) -> coreobj::ModelAssociation {
    coreobj::ModelAssociation {
        kind: a.association_type,
        priority: a.priority,
        disabled: a.disabled,
        when: a.when.map(|w| coreobj::ModelAssociationWhen {
            enabled: w.enabled,
            condition: w.condition.as_ref().map(condition_from_gql),
        }),
        channel_model: a.channel_model.map(|c| coreobj::ChannelModelAssociation {
            channel_id: c.channel_id,
            model_id: c.model_id,
        }),
        channel_regex: a.channel_regex.map(|c| coreobj::ChannelRegexAssociation {
            channel_id: c.channel_id,
            pattern: c.pattern,
        }),
        regex: a.regex.map(|r| coreobj::RegexAssociation {
            pattern: r.pattern,
            exclude: r
                .exclude
                .map(|v| v.iter().map(exclude_from_gql).collect())
                .unwrap_or_default(),
        }),
        model_id: a.model_id.map(|m| coreobj::ModelIDAssociation {
            model_id: m.model_id,
            exclude: m
                .exclude
                .map(|v| v.iter().map(exclude_from_gql).collect())
                .unwrap_or_default(),
        }),
        channel_tags_model: a
            .channel_tags_model
            .map(|c| coreobj::ChannelTagsModelAssociation {
                channel_tags: c.channel_tags,
                model_id: c.model_id,
            }),
        channel_tags_regex: a
            .channel_tags_regex
            .map(|c| coreobj::ChannelTagsRegexAssociation {
                channel_tags: c.channel_tags,
                pattern: c.pattern,
            }),
    }
}

fn model_settings_to_gql(core: &coreobj::SystemModelSettings) -> SystemModelSettings {
    let developer_settings = core
        .developer_settings
        .iter()
        .map(|dev| GqlDeveloperModelSettings {
            developer: dev.developer.clone(),
            associations: dev.associations.iter().map(assoc_to_gql).collect(),
        })
        .collect();
    SystemModelSettings {
        fallback_to_channels_on_model_not_found: core.fallback_to_channels_on_model_not_found,
        query_all_channel_models: core.query_all_channel_models,
        default_model_api_include_all: core.default_model_api_include_all,
        auto_reasoning_effort: core.auto_reasoning_effort,
        model_blacklist_regex: core.model_blacklist_regex.clone(),
        developer_settings,
    }
}

fn model_settings_from_gql(gql: SystemModelSettings) -> coreobj::SystemModelSettings {
    let developer_settings = gql
        .developer_settings
        .into_iter()
        .map(|dev| coreobj::DeveloperModelSettings {
            developer: dev.developer,
            associations: dev.associations.into_iter().map(assoc_from_gql).collect(),
        })
        .collect();
    coreobj::SystemModelSettings {
        fallback_to_channels_on_model_not_found: gql.fallback_to_channels_on_model_not_found,
        query_all_channel_models: gql.query_all_channel_models,
        default_model_api_include_all: gql.default_model_api_include_all,
        auto_reasoning_effort: gql.auto_reasoning_effort,
        model_blacklist_regex: gql.model_blacklist_regex,
        developer_settings,
    }
}

// --- OnboardingRecord -----------------------------------------------------

fn onboarding_module_to_gql(m: crate::system_service::OnboardingModule) -> GqlOnboardingModule {
    GqlOnboardingModule {
        onboarded: m.onboarded,
        completed_at: m.completed_at,
    }
}

fn onboarding_to_gql(r: crate::system_service::OnboardingRecord) -> GqlOnboardingRecord {
    GqlOnboardingRecord {
        onboarded: r.onboarded,
        completed_at: r.completed_at,
        system_model_setting: r.system_model_setting.map(onboarding_module_to_gql),
        auto_disable_channel: r.auto_disable_channel.map(onboarding_module_to_gql),
    }
}

// ===========================================================================
// Trait implementation
// ===========================================================================

#[async_trait]
impl SystemSettingsServices for DbSystemSettingsBackend {
    // --- systemVersion (Go system.resolvers.go:482-485) -------------------
    async fn system_version(&self) -> Result<SystemVersion, SystemSettingsError> {
        match &self.version {
            Some(provider) => provider
                .system_version()
                .await
                .map_err(SystemSettingsError::Version),
            None => Err(SystemSettingsError::Version(
                "no build-info provider wired".to_string(),
            )),
        }
    }

    // --- checkForUpdate (Go system.resolvers.go:487-500) ------------------
    async fn check_for_update(&self) -> Result<VersionCheck, SystemSettingsError> {
        match &self.version {
            Some(provider) => provider
                .check_for_update()
                .await
                .map_err(SystemSettingsError::CheckUpdate),
            None => Err(SystemSettingsError::CheckUpdate(
                "no update-check provider wired".to_string(),
            )),
        }
    }

    // --- proxyPresets (Go system.resolvers.go:532-540) --------------------
    // Go masks passwords in the resolver; the service's `masked_proxy_presets`
    // does the same, so we call the masked variant here.
    async fn proxy_presets(&self) -> Result<Vec<ProxyPreset>, SystemSettingsError> {
        let ctx = Self::context();
        let presets = self
            .system
            .masked_proxy_presets(&ctx)
            .await
            .map_err(|e| SystemSettingsError::ProxyPresets(e.to_string()))?;
        Ok(presets.into_iter().map(proxy_to_gql).collect())
    }

    // --- saveProxyPreset (Go system.resolvers.go:286-294) -----------------
    async fn save_proxy_preset(&self, preset: ProxyPreset) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .save_proxy_preset(&ctx, proxy_from_gql(preset))
            .await
            .map_err(|e| SystemSettingsError::SaveProxyPreset(e.to_string()))
    }

    // --- deleteProxyPreset (Go system.resolvers.go:296-304) ---------------
    async fn delete_proxy_preset(&self, url: &str) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .delete_proxy_preset(&ctx, url)
            .await
            .map_err(|e| SystemSettingsError::DeleteProxyPreset(e.to_string()))
    }

    // --- securitySettings (Go system.resolvers.go:527-530) ----------------
    async fn security_settings(&self) -> Result<SecuritySettings, SystemSettingsError> {
        let ctx = Self::context();
        let s = self
            .system
            .security_settings(&ctx)
            .await
            .map_err(|e| SystemSettingsError::ReadSecurity(e.to_string()))?;
        Ok(security_to_gql(s))
    }

    // --- updateSecuritySettings write half (Go system.resolvers.go:228) ---
    // The resolver reads current + merges + writes; the merge is done in the
    // admin-graphql resolver via `merge_security_settings`, so this method only
    // persists the already-merged value (Go `SetSecuritySettings`,
    // system.go:1626-1636). The service re-normalizes before storing.
    async fn set_security_settings(
        &self,
        settings: SecuritySettings,
    ) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .set_security_settings(&ctx, security_from_gql(settings))
            .await
            .map(|_| ())
            .map_err(|e| SystemSettingsError::UpdateSecurity(e.to_string()))
    }

    // --- onboardingInfo (Go system.resolvers.go:449-480) ------------------
    async fn onboarding_record(&self) -> Result<Option<GqlOnboardingRecord>, SystemSettingsError> {
        let ctx = Self::context();
        let record = self
            .system
            .onboarding_info(&ctx)
            .await
            .map_err(|e| SystemSettingsError::OnboardingInfo(e.to_string()))?;
        Ok(record.map(onboarding_to_gql))
    }

    // --- completeOnboarding (Go system.resolvers.go:112-120) --------------
    async fn complete_onboarding(&self) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .complete_onboarding(&ctx)
            .await
            .map_err(|e| SystemSettingsError::CompleteOnboarding(e.to_string()))
    }

    // --- brandSettings getters (Go system.resolvers.go:383-405) -----------
    async fn brand_name(&self) -> Result<String, SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .brand_name(&ctx)
            .await
            .map_err(|e| SystemSettingsError::BrandName(e.to_string()))
    }

    async fn brand_logo(&self) -> Result<String, SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .brand_logo(&ctx)
            .await
            .map_err(|e| SystemSettingsError::BrandLogo(e.to_string()))
    }

    // Go `Title` reads system_key::TITLE. SystemService has no typed `title`
    // getter yet, so read the raw value through the generic getter.
    async fn title(&self) -> Result<String, SystemSettingsError> {
        let ctx = Self::context();
        let value = self
            .system
            .get_system_value(&ctx, crate::system_service::system_key::TITLE)
            .await
            .map_err(|e| SystemSettingsError::Title(e.to_string()))?;
        Ok(value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    // --- brandSettings setters (Go system.resolvers.go:24-46) -------------
    async fn set_brand_name(&self, name: &str) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .set_brand_name(&ctx, name)
            .await
            .map_err(|e| SystemSettingsError::UpdateBrandName(e.to_string()))
    }

    async fn set_brand_logo(&self, logo: &str) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .set_brand_logo(&ctx, logo)
            .await
            .map_err(|e| SystemSettingsError::UpdateBrandLogo(e.to_string()))
    }

    async fn set_title(&self, title: &str) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .set_system_value(
                &ctx,
                crate::system_service::system_key::TITLE,
                serde_json::Value::from(title.to_string()),
            )
            .await
            .map(|_| ())
            .map_err(|e| SystemSettingsError::UpdateTitle(e.to_string()))
    }

    // --- storagePolicy (Go system.resolvers.go:407-410) -------------------
    async fn storage_policy(&self) -> Result<StoragePolicy, SystemSettingsError> {
        let ctx = Self::context();
        let p = self
            .system
            .storage_policy(&ctx)
            .await
            .map_err(|e| SystemSettingsError::StoragePolicy(e.to_string()))?;
        Ok(storage_to_gql(p))
    }

    // --- updateStoragePolicy write half (Go system.resolvers.go:50-57) ----
    async fn set_storage_policy(&self, policy: StoragePolicy) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .set_storage_policy(&ctx, &storage_from_gql(policy))
            .await
            .map_err(|e| SystemSettingsError::UpdateStoragePolicy(e.to_string()))
    }

    // --- retryPolicy (Go system.resolvers.go:412-415) ---------------------
    // The service getter returns a partially-typed RetryPolicy (only the two
    // clamped timeout fields typed, the rest in `extra`). Re-serialize it to
    // the Go wire JSON and decode the full DTO so the GraphQL layer sees every
    // typed field, with Go defaults filling any unset structural field.
    async fn retry_policy(&self) -> Result<RetryPolicy, SystemSettingsError> {
        let ctx = Self::context();
        let policy = self
            .system
            .retry_policy(&ctx)
            .await
            .map_err(|e| SystemSettingsError::RetryPolicy(e.to_string()))?
            .unwrap_or_default();
        let json = serde_json::to_value(&policy)
            .map_err(|e| SystemSettingsError::RetryPolicy(e.to_string()))?;
        Ok(retry_json_to_gql(&json))
    }

    // --- updateRetryPolicy write half (Go system.resolvers.go:60-67) ------
    async fn set_retry_policy(&self, policy: RetryPolicy) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        let json = retry_gql_to_json(&policy);
        let service_policy: ServiceRetryPolicy = serde_json::from_value(json)
            .map_err(|e| SystemSettingsError::UpdateRetryPolicy(e.to_string()))?;
        self.system
            .set_retry_policy(&ctx, service_policy)
            .await
            .map(|_| ())
            .map_err(|e| SystemSettingsError::UpdateRetryPolicy(e.to_string()))
    }

    // --- userAgentPassThroughSettings (Go system.resolvers.go:542-552) ----
    async fn user_agent_pass_through(&self) -> Result<bool, SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .user_agent_pass_through(&ctx)
            .await
            .map_err(|e| SystemSettingsError::UserAgentPassThrough(e.to_string()))
    }

    // --- updateUserAgentPassThroughSettings (Go system.resolvers.go:306-309)
    async fn set_user_agent_pass_through(&self, enabled: bool) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .set_user_agent_pass_through(&ctx, enabled)
            .await
            .map_err(|e| SystemSettingsError::UpdateUserAgentPassThrough(e.to_string()))
    }

    // --- defaultDataStorageID (Go system.resolvers.go:432-447) ------------
    async fn default_data_storage_id(&self) -> Result<i64, SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .default_data_storage_id(&ctx)
            .await
            .map_err(|e| SystemSettingsError::DefaultDataStorageID(e.to_string()))
    }

    // --- updateDefaultDataStorage (Go system.resolvers.go:102-110) --------
    async fn set_default_data_storage_id(&self, id: i64) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        self.system
            .set_default_data_storage_id(&ctx, id)
            .await
            .map_err(|e| SystemSettingsError::UpdateDefaultDataStorage(e.to_string()))
    }

    // --- systemGeneralSettings (Go system.resolvers.go:512-515) -----------
    // SystemService lacks a typed general-settings getter, so read the JSON via
    // the generic getter under system_key::GENERAL_SETTINGS and apply the
    // canonical accounting defaults for missing/empty fields.
    async fn general_settings(&self) -> Result<SystemGeneralSettings, SystemSettingsError> {
        let ctx = Self::context();
        let value = self
            .system
            .get_system_value(&ctx, crate::system_service::system_key::GENERAL_SETTINGS)
            .await
            .map_err(|e| SystemSettingsError::GeneralSettings(e.to_string()))?;
        let wire: GeneralSettingsWire = match value {
            None => GeneralSettingsWire::default(),
            Some(v) => serde_json::from_value(v)
                .map_err(|e| SystemSettingsError::GeneralSettings(e.to_string()))?,
        };
        let accounting_currency_code = if wire.accounting_currency_code.is_empty() {
            default_accounting_currency()
        } else {
            wire.accounting_currency_code
        };
        let timezone = if wire.timezone.is_empty() {
            "UTC".to_string()
        } else {
            wire.timezone
        };
        Ok(SystemGeneralSettings {
            accounting_currency_code,
            // This compatibility backend has no commercial repositories. The
            // production PostgreSQL host adapter derives the lock from the
            // price tables; callers should not wire this backend for pricing.
            accounting_currency_locked: false,
            timezone,
            credit_display_name: wire.credit_display_name,
            credits_per_accounting_unit: DecimalScalar(wire.credits_per_accounting_unit),
            exchange_rates: wire
                .exchange_rates
                .into_iter()
                .map(|rate| CurrencyExchangeRate {
                    currency_code: rate.currency,
                    quote_per_accounting_unit: DecimalScalar(rate.quote_per_accounting_unit),
                })
                .collect(),
            accounting_rate_version: i64::try_from(wire.accounting_rate_version)
                .unwrap_or(i64::MAX),
        })
    }

    // --- updateSystemGeneralSettings (Go system.resolvers.go:160-167) -----
    // Go `SetGeneralSettings` (system.go:1318-1324) json.Marshals the struct
    // and stores it. We mirror that: serialize the wire form and write via the
    // generic setter.
    async fn set_general_settings(
        &self,
        settings: SystemGeneralSettings,
    ) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        let accounting_settings = conduit_core::objects::money::AccountingSettings {
            accounting_currency: settings.accounting_currency_code.clone(),
            credit_display_name: settings.credit_display_name.clone(),
            credits_per_accounting_unit: settings.credits_per_accounting_unit.0,
            exchange_rates: settings
                .exchange_rates
                .iter()
                .map(|rate| conduit_core::objects::money::CurrencyExchangeRate {
                    currency: rate.currency_code.clone(),
                    quote_per_accounting_unit: rate.quote_per_accounting_unit.0,
                })
                .collect(),
            version: u64::try_from(settings.accounting_rate_version).unwrap_or(1),
        };
        accounting_settings
            .validate()
            .map_err(SystemSettingsError::UpdateGeneralSettings)?;
        let wire = GeneralSettingsWire {
            accounting_currency_code: settings.accounting_currency_code,
            timezone: settings.timezone,
            credit_display_name: settings.credit_display_name,
            credits_per_accounting_unit: settings.credits_per_accounting_unit.0,
            exchange_rates: settings
                .exchange_rates
                .into_iter()
                .map(|rate| conduit_core::objects::money::CurrencyExchangeRate {
                    currency: rate.currency_code,
                    quote_per_accounting_unit: rate.quote_per_accounting_unit.0,
                })
                .collect(),
            accounting_rate_version: u64::try_from(chrono::Utc::now().timestamp_millis())
                .unwrap_or(1),
        };
        let json = serde_json::to_value(&wire)
            .map_err(|e| SystemSettingsError::UpdateGeneralSettings(e.to_string()))?;
        self.system
            .set_system_value(
                &ctx,
                crate::system_service::system_key::GENERAL_SETTINGS,
                json,
            )
            .await
            .map(|_| ())
            .map_err(|e| SystemSettingsError::UpdateGeneralSettings(e.to_string()))
    }

    // --- systemModelSettings (Go system.resolvers.go:422-430) -------------
    async fn model_settings(&self) -> Result<SystemModelSettings, SystemSettingsError> {
        let ctx = Self::context();
        let core = self
            .system
            .model_settings(&ctx)
            .await
            .map_err(|e| SystemSettingsError::ModelSettings(e.to_string()))?;
        Ok(model_settings_to_gql(&core))
    }

    // --- updateSystemModelSettings (Go system.resolvers.go:79-100) --------
    // NOTE on developer-settings preservation: Go's resolver preserves
    // `DeveloperSettings` from the current value when `input.DeveloperSettings
    // == nil` (older clients). That merge lives in the admin-graphql resolver
    // (it owns the `Option<Vec<...>>` input); by the time the trait's
    // set_model_settings is called the resolver has already resolved the final
    // developer_settings, so this method just persists the resolved value.
    async fn set_model_settings(
        &self,
        settings: SystemModelSettings,
    ) -> Result<(), SystemSettingsError> {
        let ctx = Self::context();
        let core = model_settings_from_gql(settings);
        self.system
            .set_model_settings(&ctx, core)
            .await
            .map_err(|e| SystemSettingsError::UpdateModelSettings(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_cache::MemoryCache;
    use conduit_db::repo::InMemorySystemRepo;

    /// Build a backend over the repository test double. Runtime persistence is
    /// PostgreSQL-only; this in-memory implementation exercises service
    /// semantics without introducing a second database backend.
    async fn backend() -> Result<DbSystemSettingsBackend, Box<dyn std::error::Error>> {
        let repo = InMemorySystemRepo::new();
        let system =
            SystemService::from_system_repo(Arc::new(repo), Arc::new(MemoryCache::default()));
        Ok(DbSystemSettingsBackend::new(Arc::new(system)))
    }

    #[tokio::test]
    async fn security_settings_round_trips_through_db() -> Result<(), Box<dyn std::error::Error>> {
        let backend = backend().await?;

        // Default on empty store: empty blocked list + show-ban-icon true
        // (Go defaultSecuritySettings, system_default.go:81-84).
        let initial = backend
            .security_settings()
            .await
            .map_err(|e| e.to_string())?;
        assert!(initial.blocked_ips.is_empty());
        assert!(initial.show_request_log_ip_ban_icon);

        // Write + read back.
        backend
            .set_security_settings(SecuritySettings {
                blocked_ips: vec!["10.0.0.1".to_string()],
                show_request_log_ip_ban_icon: false,
            })
            .await
            .map_err(|e| e.to_string())?;

        let stored = backend
            .security_settings()
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(stored.blocked_ips, vec!["10.0.0.1".to_string()]);
        assert!(!stored.show_request_log_ip_ban_icon);
        Ok(())
    }

    #[tokio::test]
    async fn general_settings_defaults_to_cny_station_credit_and_round_trips()
    -> Result<(), Box<dyn std::error::Error>> {
        let backend = backend().await?;
        let g = backend
            .general_settings()
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(
            g.accounting_currency_code,
            conduit_core::objects::money::DEFAULT_ACCOUNTING_CURRENCY_CODE
        );
        assert!(!g.accounting_currency_locked);
        assert_eq!(g.timezone, "UTC");
        assert_eq!(
            g.credit_display_name,
            conduit_core::objects::money::DEFAULT_CREDIT_DISPLAY_NAME
        );
        assert_eq!(
            g.credits_per_accounting_unit,
            DecimalScalar(rust_decimal::Decimal::from(10_000))
        );
        assert!(g.exchange_rates.is_empty());
        assert_eq!(g.accounting_rate_version, 1);

        backend
            .set_general_settings(SystemGeneralSettings {
                timezone: "Asia/Shanghai".to_string(),
                credit_display_name: "Station Credits".to_string(),
                credits_per_accounting_unit: DecimalScalar(rust_decimal::Decimal::from(1_000)),
                ..SystemGeneralSettings::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        let g2 = backend
            .general_settings()
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(
            g2.accounting_currency_code,
            conduit_core::objects::money::DEFAULT_ACCOUNTING_CURRENCY_CODE
        );
        assert_eq!(g2.timezone, "Asia/Shanghai");
        assert_eq!(g2.credit_display_name, "Station Credits");
        assert_eq!(
            g2.credits_per_accounting_unit,
            DecimalScalar(rust_decimal::Decimal::from(1_000))
        );
        Ok(())
    }

    #[tokio::test]
    async fn storage_policy_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let backend = backend().await?;

        // Default (Go defaultStoragePolicy, system_default.go:3-20): body-store
        // flags true, two disabled cleanup options.
        let def = backend.storage_policy().await.map_err(|e| e.to_string())?;
        assert!(def.store_request_body);
        assert!(def.store_response_body);
        assert!(!def.store_chunks);
        assert_eq!(def.cleanup_options.len(), 2);

        backend
            .set_storage_policy(StoragePolicy {
                store_chunks: true,
                live_preview: true,
                store_request_headers: false,
                store_request_body: false,
                store_response_body: false,
                cleanup_options: vec![GqlCleanupOption {
                    resource_type: "requests".to_string(),
                    enabled: true,
                    cleanup_days: 7,
                }],
            })
            .await
            .map_err(|e| e.to_string())?;

        let stored = backend.storage_policy().await.map_err(|e| e.to_string())?;
        assert!(stored.store_chunks);
        assert!(!stored.store_request_headers);
        assert!(!stored.store_request_body);
        assert_eq!(stored.cleanup_options.len(), 1);
        assert_eq!(stored.cleanup_options[0].cleanup_days, 7);
        Ok(())
    }

    #[tokio::test]
    async fn retry_policy_defaults_match_go() -> Result<(), Box<dyn std::error::Error>> {
        let backend = backend().await?;
        // Empty store -> service returns default RetryPolicy; our JSON decode
        // fills Go defaults (system_default.go:22-31).
        let p = backend.retry_policy().await.map_err(|e| e.to_string())?;
        assert_eq!(p.max_channel_retries, 3);
        assert_eq!(p.max_single_channel_retries, 2);
        assert_eq!(p.retry_delay_ms, 1000);
        assert_eq!(p.load_balancer_strategy, "adaptive");
        assert!(p.enabled);
        assert_eq!(p.upstream_error_policy.mode, "passthrough");
        Ok(())
    }

    #[tokio::test]
    async fn retry_policy_round_trips_full_shape() -> Result<(), Box<dyn std::error::Error>> {
        let backend = backend().await?;
        backend
            .set_retry_policy(RetryPolicy {
                max_channel_retries: 5,
                max_single_channel_retries: 1,
                retry_delay_ms: 250,
                stream_first_event_timeout_seconds: 30,
                non_stream_response_timeout_seconds: 60,
                load_balancer_strategy: "failover".to_string(),
                enabled: true,
                auto_disable_channel: AutoDisableChannel {
                    enabled: true,
                    statuses: vec![AutoDisableChannelStatus {
                        status: 503,
                        times: 3,
                    }],
                },
                empty_response_detection: true,
                upstream_error_policy: UpstreamErrorPolicy {
                    mode: "hidden".to_string(),
                    custom_message: String::new(),
                },
            })
            .await
            .map_err(|e| e.to_string())?;

        let p = backend.retry_policy().await.map_err(|e| e.to_string())?;
        assert_eq!(p.max_channel_retries, 5);
        assert_eq!(p.load_balancer_strategy, "failover");
        assert!(p.auto_disable_channel.enabled);
        assert_eq!(p.auto_disable_channel.statuses.len(), 1);
        assert_eq!(p.auto_disable_channel.statuses[0].status, 503);
        assert_eq!(p.upstream_error_policy.mode, "hidden");
        Ok(())
    }

    #[tokio::test]
    async fn proxy_preset_save_and_masked_list() -> Result<(), Box<dyn std::error::Error>> {
        let backend = backend().await?;
        backend
            .save_proxy_preset(ProxyPreset {
                name: Some("p1".to_string()),
                url: "http://proxy".to_string(),
                username: Some("u".to_string()),
                password: Some("secret".to_string()),
            })
            .await
            .map_err(|e| e.to_string())?;

        let list = backend.proxy_presets().await.map_err(|e| e.to_string())?;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].url, "http://proxy");
        // Masked on the way out (Go masks in the resolver / service).
        assert_eq!(list[0].password.as_deref(), Some("****"));

        backend
            .delete_proxy_preset("http://proxy")
            .await
            .map_err(|e| e.to_string())?;
        assert!(
            backend
                .proxy_presets()
                .await
                .map_err(|e| e.to_string())?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn brand_and_title_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let backend = backend().await?;
        backend
            .set_brand_name("Acme")
            .await
            .map_err(|e| e.to_string())?;
        backend
            .set_brand_logo("data:img")
            .await
            .map_err(|e| e.to_string())?;
        backend
            .set_title("Acme Portal")
            .await
            .map_err(|e| e.to_string())?;

        assert_eq!(
            backend.brand_name().await.map_err(|e| e.to_string())?,
            "Acme"
        );
        assert_eq!(
            backend.brand_logo().await.map_err(|e| e.to_string())?,
            "data:img"
        );
        assert_eq!(
            backend.title().await.map_err(|e| e.to_string())?,
            "Acme Portal"
        );
        Ok(())
    }

    #[tokio::test]
    async fn model_settings_defaults_and_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let backend = backend().await?;
        // Defaults (Go defaultModelSettings, system_default.go:33-40).
        let def = backend.model_settings().await.map_err(|e| e.to_string())?;
        assert!(def.fallback_to_channels_on_model_not_found);
        assert!(def.query_all_channel_models);
        assert!(!def.default_model_api_include_all);
        assert!(def.developer_settings.is_empty());

        backend
            .set_model_settings(SystemModelSettings {
                fallback_to_channels_on_model_not_found: false,
                query_all_channel_models: false,
                default_model_api_include_all: true,
                auto_reasoning_effort: true,
                model_blacklist_regex: "gpt-3.*".to_string(),
                developer_settings: vec![],
            })
            .await
            .map_err(|e| e.to_string())?;

        let stored = backend.model_settings().await.map_err(|e| e.to_string())?;
        assert!(!stored.fallback_to_channels_on_model_not_found);
        assert!(stored.default_model_api_include_all);
        assert_eq!(stored.model_blacklist_regex, "gpt-3.*");
        Ok(())
    }

    #[tokio::test]
    async fn user_agent_and_default_storage_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let backend = backend().await?;
        assert!(
            !backend
                .user_agent_pass_through()
                .await
                .map_err(|e| e.to_string())?
        );
        backend
            .set_user_agent_pass_through(true)
            .await
            .map_err(|e| e.to_string())?;
        assert!(
            backend
                .user_agent_pass_through()
                .await
                .map_err(|e| e.to_string())?
        );

        assert_eq!(
            backend
                .default_data_storage_id()
                .await
                .map_err(|e| e.to_string())?,
            0
        );
        backend
            .set_default_data_storage_id(42)
            .await
            .map_err(|e| e.to_string())?;
        assert_eq!(
            backend
                .default_data_storage_id()
                .await
                .map_err(|e| e.to_string())?,
            42
        );
        Ok(())
    }

    #[tokio::test]
    async fn onboarding_record_none_then_completed() -> Result<(), Box<dyn std::error::Error>> {
        let backend = backend().await?;
        // No record yet -> None (Go OnboardingInfo returns nil).
        assert!(
            backend
                .onboarding_record()
                .await
                .map_err(|e| e.to_string())?
                .is_none()
        );

        backend
            .complete_onboarding()
            .await
            .map_err(|e| e.to_string())?;

        let record = backend
            .onboarding_record()
            .await
            .map_err(|e| e.to_string())?;
        match record {
            Some(r) => {
                assert!(r.onboarded);
                assert!(r.completed_at.is_some());
                // New user: auto_disable_channel is filled by complete_onboarding
                // (Go system_onboarding.go:78-83).
                assert!(r.auto_disable_channel.is_some());
            }
            None => return Err("expected an onboarding record after completion".into()),
        }
        Ok(())
    }
}
