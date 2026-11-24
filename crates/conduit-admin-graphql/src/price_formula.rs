//! Persistent formulas generate base retail-price drafts under existing governance.
use async_graphql::{Context, ID, InputObject, Json, SimpleObject};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, InputObject)]
pub struct RetailPriceFormulaInput {
    #[graphql(name = "priceBookID")]
    pub price_book_id: ID,
    #[graphql(name = "publicModelID")]
    pub public_model_id: ID,
    #[graphql(name = "deploymentID")]
    pub deployment_id: ID,
    pub expression: String,
    /// Decimal strings keyed by identifier. `cost` is reserved.
    pub parameters: Json<Value>,
    pub enabled: bool,
    /// "0" creates a binding. Updates require the saved revision.
    pub expected_revision: String,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct RetailPriceFormula {
    pub id: ID,
    #[graphql(name = "priceBookID")]
    pub price_book_id: ID,
    #[graphql(name = "publicModelID")]
    pub public_model_id: ID,
    #[graphql(name = "deploymentID")]
    pub deployment_id: ID,
    pub expression: String,
    pub parameters: Json<Value>,
    pub enabled: bool,
    pub revision: String,
    pub status: String,
    pub last_error: Option<String>,
    pub last_fingerprint: Option<String>,
    #[graphql(name = "lastChangeSetID")]
    pub last_change_set_id: Option<ID>,
}

#[derive(Debug, Clone, SimpleObject)]
pub struct RetailPriceFormulaPreview {
    pub price: Json<Value>,
    pub source_price: Json<Value>,
    pub currency: String,
    pub fingerprint: String,
    pub source: Json<Value>,
}

#[async_trait::async_trait]
pub trait PriceFormulaServices: Send + Sync {
    async fn list(&self, actor: i64, book: &str) -> Result<Vec<RetailPriceFormula>, String>;
    async fn preview(
        &self,
        actor: i64,
        input: RetailPriceFormulaInput,
    ) -> Result<RetailPriceFormulaPreview, String>;
    async fn save(
        &self,
        actor: i64,
        input: RetailPriceFormulaInput,
    ) -> Result<RetailPriceFormula, String>;
}

pub(crate) fn services(ctx: &Context<'_>) -> Result<Arc<dyn PriceFormulaServices>, String> {
    ctx.data::<Arc<dyn PriceFormulaServices>>()
        .cloned()
        .map_err(|_| "price formulas unavailable".into())
}
pub(crate) fn actor(ctx: &Context<'_>) -> Result<i64, String> {
    ctx.data::<crate::me::CurrentUser>()
        .map(|user| user.user_id)
        .map_err(|_| "authentication required".into())
}
