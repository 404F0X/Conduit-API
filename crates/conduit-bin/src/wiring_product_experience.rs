//! Production adapter for the Conduit API simple/enterprise product projection.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use conduit_admin_graphql::product_experience::{
    FeatureOverride, ProductExperienceError, ProductExperienceServices, ProductExperienceSettings,
    ProductFeature, ProductMode, UpdateProductExperienceSettingsInput,
};
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_services::{SystemService, system_key};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct StoredProductExperienceSettings {
    mode: String,
    revision: u64,
    overrides: BTreeMap<String, StoredOverride>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct StoredOverride {
    visibility: FeatureOverride,
    execution: FeatureOverride,
}

pub(crate) const FEATURES: &[(&str, bool, bool)] = &[
    ("operations_reports", false, true),
    ("automatic_model_sync", false, true),
    ("automatic_provider_observation", false, true),
    ("redemption_issuance", false, true),
    ("subscription_management", false, true),
    ("projects", false, true),
    ("roles", false, true),
    ("change_sets", false, true),
    ("groups", false, true),
    ("system", false, true),
    ("prompt_protection", false, true),
    ("data_storages", false, true),
    ("prompts", false, true),
    ("traces", false, true),
    ("threads", false, true),
    ("playground", false, true),
];

fn resolve(value: FeatureOverride, inherited: bool) -> bool {
    match value {
        FeatureOverride::Inherit => inherited,
        FeatureOverride::On => true,
        FeatureOverride::Off => false,
    }
}

/// Reads the authority on every worker invocation. Missing settings retain the
/// existing execution behavior independently of the presentation mode.
pub(crate) async fn feature_enabled(pool: &sqlx::PgPool, id: &str) -> Result<bool, String> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM systems WHERE key=$1 AND deleted_at=0")
            .bind(system_key::PRODUCT_EXPERIENCE_SETTINGS)
            .fetch_optional(pool)
            .await
            .map_err(|_| "feature settings unavailable".to_string())?;
    let settings: StoredProductExperienceSettings = raw
        .map(|raw| serde_json::from_str(&raw))
        .transpose()
        .map_err(|_| "invalid feature settings".to_string())?
        .unwrap_or_default();
    let (_, _, inherited) = FEATURES
        .iter()
        .find(|(name, _, _)| *name == id)
        .ok_or_else(|| "unknown feature".to_string())?;
    Ok(resolve(
        settings
            .overrides
            .get(id)
            .map(|v| v.execution)
            .unwrap_or_default(),
        *inherited,
    ))
}

pub(crate) async fn require_feature_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &str,
) -> Result<(), String> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM systems WHERE key=$1 AND deleted_at=0")
            .bind(system_key::PRODUCT_EXPERIENCE_SETTINGS)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|_| "feature settings unavailable")?;
    let stored: StoredProductExperienceSettings = raw
        .map(|v| serde_json::from_str(&v))
        .transpose()
        .map_err(|_| "invalid feature settings")?
        .unwrap_or_default();
    let inherited = FEATURES
        .iter()
        .find(|(name, _, _)| *name == id)
        .map(|(_, _, v)| *v)
        .ok_or("unknown feature")?;
    if resolve(
        stored
            .overrides
            .get(id)
            .map(|v| v.execution)
            .unwrap_or_default(),
        inherited,
    ) {
        Ok(())
    } else {
        Err(format!("FEATURE_DISABLED: {id}"))
    }
}

impl StoredProductExperienceSettings {
    fn product_mode(&self) -> ProductMode {
        match self.mode.trim().to_ascii_lowercase().as_str() {
            "simple" => ProductMode::Simple,
            // Missing, unknown and future values remain compatible with
            // existing Conduit API deployments by projecting enterprise mode.
            _ => ProductMode::Enterprise,
        }
    }

    fn set_product_mode(&mut self, mode: ProductMode) {
        self.mode = match mode {
            ProductMode::Simple => "simple",
            ProductMode::Enterprise => "enterprise",
        }
        .to_string();
    }

    fn into_graphql(self) -> ProductExperienceSettings {
        let mode = self.product_mode();
        ProductExperienceSettings {
            mode,
            revision: self.revision.to_string(),
            features: FEATURES
                .iter()
                .map(|(id, visible, enabled)| {
                    let selection = self.overrides.get(*id).cloned().unwrap_or_default();
                    ProductFeature {
                        id: (*id).into(),
                        visibility: selection.visibility,
                        execution: selection.execution,
                        visible: resolve(
                            selection.visibility,
                            mode == ProductMode::Enterprise || *visible,
                        ),
                        enabled: resolve(selection.execution, *enabled),
                        dependencies: vec![],
                    }
                })
                .collect(),
        }
    }

    fn apply(
        &mut self,
        input: UpdateProductExperienceSettingsInput,
    ) -> Result<(), ProductExperienceError> {
        if input
            .expected_revision
            .as_deref()
            .is_some_and(|v| v != self.revision.to_string())
        {
            return Err(ProductExperienceError::Update("REVISION_CONFLICT".into()));
        }
        self.set_product_mode(input.mode);
        for item in input.overrides.unwrap_or_default() {
            if !FEATURES.iter().any(|(id, _, _)| *id == item.id) {
                return Err(ProductExperienceError::Update("unknown feature".into()));
            }
            if item.execution.is_some()
                && !matches!(
                    item.id.as_str(),
                    "operations_reports"
                        | "automatic_model_sync"
                        | "automatic_provider_observation"
                        | "redemption_issuance"
                        | "subscription_management"
                )
            {
                return Err(ProductExperienceError::Update(
                    "this feature has presentation controls only".into(),
                ));
            }
            let entry = self.overrides.entry(item.id.clone()).or_default();
            if let Some(value) = item.visibility {
                entry.visibility = value;
            }
            if let Some(value) = item.execution {
                entry.execution = value;
            }
            if entry.visibility == FeatureOverride::Inherit
                && entry.execution == FeatureOverride::Inherit
            {
                self.overrides.remove(&item.id);
            }
        }
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or_else(|| ProductExperienceError::Update("revision exhausted".into()))?;
        Ok(())
    }
}

pub(crate) fn governed_product_form(
    input: UpdateProductExperienceSettingsInput,
    old: Value,
) -> Result<Value, String> {
    let mut stored: StoredProductExperienceSettings = if old.is_null() {
        Default::default()
    } else {
        serde_json::from_value(old).map_err(|_| "invalid product settings")?
    };
    stored.apply(input).map_err(|e| e.to_string())?;
    serde_json::to_value(stored).map_err(|_| "invalid product settings".into())
}

pub struct ProductExperienceAdapter {
    system: Arc<SystemService>,
    pool: Option<sqlx::PgPool>,
}

impl ProductExperienceAdapter {
    pub fn new(system: Arc<SystemService>) -> Self {
        Self { system, pool: None }
    }
    pub(crate) fn with_pool(mut self, pool: sqlx::PgPool) -> Self {
        self.pool = Some(pool);
        self
    }
}

fn boot_request_context() -> RequestContext {
    RequestContext::new(PolicyContext::new(Principal::test()))
}

#[async_trait]
impl ProductExperienceServices for ProductExperienceAdapter {
    async fn settings(&self) -> Result<ProductExperienceSettings, ProductExperienceError> {
        let ctx = boot_request_context();
        let stored = self
            .system
            .get_json::<StoredProductExperienceSettings>(
                &ctx,
                system_key::PRODUCT_EXPERIENCE_SETTINGS,
            )
            .await
            .map_err(|error| ProductExperienceError::Read(error.to_string()))?
            .unwrap_or_default();
        Ok(stored.into_graphql())
    }

    async fn update_settings(
        &self,
        input: UpdateProductExperienceSettingsInput,
    ) -> Result<ProductExperienceSettings, ProductExperienceError> {
        let ctx = boot_request_context();
        if let Some(pool) = &self.pool {
            let mut tx = pool.begin().await.map_err(|_| {
                ProductExperienceError::Update("settings transaction unavailable".into())
            })?;
            let policy = crate::wiring_governance::lock_policy(&mut tx)
                .await
                .map_err(ProductExperienceError::Update)?;
            if policy.requires("system_configuration")
                && input
                    .overrides
                    .as_ref()
                    .is_some_and(|overrides| overrides.iter().any(|v| v.execution.is_some()))
            {
                return Err(ProductExperienceError::Update(
                    "APPROVAL_REQUIRED: execution changes require a governed proposal".into(),
                ));
            }
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(system_key::PRODUCT_EXPERIENCE_SETTINGS)
                .execute(&mut *tx)
                .await
                .map_err(|_| ProductExperienceError::Update("settings lock unavailable".into()))?;
            let raw: Option<String> = sqlx::query_scalar(
                "SELECT value FROM systems WHERE key=$1 AND deleted_at=0 FOR UPDATE",
            )
            .bind(system_key::PRODUCT_EXPERIENCE_SETTINGS)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ProductExperienceError::Read("settings unavailable".into()))?;
            let mut stored: StoredProductExperienceSettings = raw
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .map_err(|_| ProductExperienceError::Read("invalid settings".into()))?
                .unwrap_or_default();
            stored.apply(input)?;
            let value = serde_json::to_string(&stored)
                .map_err(|_| ProductExperienceError::Update("invalid settings".into()))?;
            sqlx::query("INSERT INTO systems(key,value,created_at,updated_at) VALUES($1,$2,now(),now()) ON CONFLICT(key) DO UPDATE SET value=EXCLUDED.value,updated_at=now(),deleted_at=0").bind(system_key::PRODUCT_EXPERIENCE_SETTINGS).bind(value).execute(&mut *tx).await.map_err(|_|ProductExperienceError::Update("settings write failed".into()))?;
            tx.commit()
                .await
                .map_err(|_| ProductExperienceError::Update("settings commit failed".into()))?;
            return Ok(stored.into_graphql());
        }
        let mut stored = self
            .system
            .get_json::<StoredProductExperienceSettings>(
                &ctx,
                system_key::PRODUCT_EXPERIENCE_SETTINGS,
            )
            .await
            .map_err(|error| ProductExperienceError::Read(error.to_string()))?
            .unwrap_or_default();
        stored.apply(input)?;
        let saved = self
            .system
            .set_json(&ctx, system_key::PRODUCT_EXPERIENCE_SETTINGS, &stored)
            .await
            .map_err(|error| ProductExperienceError::Update(error.to_string()))?;
        Ok(saved.into_graphql())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_cache::{Cache, NoopCache};
    use conduit_db::InMemorySystemRepo;

    fn adapter() -> ProductExperienceAdapter {
        let cache: Arc<dyn Cache> = Arc::new(NoopCache::new());
        let system = Arc::new(SystemService::from_system_repo(
            Arc::new(InMemorySystemRepo::new()),
            cache,
        ));
        ProductExperienceAdapter::new(system)
    }

    #[tokio::test]
    async fn absent_and_unknown_values_fall_back_to_enterprise() {
        let adapter = adapter();
        assert_eq!(
            adapter.settings().await.expect("default").mode,
            ProductMode::Enterprise
        );

        let ctx = boot_request_context();
        adapter
            .system
            .set_json(
                &ctx,
                system_key::PRODUCT_EXPERIENCE_SETTINGS,
                &StoredProductExperienceSettings {
                    mode: "future-mode".to_string(),
                    extra: BTreeMap::new(),
                    ..Default::default()
                },
            )
            .await
            .expect("seed unknown mode");
        assert_eq!(
            adapter.settings().await.expect("fallback").mode,
            ProductMode::Enterprise
        );
    }

    #[tokio::test]
    async fn update_round_trips_and_preserves_unknown_fields() {
        let adapter = adapter();
        let ctx = boot_request_context();
        adapter
            .system
            .set_json(
                &ctx,
                system_key::PRODUCT_EXPERIENCE_SETTINGS,
                &serde_json::json!({"mode": "enterprise", "future_flag": true}),
            )
            .await
            .expect("seed settings");

        let updated = adapter
            .update_settings(UpdateProductExperienceSettingsInput {
                mode: ProductMode::Simple,
                expected_revision: None,
                overrides: None,
            })
            .await
            .expect("update settings");
        assert_eq!(updated.mode, ProductMode::Simple);

        let raw = adapter
            .system
            .get_system_value(&ctx, system_key::PRODUCT_EXPERIENCE_SETTINGS)
            .await
            .expect("read raw")
            .expect("stored value");
        assert_eq!(raw["mode"], "simple");
        assert_eq!(raw["future_flag"], true);
    }
}
