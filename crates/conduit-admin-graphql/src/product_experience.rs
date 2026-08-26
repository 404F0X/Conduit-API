//! Conduit API product-experience mode.
//!
//! `simple` and `enterprise` are two projections over the same data and
//! authorization model. They are not security boundaries; server-side scope
//! checks remain authoritative for every operation.

use std::sync::Arc;

use async_graphql::{Context, Enum, InputObject, SimpleObject};
use async_trait::async_trait;
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, SimpleObject)]
#[graphql(name = "ProductExperienceSettings")]
pub struct ProductExperienceSettings {
    pub mode: ProductMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdateProductExperienceSettingsInput")]
pub struct UpdateProductExperienceSettingsInput {
    pub mode: ProductMode,
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
