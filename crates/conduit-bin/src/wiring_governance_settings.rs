//! Fixed storage DTOs for settings proposals. Keys never select an arbitrary
//! repository field and secrets remain in the private command store.
use crate::wiring_governance as governance;
use crate::wiring_governance_inputs::GraphInput;
use conduit_admin_graphql::{system as gql, system_settings_ext as ext};
use conduit_services::system_service as sys;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};

#[derive(Clone, Serialize)]
pub(crate) enum SettingForm {
    ProductExperience(
        GraphInput<conduit_admin_graphql::product_experience::UpdateProductExperienceSettingsInput>,
    ),
    General(GraphInput<gql::UpdateSystemGeneralSettingsInput>),
    Model(GraphInput<gql::UpdateSystemModelSettingsInput>),
    Channel(GraphInput<conduit_admin_graphql::system_ext::UpdateSystemChannelSettingsInput>),
    Security(GraphInput<gql::UpdateSecuritySettingsInput>),
    Storage(GraphInput<gql::UpdateStoragePolicyInput>),
    Retry(GraphInput<gql::UpdateRetryPolicyInput>),
    Webhook(GraphInput<ext::WebhookNotifierConfigInput>),
    Video(GraphInput<ext::UpdateVideoStorageSettingsInput>),
    Backup(GraphInput<ext::UpdateAutoBackupSettingsInput>),
    Quota(GraphInput<conduit_admin_graphql::mutation::UpdateQuotaEnforcementSettingsInput>),
    ProxySave(GraphInput<gql::SaveProxyPresetInput>),
    ProxyDelete(ProxyDelete),
    Brand(GraphInput<gql::UpdateBrandSettingsInput>),
    PublicHealth(
        GraphInput<
            conduit_admin_graphql::channel_probe_ext::UpdatePublicChannelHealthSettingsInput,
        >,
    ),
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProxyDelete {
    url: String,
}
impl SettingForm {
    pub(crate) fn parse(action: &str, value: Value) -> Result<Self, String> {
        fn form<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
            serde_json::from_value(value)
                .map_err(|_| "VALIDATION_FAILED: invalid fixed settings form".into())
        }
        Ok(match action {
            "system.product_experience_form" => Self::ProductExperience(form(value)?),
            "system.general_form" => Self::General(form(value)?),
            "system.model_form" => Self::Model(form(value)?),
            "system.channel_form" => Self::Channel(form(value)?),
            "system.security_form" => Self::Security(form(value)?),
            "system.storage_form" => Self::Storage(form(value)?),
            "system.retry_form" => Self::Retry(form(value)?),
            "system.webhook_form" => Self::Webhook(form(value)?),
            "system.video_form" => Self::Video(form(value)?),
            "system.backup_form" => Self::Backup(form(value)?),
            "system.quota_form" => Self::Quota(form(value)?),
            "system.proxy_save" => Self::ProxySave(form(value)?),
            "system.proxy_delete" => Self::ProxyDelete(form(value)?),
            "system.public_health_form" => Self::PublicHealth(form(value)?),
            "system.brand_form" => Self::Brand(form(value)?),
            _ => return Err("GOVERNANCE_UNSUPPORTED".into()),
        })
    }
    pub(crate) fn key(&self) -> &'static str {
        use sys::system_key as k;
        match self {
            Self::ProductExperience(_) => k::PRODUCT_EXPERIENCE_SETTINGS,
            Self::General(_) => k::GENERAL_SETTINGS,
            Self::Model(_) => k::MODEL_SETTINGS,
            Self::Channel(_) | Self::PublicHealth(_) => k::CHANNEL_SETTINGS,
            Self::Security(_) => k::SECURITY_SETTINGS,
            Self::Storage(_) => k::STORAGE_POLICY,
            Self::Retry(_) => k::RETRY_POLICY,
            Self::Webhook(_) => k::WEBHOOK_NOTIFIER_CONFIG,
            Self::Video(_) => k::VIDEO_STORAGE_SETTINGS,
            Self::Backup(_) => k::AUTO_BACKUP_SETTINGS,
            Self::Quota(_) => k::QUOTA_ENFORCEMENT_SETTINGS,
            Self::ProxySave(_) | Self::ProxyDelete(_) => k::PROXY_PRESETS,
            Self::Brand(_) => k::BRAND_NAME,
        }
    }
    pub(crate) fn keys(&self) -> Vec<&'static str> {
        if matches!(self, Self::Brand(_)) {
            vec![
                sys::system_key::BRAND_NAME,
                sys::system_key::BRAND_LOGO,
                sys::system_key::TITLE,
            ]
        } else {
            vec![self.key()]
        }
    }
    pub(crate) fn storage_id(&self) -> Option<i64> {
        match self {
            Self::Video(v) => v.0.data_storage_id.map(i64::from),
            Self::Backup(v) => v.0.data_storage_id.map(i64::from),
            _ => None,
        }
    }
    fn lower(&self, old: Value) -> Result<Value, String> {
        fn stored<T: serde::de::DeserializeOwned + Default>(old: Value) -> Result<T, String> {
            if old.is_null() {
                Ok(T::default())
            } else {
                serde_json::from_value(old).map_err(|_| "invalid stored settings".into())
            }
        }
        fn json<T: Serialize>(v: T) -> Result<Value, String> {
            serde_json::to_value(v).map_err(|_| "invalid settings form".into())
        }
        match self {
            Self::ProductExperience(v) => {
                crate::wiring_product_experience::governed_product_form(v.0.clone(), old)
            }
            Self::General(v) => crate::wiring::governed_general_form(v.0.clone(), old),
            Self::Model(v) => crate::wiring::governed_model_form(v.0.clone(), old),
            Self::Channel(v) => crate::wiring::governed_channel_form(v.0.clone(), old),
            Self::Retry(v) => crate::wiring::governed_retry_form(v.0.clone()),
            Self::Security(v) => {
                let mut old: sys::SecuritySettings = stored(old)?;
                old.extra.clear();
                if let Some(ips) = &v.0.blocked_ips {
                    old.blocked_ips = ips.clone();
                }
                if let Some(show) = v.0.show_request_log_ip_ban_icon {
                    old.show_request_log_ip_ban_icon = show;
                }
                json(old)
            }
            Self::Storage(v) => {
                let v: gql::StoragePolicy = v.0.clone().into();
                json(sys::StoragePolicy {
                    store_chunks: v.store_chunks,
                    live_preview: v.live_preview,
                    store_request_headers: v.store_request_headers,
                    store_request_body: v.store_request_body,
                    store_response_body: v.store_response_body,
                    cleanup_options: v
                        .cleanup_options
                        .into_iter()
                        .map(|o| conduit_services::CleanupOption {
                            resource_type: o.resource_type,
                            enabled: o.enabled,
                            cleanup_days: i64::from(o.cleanup_days),
                        })
                        .collect(),
                })
            }
            Self::Webhook(v) => {
                crate::wiring_system_settings_ext::governed_webhook_form(v.0.clone(), old)
            }
            Self::Backup(v) => {
                crate::wiring_system_settings_ext::governed_backup_form(v.0.clone(), old)
            }
            Self::Video(v) => json(crate::wiring_system_settings_ext::clamp_video_settings(
                crate::wiring_system_settings_ext::video_to_domain(v.0.into()),
            )),
            Self::Quota(v) => {
                let old: Quota = if old.is_null() {
                    Quota {
                        enabled: false,
                        mode: "exhausted_only".into(),
                    }
                } else {
                    serde_json::from_value(old).map_err(|_| "invalid quota settings")?
                };
                json(Quota {
                    enabled: v.0.enabled.unwrap_or(old.enabled),
                    mode: v
                        .0
                        .mode
                        .map(|m| m.as_json_value().into())
                        .unwrap_or(old.mode),
                })
            }
            Self::ProxySave(v) => {
                let mut old: Vec<sys::ProxyPreset> = stored(old)?;
                let value = sys::ProxyPreset {
                    name: v.0.name.clone(),
                    url: v.0.url.clone(),
                    username: v.0.username.clone(),
                    password: v.0.password.clone(),
                    extra: Default::default(),
                };
                if let Some(existing) = old.iter_mut().find(|p| p.url == value.url) {
                    *existing = value;
                } else {
                    old.push(value);
                }
                json(old)
            }
            Self::ProxyDelete(v) => {
                let mut old: Vec<sys::ProxyPreset> = stored(old)?;
                old.retain(|p| p.url != v.url);
                json(old)
            }
            Self::Brand(_) => Err("brand form uses multiple fixed settings".into()),
            Self::PublicHealth(v) => {
                let mut old: sys::SystemChannelSettings = stored(old)?;
                old.extra
                    .insert("expose_public_channel_health".into(), json!(v.0.enabled));
                json(old)
            }
        }
    }
    pub(crate) async fn validate(&self, tx: &mut Transaction<'_, Postgres>) -> Result<(), String> {
        if let Self::Brand(v) = self {
            for (key, value) in [
                (sys::system_key::BRAND_NAME, &v.0.brand_name),
                (sys::system_key::BRAND_LOGO, &v.0.brand_logo),
                (sys::system_key::TITLE, &v.0.title),
            ] {
                if let Some(value) = value {
                    normalize(key, &json!(value))?;
                }
            }
            return Ok(());
        }
        let old: Option<String> = sqlx::query_scalar(
            "SELECT value FROM systems WHERE key=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(self.key())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| "settings lookup failed")?;
        let old = old
            .map(|v| serde_json::from_str(&v).map_err(|_| "invalid stored setting"))
            .transpose()?
            .unwrap_or(Value::Null);
        normalize(self.key(), &self.lower(old)?)?;
        Ok(())
    }
    pub(crate) async fn apply(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor: i64,
    ) -> Result<(), String> {
        if let Self::Brand(v) = self {
            for (key, value) in [
                (sys::system_key::BRAND_NAME, &v.0.brand_name),
                (sys::system_key::BRAND_LOGO, &v.0.brand_logo),
                (sys::system_key::TITLE, &v.0.title),
            ] {
                if let Some(value) = value {
                    apply(tx, actor, key, &json!(value)).await?;
                }
            }
            return Ok(());
        }
        let old: Option<String> = sqlx::query_scalar(
            "SELECT value FROM systems WHERE key=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(self.key())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| "settings lookup failed")?;
        let old = old
            .map(|v| serde_json::from_str(&v).map_err(|_| "invalid stored setting"))
            .transpose()?
            .unwrap_or(Value::Null);
        apply(tx, actor, self.key(), &self.lower(old)?).await
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeneralSettings {
    accounting_currency_code: String,
    timezone: String,
    credit_display_name: String,
    credits_per_accounting_unit: rust_decimal::Decimal,
    exchange_rates: Vec<conduit_core::objects::money::CurrencyExchangeRate>,
    accounting_rate_version: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Quota {
    enabled: bool,
    mode: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Product {
    mode: String,
    revision: u64,
    overrides: std::collections::BTreeMap<String, Override>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Override {
    visibility: conduit_admin_graphql::product_experience::FeatureOverride,
    execution: conduit_admin_graphql::product_experience::FeatureOverride,
}
fn typed<T: serde::de::DeserializeOwned + Serialize>(value: &Value) -> Result<Value, String> {
    serde_json::from_value::<T>(value.clone())
        .and_then(serde_json::to_value)
        .map_err(|_| "VALIDATION_FAILED: invalid typed setting".into())
}
fn keys(value: &Value, allowed: &[&str]) -> Result<(), String> {
    if value
        .as_object()
        .is_none_or(|v| v.keys().any(|k| !allowed.contains(&k.as_str())))
    {
        Err("VALIDATION_FAILED: unknown setting field".into())
    } else {
        Ok(())
    }
}
pub(crate) fn normalize(key: &str, value: &Value) -> Result<Value, String> {
    use sys::system_key as k;
    Ok(match key {
        k::GENERAL_SETTINGS => {
            let v: GeneralSettings =
                serde_json::from_value(value.clone()).map_err(|_| "invalid financial setting")?;
            let settings = conduit_core::objects::money::AccountingSettings {
                accounting_currency: v.accounting_currency_code.clone(),
                credit_display_name: v.credit_display_name.clone(),
                credits_per_accounting_unit: v.credits_per_accounting_unit,
                exchange_rates: v.exchange_rates.clone(),
                version: v.accounting_rate_version,
            };
            settings
                .validate()
                .map_err(|_| "invalid accounting configuration")?;
            typed::<GeneralSettings>(value)?
        }
        k::MODEL_SETTINGS => {
            keys(
                value,
                &[
                    "fallback_to_channels_on_model_not_found",
                    "query_all_channel_models",
                    "default_model_api_include_all",
                    "auto_reasoning_effort",
                    "model_blacklist_regex",
                    "developer_settings",
                ],
            )?;
            typed::<conduit_core::objects::SystemModelSettings>(value)?
        }
        k::CHANNEL_SETTINGS => {
            keys(
                value,
                &["probe", "auto_sync", "expose_public_channel_health"],
            )?;
            let mut v: sys::SystemChannelSettings =
                serde_json::from_value(value.clone()).map_err(|_| "invalid channel setting")?;
            sys::normalize_channel_settings(&mut v);
            serde_json::to_value(v).map_err(|_| "invalid channel setting")?
        }
        k::SECURITY_SETTINGS => {
            keys(value, &["blocked_ips", "show_request_log_ip_ban_icon"])?;
            let mut v: sys::SecuritySettings =
                serde_json::from_value(value.clone()).map_err(|_| "invalid security setting")?;
            sys::normalize_security_settings(&mut v);
            serde_json::to_value(v).map_err(|_| "invalid security setting")?
        }
        k::RETRY_POLICY => crate::wiring::normalize_governed_retry(value.clone())?,
        k::STORAGE_POLICY => {
            keys(
                value,
                &[
                    "store_chunks",
                    "live_preview",
                    "store_request_headers",
                    "store_request_body",
                    "store_response_body",
                    "cleanup_options",
                ],
            )?;
            let v: sys::StoragePolicy =
                serde_json::from_value(value.clone()).map_err(|_| "invalid storage policy")?;
            if v.cleanup_options.iter().any(|v| v.cleanup_days <= 0) {
                return Err("cleanup days must be positive".into());
            }
            serde_json::to_value(v).map_err(|_| "invalid storage policy")?
        }
        k::PROXY_PRESETS => {
            let values = value.as_array().ok_or("proxy presets must be an array")?;
            let mut seen = std::collections::BTreeSet::new();
            for v in values {
                keys(v, &["name", "url", "username", "password"])?;
                let p: sys::ProxyPreset =
                    serde_json::from_value(v.clone()).map_err(|_| "invalid proxy preset")?;
                if p.url.trim().is_empty() || !seen.insert(p.url) {
                    return Err("proxy URL is empty or repeated".into());
                }
            }
            typed::<Vec<sys::ProxyPreset>>(value)?
        }
        k::WEBHOOK_NOTIFIER_CONFIG => {
            keys(value, &["enabled", "targets", "subscriptions"])?;
            for target in value
                .get("targets")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                keys(
                    target,
                    &[
                        "name",
                        "url",
                        "headers",
                        "proxy",
                        "enabled",
                        "timeout_ms",
                        "body",
                    ],
                )?;
            }
            typed::<sys::WebhookNotifierConfig>(value)?
        }
        k::VIDEO_STORAGE_SETTINGS => {
            keys(
                value,
                &[
                    "enabled",
                    "data_storage_id",
                    "scan_interval_minutes",
                    "scan_limit",
                ],
            )?;
            typed::<conduit_services::VideoStorageSettings>(value)?
        }
        k::AUTO_BACKUP_SETTINGS => {
            keys(
                value,
                &[
                    "enabled",
                    "frequency",
                    "include_channels",
                    "include_models",
                    "include_api_keys",
                    "include_model_prices",
                    "include_usage_stats",
                    "include_request_logs",
                    "data_storage_id",
                    "retention_days",
                    "last_backup_at",
                    "last_backup_error",
                ],
            )?;
            typed::<conduit_services::AutoBackupSettings>(value)?
        }
        k::QUOTA_ENFORCEMENT_SETTINGS => {
            let q: Quota =
                serde_json::from_value(value.clone()).map_err(|_| "invalid quota setting")?;
            if !matches!(q.mode.as_str(), "exhausted_only" | "de_prioritize") {
                return Err("invalid quota mode".into());
            }
            typed::<Quota>(value)?
        }
        k::PRODUCT_EXPERIENCE_SETTINGS => {
            let p: Product =
                serde_json::from_value(value.clone()).map_err(|_| "invalid feature settings")?;
            if !matches!(p.mode.as_str(), "simple" | "enterprise") {
                return Err("invalid mode".into());
            }
            for (id, v) in &p.overrides {
                if !crate::wiring_product_experience::FEATURES
                    .iter()
                    .any(|(name, _, _)| name == id)
                {
                    return Err("unknown feature".into());
                }
                if !matches!(
                    id.as_str(),
                    "operations_reports"
                        | "automatic_model_sync"
                        | "automatic_provider_observation"
                        | "redemption_issuance"
                        | "subscription_management"
                ) && v.execution
                    != conduit_admin_graphql::product_experience::FeatureOverride::Inherit
                {
                    return Err("feature only supports visibility controls".into());
                }
            }
            typed::<Product>(value)?
        }
        governance::KEY => {
            let p: governance::Policy =
                serde_json::from_value(value.clone()).map_err(|_| "invalid governance settings")?;
            if p.legacy_review_scopes {
                return Err("legacy review privileges cannot be newly granted".into());
            }
            for kind in &p.review_kinds {
                governance::kind(kind)?;
            }
            typed::<governance::Policy>(value)?
        }
        k::BRAND_NAME | k::BRAND_LOGO | k::TITLE => typed::<String>(value)?,
        k::USER_AGENT_PASS_THROUGH | k::PASS_THROUGH => typed::<bool>(value)?,
        k::DEFAULT_DATA_STORAGE_ID => typed::<i64>(value)?,
        _ => return Err("GOVERNANCE_UNSUPPORTED: unsupported setting key".into()),
    })
}
pub(crate) async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    actor: i64,
    key: &str,
    value: &Value,
) -> Result<(), String> {
    let mut value = normalize(key, value)?;
    if key == governance::KEY {
        governance::authorize(tx, actor, "manage_governance").await?;
        let old = governance::lock_policy(tx).await?;
        let p: governance::Policy =
            serde_json::from_value(value.clone()).map_err(|_| "invalid governance policy")?;
        if p.revision != old.revision + 1 {
            return Err("REVISION_CONFLICT".into());
        }
        governance::ensure_policy_viable(tx, &p).await?;
    }
    if key == sys::system_key::PRODUCT_EXPERIENCE_SETTINGS {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT value FROM systems WHERE key=$1 FOR UPDATE")
                .bind(key)
                .fetch_optional(&mut **tx)
                .await
                .map_err(|_| "feature settings lookup failed")?;
        let old = raw
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or(json!({"revision":0}));
        if value["revision"].as_u64() != Some(old["revision"].as_u64().unwrap_or(0) + 1) {
            return Err("REVISION_CONFLICT".into());
        }
    }
    if key == sys::system_key::GENERAL_SETTINGS {
        return crate::wiring::apply_general_setting_in_tx(tx, Some(actor), value).await;
    }
    if key == sys::system_key::AUTO_BACKUP_SETTINGS {
        let raw: Option<String> = sqlx::query_scalar(
            "SELECT value FROM systems WHERE key=$1 AND deleted_at=0 FOR UPDATE",
        )
        .bind(key)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| "backup settings lookup failed")?;
        let previous = raw
            .and_then(|v| serde_json::from_str::<Value>(&v).ok())
            .unwrap_or(Value::Null);
        for field in ["last_backup_at", "last_backup_error"] {
            if let Some(old) = previous.get(field) {
                value[field] = old.clone();
            } else if let Some(m) = value.as_object_mut() {
                m.remove(field);
            }
        }
    }
    if matches!(
        key,
        sys::system_key::DEFAULT_DATA_STORAGE_ID | sys::system_key::VIDEO_STORAGE_SETTINGS
    ) {
        let id = if key == sys::system_key::DEFAULT_DATA_STORAGE_ID {
            value.as_i64()
        } else {
            value["data_storage_id"].as_i64()
        };
        if value
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            let id = id.ok_or("data storage ID is required")?;
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM data_storages WHERE id=$1 AND deleted_at=0)",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await
            .map_err(|_| "data storage lookup failed")?;
            if !exists {
                return Err("data storage does not exist".into());
            }
        }
    }
    if key == sys::system_key::VIDEO_STORAGE_SETTINGS && value["enabled"].as_bool() == Some(true) {
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM data_storages WHERE id=$1 AND deleted_at=0 AND NOT \"primary\" AND type<>'database')").bind(value["data_storage_id"].as_i64()).fetch_one(&mut **tx).await.map_err(|_|"video storage validation failed")?;
        if !valid {
            return Err("video storage must use a non-database, non-primary data storage".into());
        }
    }
    let raw = if let Value::String(s) = value {
        s
    } else {
        value.to_string()
    };
    sqlx::query("INSERT INTO systems(key,value,created_at,updated_at) VALUES($1,$2,now(),now()) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,updated_at=now(),deleted_at=0").bind(key).bind(raw).execute(&mut **tx).await.map_err(|_|"setting application failed")?;
    Ok(())
}
