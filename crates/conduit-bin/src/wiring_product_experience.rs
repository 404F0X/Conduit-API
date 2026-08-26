//! Production adapter for the Conduit API simple/enterprise product projection.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use conduit_admin_graphql::product_experience::{
    ProductExperienceError, ProductExperienceServices, ProductExperienceSettings, ProductMode,
    UpdateProductExperienceSettingsInput,
};
use conduit_db::{PolicyContext, Principal, RequestContext};
use conduit_services::{SystemService, system_key};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct StoredProductExperienceSettings {
    mode: String,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
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
        ProductExperienceSettings {
            mode: self.product_mode(),
        }
    }
}

pub struct ProductExperienceAdapter {
    system: Arc<SystemService>,
}

impl ProductExperienceAdapter {
    pub fn new(system: Arc<SystemService>) -> Self {
        Self { system }
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
        let mut stored = self
            .system
            .get_json::<StoredProductExperienceSettings>(
                &ctx,
                system_key::PRODUCT_EXPERIENCE_SETTINGS,
            )
            .await
            .map_err(|error| ProductExperienceError::Read(error.to_string()))?
            .unwrap_or_default();
        stored.set_product_mode(input.mode);
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
