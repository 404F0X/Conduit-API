//! Authenticated, sanitized self-service model catalog.

use std::sync::Arc;

use async_graphql::{Context, ID, SimpleObject};

#[derive(Debug, Clone, SimpleObject)]
pub struct MyCatalogHealth {
    pub status: String,
    pub success_rate: Option<f64>,
    pub avg_time_to_first_token_ms: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
    pub last_updated_at: Option<String>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct MyCatalogPrice {
    pub currency: String,
    pub display_name: String,
    pub input_per_million: Option<String>,
    pub output_per_million: Option<String>,
    pub cache_read_per_million: Option<String>,
    pub cache_write_per_million: Option<String>,
    pub effective_multiplier: String,
    pub billable: bool,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct MyCatalogRoute {
    pub id: ID,
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub channel_name: String,
    pub label: String,
    pub route_type: String,
    pub health: Option<MyCatalogHealth>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct MyCatalogModel {
    pub id: ID,
    pub model_id: String,
    pub name: String,
    pub group: String,
    pub developer: String,
    pub model_type: String,
    pub capabilities: Vec<String>,
    pub context_limit: Option<i64>,
    pub output_limit: Option<i64>,
    pub price: MyCatalogPrice,
    pub routes: Vec<MyCatalogRoute>,
    pub health: Option<MyCatalogHealth>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct MyModelCatalog {
    pub models: Vec<MyCatalogModel>,
    pub health_visible: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelCatalogError {
    #[error("model catalog service is unavailable")]
    Unavailable,
    #[error("model catalog query failed: {0}")]
    Query(String),
    #[error("user has no enabled access group")]
    NoAccessGroup,
    #[error("a current project must be selected")]
    ProjectRequired,
    #[error("user does not have access to the selected project")]
    ProjectAccessDenied,
}

#[async_trait::async_trait]
pub trait ModelCatalogServices: Send + Sync {
    async fn my_model_catalog(
        &self,
        user_id: i64,
        project_id: i64,
    ) -> Result<MyModelCatalog, ModelCatalogError>;
}

pub(crate) fn model_catalog_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ModelCatalogServices>, String> {
    ctx.data::<Arc<dyn ModelCatalogServices>>()
        .cloned()
        .map_err(|_| ModelCatalogError::Unavailable.to_string())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn catalog_requires_an_authenticated_current_user() {
        let schema = crate::admin_schema_builder().finish();
        let response = schema.execute("{ myModelCatalog { healthVisible } }").await;
        assert!(!response.errors.is_empty());
        assert!(
            response.errors[0]
                .message
                .contains("authentication required")
        );
    }

    #[test]
    fn route_contract_exposes_only_the_authenticated_channel_identity() {
        let sdl = crate::admin_schema_builder().finish().sdl();
        let start = sdl.find("type MyCatalogRoute").expect("catalog route type");
        let tail = &sdl[start..];
        let end = tail.find("\n}").expect("catalog route end") + 2;
        let route_type = &tail[..end];
        for forbidden in [
            "baseURL",
            "credentials",
            "headers",
            "proxy",
            "upstreamModelID",
            "cost",
            "priority",
            "weight",
            "source",
        ] {
            assert!(!route_type.contains(forbidden), "leaked field {forbidden}");
        }
        assert!(route_type.contains("channelID: ID!"));
        assert!(route_type.contains("channelName: String!"));
    }
}
