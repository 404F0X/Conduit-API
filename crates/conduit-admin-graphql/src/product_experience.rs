//! Conduit API product-experience mode.
//!
//! `simple` and `enterprise` are two projections over the same data and
//! authorization model. Server-side scope checks remain authoritative for
//! every operation; on top of them, the authz extension refuses a short list
//! of enterprise-only root fields to non-owners while the site runs in SIMPLE
//! mode (see `authz_extension::enterprise_only_field`).

use std::sync::Arc;

use async_graphql::{Context, Enum, InputObject, SimpleObject};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Product navigation and landing-page projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "ProductMode")]
pub enum ProductMode {
    Simple,
    Enterprise,
}

impl Default for ProductMode {
    fn default() -> Self {
        Self::Enterprise
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FeatureOverride {
    #[default]
    Inherit,
    On,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ProductFeature {
    pub id: String,
    pub visibility: FeatureOverride,
    pub execution: FeatureOverride,
    pub visible: bool,
    pub enabled: bool,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct ProductFeatureOverrideInput {
    pub id: String,
    pub visibility: Option<FeatureOverride>,
    pub execution: Option<FeatureOverride>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ProductExperienceSettings")]
pub struct ProductExperienceSettings {
    pub mode: ProductMode,
    pub revision: String,
    pub features: Vec<ProductFeature>,
}

#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateProductExperienceSettingsInput")]
pub struct UpdateProductExperienceSettingsInput {
    pub mode: ProductMode,
    pub expected_revision: Option<String>,
    pub overrides: Option<Vec<ProductFeatureOverrideInput>>,
}

#[derive(Debug, Error)]
pub enum ProductExperienceError {
    #[error("product experience service is not available")]
    ServiceUnavailable,
    #[error("failed to read product experience settings: {0}")]
    Read(String),
    #[error("failed to update product experience settings: {0}")]
    Update(String),
}

#[async_trait]
pub trait ProductExperienceServices: Send + Sync {
    async fn settings(&self) -> Result<ProductExperienceSettings, ProductExperienceError>;

    async fn update_settings(
        &self,
        input: UpdateProductExperienceSettingsInput,
    ) -> Result<ProductExperienceSettings, ProductExperienceError>;
}

pub fn product_experience_services<'a>(
    ctx: &'a Context<'_>,
) -> Result<&'a Arc<dyn ProductExperienceServices>, String> {
    ctx.data::<Arc<dyn ProductExperienceServices>>()
        .map_err(|_| ProductExperienceError::ServiceUnavailable.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_default_to_enterprise() {
        assert_eq!(
            ProductExperienceSettings::default().mode,
            ProductMode::Enterprise
        );
    }

    #[test]
    fn schema_exposes_product_experience_query_and_mutation() {
        let sdl = crate::build_admin_schema().sdl();
        assert!(sdl.contains("productExperienceSettings: ProductExperienceSettings!"));
        assert!(sdl.contains(
            "updateProductExperienceSettings(input: UpdateProductExperienceSettingsInput!): ProductExperienceSettings!"
        ));
        assert!(sdl.contains("enum ProductMode"));
        assert!(sdl.contains("SIMPLE"));
        assert!(sdl.contains("ENTERPRISE"));
    }
}
