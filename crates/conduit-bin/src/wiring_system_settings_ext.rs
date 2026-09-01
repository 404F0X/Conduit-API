//! Host-side adapter for the GAP-D system-settings GraphQL slice
//! (`conduit_admin_graphql::system_settings_ext::SystemSettingsExtServices`):
//! video storage settings, webhook notifier config, auto-backup settings, and
//! the two onboarding-completion mutations.
//!
//! All reads/writes go through the same domain
//! [`conduit_services::SystemService`] KV store the other system adapters
//! (`SystemSettingsAdapter` SVC-01, `SystemChannelAdapter`) use — no parallel
//! storage. Key names come from `conduit_services::system_key`, which mirrors
//! the Go constants byte-for-byte:
//!
//!   - `system_video_storage_settings` — Go `SystemKeyVideoStorageSettings`
//!     (`conduit/internal/server/biz/system.go:99`).
//!   - `webhook_notifier_config` — Go `SystemKeyWebhookNotifierConfig`
//!     (`system.go:71`).
//!   - `system_auto_backup_settings` — Go `SystemKeyAutoBackupSettings`
//!     (`system.go:95`).
//!   - `system_onboarded` — Go `SystemKeyOnboarded` (onboarding flags live as
//!     sub-objects inside this record, `system_onboarding.go:19-27`).
//!
//! ## Go parity map (method → Go source)
//!
//!   - `video_storage_settings` — Go `SystemService.VideoStorageSettings`
//!     (`system.go:1420-1444`): missing key → `defaultVideoStorageSettings`
//!     (`system_default.go:69-74`: disabled, id 0, interval 1, limit 50);
//!     stored value clamps non-positive `scan_interval_minutes`/`scan_limit`
//!     to the defaults. The typed wire struct is the already-ported domain
//!     `conduit_services::VideoStorageSettings` (snake_case tags + Go default).
//!   - `set_video_storage_settings` — Go `SetVideoStorageSettings`
//!     (`system.go:1446-1483`): clamp, then when `enabled` validate the target
//!     data storage exists and is neither primary nor `database`-typed, then
//!     persist. The Go resolver additionally calls `videoWorker.Reschedule`
//!     (`backup.resolvers.go`/`system.resolvers.go:180`) — the Rust host has
//!     no scheduler wired yet, so rescheduling is a documented follow-up.
//!   - `webhook_notifier_config` / `set_webhook_notifier_config` — Go
//!     `WebhookNotifierConfig`/`SetWebhookNotifierConfig`
//!     (`system.go:1085-1131`), already typed in the domain service; this
//!     adapter only bridges the GraphQL shapes (incl. the `proxy` field the
//!     domain type keeps in its `extra` flatten-map).
//!   - `auto_backup_settings` — Go `AutoBackupSettings` (`system.go:1362-1403`):
//!     missing key → `defaultAutoBackupSettings` (`system_default.go:57-67`);
//!     stored value zero-fills missing fields (Go `autoBackupSettingsJSON`'s
//!     `*bool` fallbacks for `include_usage_stats`/`include_request_logs` both
//!     default to `false`, which is exactly what serde's field-level
//!     `#[serde(default)]` on the domain struct produces).
//!   - `set_auto_backup_settings` — Go `SetAutoBackupSettings`
//!     (`system.go:1405-1418`): plain serialize + write. The read-merge is the
//!     resolver's job (`backup.resolvers.go:55-113`), per the trait contract.
//!     Go also calls `backupService.Reschedule` — same scheduler caveat.
//!   - `complete_system_model_setting_onboarding` /
//!     `complete_auto_disable_channel_onboarding` — Go
//!     `system_onboarding.go:89-126`; the domain service already ports the
//!     read-modify-write (`complete_system_model_setting_onboarding` /
//!     `complete_auto_disable_channel_onboarding`), so the adapter delegates.
//!
//! Owner-gating (`UpdateAutoBackupSettings` is owner-only in Go) is the host
//! resolver's responsibility, same convention as the other admin slices.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use conduit_admin_graphql::channel::{ProxyConfig as GqlProxyConfig, ProxyType as GqlProxyType};
use conduit_admin_graphql::scalars::TimeScalar as GqlTimeScalar;
use conduit_admin_graphql::system_settings_ext::{
    AutoBackupSettings as GqlAutoBackupSettings, BackupFrequency as GqlBackupFrequency,
    HeaderEntry as GqlHeaderEntry, SystemSettingsExtError as ExtErr, SystemSettingsExtServices,
    VideoStorageSettings as GqlVideoStorageSettings,
    WebhookNotifierConfig as GqlWebhookNotifierConfig,
    WebhookSubscription as GqlWebhookSubscription, WebhookTarget as GqlWebhookTarget,
};
use conduit_core::objects::channel_settings::HeaderEntry as CoreHeaderEntry;
use conduit_db::{DataStorageRepo, PolicyContext, Principal, RequestContext};
use conduit_services::{
    AutoBackupSettings as DomainAutoBackupSettings, BackupFrequency as DomainBackupFrequency,
    DEFAULT_VIDEO_SCAN_INTERVAL_MINUTES, DEFAULT_VIDEO_SCAN_LIMIT,
    SystemService as DomainSystemService, VideoStorageSettings as DomainVideoStorageSettings,
    WebhookNotifierConfig as DomainWebhookNotifierConfig,
    WebhookSubscription as DomainWebhookSubscription, WebhookTarget as DomainWebhookTarget,
    system_key,
};

/// GraphQL-facing [`SystemSettingsExtServices`] adapter backed by the live
/// domain `SystemService` KV store (video / webhook / auto-backup settings +
/// onboarding flags) and the configured [`DataStorageRepo`] (video-storage
/// target validation, mirroring Go `SetVideoStorageSettings`'s `DataStorage.Get`
/// check).
pub struct SystemSettingsExtAdapter {
    system: Arc<DomainSystemService>,
    data_storage_repo: Arc<dyn DataStorageRepo>,
}

impl SystemSettingsExtAdapter {
    /// Build the adapter over the shared domain system service and the shared
    /// data-storage repo (both already constructed by `wiring::build_services`).
    pub fn new(
        system: Arc<DomainSystemService>,
        data_storage_repo: Arc<dyn DataStorageRepo>,
    ) -> Self {
        Self {
            system,
            data_storage_repo,
        }
    }
}

/// Admin settings run pre-auth at the boot singleton level, so repo access
/// uses the trusted `Test` principal (`conduit-db` policy treats
/// `PrincipalKind::System | Test` as a bypass). Same convention as
/// `wiring::boot_request_context` and the other `wiring_*.rs` adapters.
fn boot_request_context() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::test()))
}

// ---------------------------------------------------------------------------
// Wire shape of the webhook target `proxy` field.
//
// The domain `WebhookTarget` keeps Go's `Proxy *httpclient.ProxyConfig`
// (`system.go:376`, json `proxy,omitempty`) untyped in its `extra` flatten-map
// (the httpclient::ProxyConfig port is pending); the GraphQL type carries a
// typed `Option<ProxyConfig>`. This wire struct bridges the two, matching the
// Go json tags (`httpclient/proxy.go:11-16`): `type`, `url,omitempty`,
// `username,omitempty`, `password,omitempty`.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WireProxyConfig {
    #[serde(rename = "type")]
    proxy_type: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    username: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    password: String,
}

/// GraphQL `ProxyType` → the stored wire literal. Go's gqlgen binds the enum
/// straight to the `httpclient.ProxyType` string, so a proxy configured via
/// GraphQL stores the enum literal verbatim (`DISABLED`/`ENVIRONMENT`/`URL`) —
/// same convention `wiring_channel_crud::proxy_type_to_wire` already pinned
/// for channel proxies.
fn proxy_type_to_wire(p: GqlProxyType) -> &'static str {
    match p {
        GqlProxyType::Disabled => "DISABLED",
        GqlProxyType::Environment => "ENVIRONMENT",
        GqlProxyType::Url => "URL",
    }
}

/// Wire literal → GraphQL `ProxyType`. Lenient: accepts both the GraphQL enum
/// literals and the lowercase Go `httpclient` constants
/// (`disabled`/`environment`/`url`, `proxy.go:6-8`); anything else degrades to
/// `DISABLED` (same fallback as `conv::proxy_type_from_str`).
fn proxy_type_from_wire(s: &str) -> GqlProxyType {
    match s {
        "ENVIRONMENT" | "environment" => GqlProxyType::Environment,
        "URL" | "url" => GqlProxyType::Url,
        _ => GqlProxyType::Disabled,
    }
}

fn proxy_to_wire(p: &GqlProxyConfig) -> WireProxyConfig {
    WireProxyConfig {
        proxy_type: proxy_type_to_wire(p.proxy_type).to_string(),
        // GraphQL null → Go zero string → omitted on marshal (omitempty).
        url: p.url.clone().unwrap_or_default(),
        username: p.username.clone().unwrap_or_default(),
        password: p.password.clone().unwrap_or_default(),
    }
}

fn proxy_from_wire(w: WireProxyConfig) -> GqlProxyConfig {
    // Empty strings were `omitempty` on the Go side — surface them as null.
    let non_empty = |s: String| if s.is_empty() { None } else { Some(s) };
    GqlProxyConfig {
        proxy_type: proxy_type_from_wire(&w.proxy_type),
        url: non_empty(w.url),
        username: non_empty(w.username),
        // Stored proxy credentials are write-only. Discard the wire value at
        // the adapter boundary as defense in depth; ProxyConfig's GraphQL
        // resolver independently guarantees that it can never be projected.
        password: None,
    }
}

// ---------------------------------------------------------------------------
// Domain <-> GraphQL conversions
// ---------------------------------------------------------------------------

/// `i64` (domain, Go `int`) → `i32` (GraphQL `Int!`), saturating at the i32
/// bounds like the other adapters (`SystemSettingsAdapter::into_gql`).
fn saturate_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v > 0 { i32::MAX } else { i32::MIN })
}

fn video_to_gql(s: DomainVideoStorageSettings) -> GqlVideoStorageSettings {
    GqlVideoStorageSettings {
        enabled: s.enabled,
        data_storage_id: saturate_i32(s.data_storage_id),
        scan_interval_minutes: saturate_i32(s.scan_interval_minutes),
        scan_limit: saturate_i32(s.scan_limit),
    }
}

fn video_to_domain(s: GqlVideoStorageSettings) -> DomainVideoStorageSettings {
    DomainVideoStorageSettings {
        enabled: s.enabled,
        data_storage_id: i64::from(s.data_storage_id),
        scan_interval_minutes: i64::from(s.scan_interval_minutes),
        scan_limit: i64::from(s.scan_limit),
    }
}

/// Clamp non-positive scan fields to the Go defaults. Applied on BOTH the
/// read and write paths, mirroring Go `VideoStorageSettings`
/// (`system.go:1436-1441`) and `SetVideoStorageSettings` (`system.go:1447-1452`).
fn clamp_video_settings(mut s: DomainVideoStorageSettings) -> DomainVideoStorageSettings {
    if s.scan_interval_minutes <= 0 {
        s.scan_interval_minutes = DEFAULT_VIDEO_SCAN_INTERVAL_MINUTES;
    }
    if s.scan_limit <= 0 {
        s.scan_limit = DEFAULT_VIDEO_SCAN_LIMIT;
    }
    s
}

fn frequency_to_gql(f: DomainBackupFrequency) -> GqlBackupFrequency {
    match f {
        DomainBackupFrequency::Daily => GqlBackupFrequency::Daily,
        DomainBackupFrequency::Weekly => GqlBackupFrequency::Weekly,
        DomainBackupFrequency::Monthly => GqlBackupFrequency::Monthly,
    }
}

fn frequency_to_domain(f: GqlBackupFrequency) -> DomainBackupFrequency {
    match f {
        GqlBackupFrequency::Daily => DomainBackupFrequency::Daily,
        GqlBackupFrequency::Weekly => DomainBackupFrequency::Weekly,
        GqlBackupFrequency::Monthly => DomainBackupFrequency::Monthly,
    }
}

fn auto_backup_to_gql(s: DomainAutoBackupSettings) -> GqlAutoBackupSettings {
    GqlAutoBackupSettings {
        enabled: s.enabled,
        frequency: frequency_to_gql(s.frequency),
        data_storage_id: saturate_i32(s.data_storage_id),
        include_channels: s.include_channels,
        include_models: s.include_models,
        include_api_keys: s.include_api_keys,
        include_model_prices: s.include_model_prices,
        include_usage_stats: s.include_usage_stats,
        include_request_logs: s.include_request_logs,
        retention_days: saturate_i32(s.retention_days),
        last_backup_at: s.last_backup_at.map(GqlTimeScalar),
        last_backup_error: s.last_backup_error,
    }
}

fn auto_backup_to_domain(s: GqlAutoBackupSettings) -> DomainAutoBackupSettings {
    DomainAutoBackupSettings {
        enabled: s.enabled,
        frequency: frequency_to_domain(s.frequency),
        data_storage_id: i64::from(s.data_storage_id),
        include_channels: s.include_channels,
        include_models: s.include_models,
        include_api_keys: s.include_api_keys,
        include_model_prices: s.include_model_prices,
        include_usage_stats: s.include_usage_stats,
        include_request_logs: s.include_request_logs,
        retention_days: i64::from(s.retention_days),
        last_backup_at: s.last_backup_at.map(|t| t.0),
        // Go `LastBackupError string json:"last_backup_error,omitempty"` — an
        // empty string is omitted on marshal, so it degrades to `None` here.
        last_backup_error: s.last_backup_error.filter(|e| !e.is_empty()),
    }
}

fn webhook_target_to_gql(t: DomainWebhookTarget) -> GqlWebhookTarget {
    // The domain type keeps Go's `proxy` field in its `extra` flatten-map;
    // decode it leniently — a malformed value degrades to null (same
    // convention as `conv::proxy_value_to_gql` for channel proxies).
    let proxy = t
        .extra
        .get("proxy")
        .cloned()
        .and_then(|v| serde_json::from_value::<WireProxyConfig>(v).ok())
        .map(proxy_from_wire);
    GqlWebhookTarget {
        name: t.name,
        enabled: t.enabled,
        url: t.url,
        proxy,
        timeout_ms: saturate_i32(t.timeout_ms),
        headers: t
            .headers
            .into_iter()
            .map(|h| GqlHeaderEntry {
                key: h.key,
                value: h.value,
            })
            .collect(),
        body: t.body,
    }
}

/// GraphQL target → domain target. Proxy serialization is infallible in
/// practice (`WireProxyConfig` is a plain string struct), but the `Result`
/// keeps the write path honest.
fn webhook_target_to_domain(t: GqlWebhookTarget) -> Result<DomainWebhookTarget, serde_json::Error> {
    let mut extra = BTreeMap::new();
    if let Some(proxy) = &t.proxy {
        // Go json tag is `proxy,omitempty` — only present when configured.
        extra.insert(
            "proxy".to_string(),
            serde_json::to_value(proxy_to_wire(proxy))?,
        );
    }
    Ok(DomainWebhookTarget {
        name: t.name,
        enabled: t.enabled,
        url: t.url,
        timeout_ms: i64::from(t.timeout_ms),
        headers: t
            .headers
            .into_iter()
            .map(|h| CoreHeaderEntry {
                key: h.key,
                value: h.value,
            })
            .collect(),
        body: t.body,
        extra,
    })
}

fn webhook_config_to_gql(cfg: DomainWebhookNotifierConfig) -> GqlWebhookNotifierConfig {
    GqlWebhookNotifierConfig {
        targets: cfg.targets.into_iter().map(webhook_target_to_gql).collect(),
        subscriptions: cfg
            .subscriptions
            .into_iter()
            .map(|s| GqlWebhookSubscription {
                event: s.event,
                target_names: s.target_names,
            })
            .collect(),
    }
}

fn webhook_config_to_domain(
    cfg: GqlWebhookNotifierConfig,
) -> Result<DomainWebhookNotifierConfig, serde_json::Error> {
    Ok(DomainWebhookNotifierConfig {
        targets: cfg
            .targets
            .into_iter()
            .map(webhook_target_to_domain)
            .collect::<Result<Vec<_>, _>>()?,
        subscriptions: cfg
            .subscriptions
            .into_iter()
            .map(|s| DomainWebhookSubscription {
                event: s.event,
                target_names: s.target_names,
                extra: BTreeMap::new(),
            })
            .collect(),
        extra: BTreeMap::new(),
    })
}

// ---------------------------------------------------------------------------
// Trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SystemSettingsExtServices for SystemSettingsExtAdapter {
    /// Go `SystemService.VideoStorageSettings` (`system.go:1420-1444`) via
    /// `queryResolver.VideoStorageSettings` (`system.resolvers.go:518`):
    /// missing key → `defaultVideoStorageSettings`; stored value → clamp.
    async fn video_storage_settings(&self) -> Result<GqlVideoStorageSettings, ExtErr> {
        let ctx = boot_request_context();
        let stored = self
            .system
            .get_json::<DomainVideoStorageSettings>(&ctx, system_key::VIDEO_STORAGE_SETTINGS)
            .await
            .map_err(|err| ExtErr::VideoStorage(err.to_string()))?;
        let settings = match stored {
            Some(s) => clamp_video_settings(s),
            // `defaultVideoStorageSettings` (`system_default.go:69-74`) is the
            // domain struct's `Default` impl.
            None => DomainVideoStorageSettings::default(),
        };
        Ok(video_to_gql(settings))
    }

    /// Go `SystemService.SetVideoStorageSettings` (`system.go:1446-1483`):
    /// clamp → validate the enabled target data storage (must exist, be
    /// non-primary, and not `database`-typed) → persist. Error strings mirror
    /// the Go `fmt.Errorf` messages; the trait's `UpdateVideoStorage` variant
    /// adds the resolver-level `failed to update video storage settings:`
    /// prefix. Go's `videoWorker.Reschedule` has no Rust scheduler yet.
    async fn set_video_storage_settings(
        &self,
        settings: GqlVideoStorageSettings,
    ) -> Result<(), ExtErr> {
        let ctx = boot_request_context();
        let settings = clamp_video_settings(video_to_domain(settings));

        if settings.enabled {
            // Go: `data_storage_id is required when video storage is enabled`.
            if settings.data_storage_id == 0 {
                return Err(ExtErr::UpdateVideoStorage(
                    "data_storage_id is required when video storage is enabled".to_string(),
                ));
            }
            // Go: `ds, err := ...DataStorage.Get(ctx, id)` →
            // `failed to get data storage: %w`. Database ids are numeric text.
            let row = self
                .data_storage_repo
                .find_data_storage(&ctx, &settings.data_storage_id.to_string())
                .await
                .map_err(|err| {
                    ExtErr::UpdateVideoStorage(format!("failed to get data storage: {err}"))
                })?
                .ok_or_else(|| {
                    ExtErr::UpdateVideoStorage(
                        "failed to get data storage: data storage not found".to_string(),
                    )
                })?;
            // Go: `if ds.Primary || ds.Type == datastorage.TypeDatabase`.
            if row.primary || row.storage_type == "database" {
                return Err(ExtErr::UpdateVideoStorage(
                    "video storage must use a non-database data storage".to_string(),
                ));
            }
        }

        self.system
            .set_json(&ctx, system_key::VIDEO_STORAGE_SETTINGS, &settings)
            .await
            .map(|_| ())
            .map_err(|err| {
                ExtErr::UpdateVideoStorage(format!("failed to set video storage settings: {err}"))
            })
    }

    /// Go `SystemService.WebhookNotifierConfig` (`system.go:1085-1106`) via
    /// `queryResolver.WebhookNotifierConfig` (`system.resolvers.go:418`). The
    /// domain method already handles missing-key → normalized empty config.
    async fn webhook_notifier_config(&self) -> Result<GqlWebhookNotifierConfig, ExtErr> {
        let ctx = boot_request_context();
        let cfg = self
            .system
            .webhook_notifier_config(&ctx)
            .await
            .map_err(|err| ExtErr::WebhookConfig(err.to_string()))?;
        Ok(webhook_config_to_gql(cfg))
    }

    /// Go `SetWebhookNotifierConfig` (`system.go:1122-1131`) via
    /// `UpdateWebhookNotifierConfig` (`system.resolvers.go:70`).
    async fn set_webhook_notifier_config(
        &self,
        config: GqlWebhookNotifierConfig,
    ) -> Result<(), ExtErr> {
        let ctx = boot_request_context();
        let domain = webhook_config_to_domain(config)
            .map_err(|err| ExtErr::UpdateWebhookConfig(err.to_string()))?;
        self.system
            .set_webhook_notifier_config(&ctx, domain)
            .await
            .map(|_| ())
            .map_err(|err| ExtErr::UpdateWebhookConfig(err.to_string()))
    }

    /// Go `SystemService.AutoBackupSettings` (`system.go:1362-1403`) via
    /// `queryResolver.AutoBackupSettings` (`backup.resolvers.go:140`): missing
    /// key → `defaultAutoBackupSettings`; stored value zero-fills missing
    /// fields (the domain struct's field-level `#[serde(default)]` mirrors the
    /// Go `autoBackupSettingsJSON` fallbacks, which are all `false`/zero).
    async fn auto_backup_settings(&self) -> Result<GqlAutoBackupSettings, ExtErr> {
        let ctx = boot_request_context();
        let stored = self
            .system
            .get_json::<DomainAutoBackupSettings>(&ctx, system_key::AUTO_BACKUP_SETTINGS)
            .await
            .map_err(|err| ExtErr::AutoBackup(err.to_string()))?;
        // `defaultAutoBackupSettings` (`system_default.go:57-67`) is the
        // domain struct's `Default` impl (channels/models/prices on, 30 days).
        Ok(auto_backup_to_gql(stored.unwrap_or_default()))
    }

    /// Go `SetAutoBackupSettings` (`system.go:1405-1418`) — the write half of
    /// `UpdateAutoBackupSettings` (`backup.resolvers.go:55`). The resolver
    /// performs the read-merge; this persists the merged settings verbatim
    /// (including the preserved `last_backup_at`/`last_backup_error`). Go's
    /// `backupService.Reschedule` has no Rust scheduler yet.
    async fn set_auto_backup_settings(
        &self,
        settings: GqlAutoBackupSettings,
    ) -> Result<(), ExtErr> {
        let ctx = boot_request_context();
        let domain = auto_backup_to_domain(settings);
        self.system
            .set_json(&ctx, system_key::AUTO_BACKUP_SETTINGS, &domain)
            .await
            .map(|_| ())
            .map_err(|err| {
                ExtErr::UpdateAutoBackup(format!("failed to set auto backup settings: {err}"))
            })
    }

    /// Go `CompleteSystemModelSettingOnboarding` (`system_onboarding.go:89-106`)
    /// via `system.resolvers.go:123`. Delegates to the domain port, which does
    /// the read-modify-write on the `system_onboarded` record.
    async fn complete_system_model_setting_onboarding(&self) -> Result<(), ExtErr> {
        let ctx = boot_request_context();
        self.system
            .complete_system_model_setting_onboarding(&ctx)
            .await
            .map_err(|err| ExtErr::CompleteModelOnboarding(err.to_string()))
    }

    /// Go `CompleteAutoDisableChannelOnboarding` (`system_onboarding.go:109-126`)
    /// via `system.resolvers.go:133`.
    async fn complete_auto_disable_channel_onboarding(&self) -> Result<(), ExtErr> {
        let ctx = boot_request_context();
        self.system
            .complete_auto_disable_channel_onboarding(&ctx)
            .await
            .map_err(|err| ExtErr::CompleteAutoDisableOnboarding(err.to_string()))
    }
}

#[cfg(test)]
mod proxy_conversion_tests {
    use super::*;

    #[test]
    fn webhook_proxy_password_is_discarded_on_read() {
        let proxy = proxy_from_wire(WireProxyConfig {
            proxy_type: "URL".to_owned(),
            url: "http://proxy.example".to_owned(),
            username: "operator".to_owned(),
            password: "must-not-leak".to_owned(),
        });

        assert_eq!(proxy.url.as_deref(), Some("http://proxy.example"));
        assert_eq!(proxy.username.as_deref(), Some("operator"));
        assert_eq!(proxy.password, None);
    }
}
