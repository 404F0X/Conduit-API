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

#[derive(async_graphql::InputObject)]
pub struct NewApiBillingExpressionPreviewInput {
    pub pricing: async_graphql::Json<serde_json::Value>,
    pub prompt_tokens: String,
    pub completion_tokens: String,
    pub cache_read_tokens: String,
    pub cache_write_tokens: String,
    #[graphql(name = "cacheWrite1hTokens")]
    pub cache_write_1h_tokens: String,
    pub input_includes_cache: bool,
}

#[derive(async_graphql::SimpleObject)]
pub struct NewApiBillingExpressionPreview {
    #[graphql(name = "amountUSD")]
    pub amount_usd: String,
}

impl NewApiBillingExpressionPreviewInput {
    pub(crate) fn evaluate(self) -> Result<NewApiBillingExpressionPreview, String> {
        use conduit_core::{
            newapi_billing_expression::TokenFacts, objects::newapi_price::NewApiPrice,
        };
        fn tokens(value: &str, field: &str) -> Result<u64, String> {
            if value.is_empty()
                || value.len() > 20
                || !value.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(format!("{field} must be an unsigned integer string"));
            }
            value
                .parse()
                .map_err(|_| format!("{field} exceeds the supported range"))
        }
        let price: NewApiPrice =
            serde_json::from_value(self.pricing.0).map_err(|error| error.to_string())?;
        let amount = price.evaluate(TokenFacts {
            input: tokens(&self.prompt_tokens, "promptTokens")?,
            output: tokens(&self.completion_tokens, "completionTokens")?,
            cache_read: tokens(&self.cache_read_tokens, "cacheReadTokens")?,
            cache_write: tokens(&self.cache_write_tokens, "cacheWriteTokens")?,
            cache_write_1h: tokens(&self.cache_write_1h_tokens, "cacheWrite1hTokens")?,
            cache_write_12h: 0,
            input_includes_cache: self.input_includes_cache,
        })?;
        Ok(NewApiBillingExpressionPreview {
            amount_usd: amount.normalize().to_string(),
        })
    }
}
