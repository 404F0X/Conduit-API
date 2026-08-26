use std::{str::FromStr, sync::Arc};

#[cfg(test)]
use std::sync::Mutex;

use async_trait::async_trait;
use conduit_db::RequestContext;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type ModelPriceServiceResult<T> = Result<T, ModelPriceServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModelPriceServiceError {
    #[error("invalid decimal for {field}: {value}")]
    InvalidDecimal { field: &'static str, value: String },
    #[error("model price not found for provider {provider} and model {model}")]
    PriceNotFound { provider: String, model: String },
    #[error("model price persistence lock poisoned")]
    LockPoisoned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceUnit {
    PerToken,
    PerThousandTokens,
    PerMillionTokens,
}

impl PriceUnit {
    fn denominator(self) -> Decimal {
        match self {
            Self::PerToken => Decimal::ONE,
            Self::PerThousandTokens => Decimal::from(1_000_u64),
            Self::PerMillionTokens => Decimal::from(1_000_000_u64),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub provider: String,
    pub model: String,
    pub prompt_price: String,
    pub completion_price: String,
    pub unit: PriceUnit,
    pub currency: String,
}

impl ModelPrice {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        prompt_price: impl Into<String>,
        completion_price: impl Into<String>,
        unit: PriceUnit,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            prompt_price: prompt_price.into(),
            completion_price: completion_price.into(),
            unit,
            currency: currency.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EstimatedModelCost {
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub prompt_cost: String,
    pub completion_cost: String,
    pub total_cost: String,
    pub currency: String,
}

#[async_trait]
pub trait ModelPriceRepo: Send + Sync {
    async fn get_model_price(
        &self,
        ctx: &RequestContext,
        provider: &str,
        model: &str,
    ) -> ModelPriceServiceResult<Option<ModelPrice>>;
}

pub struct ModelPriceService {
    repo: Arc<dyn ModelPriceRepo>,
}

impl ModelPriceService {
    pub fn new(repo: Arc<dyn ModelPriceRepo>) -> Self {
        Self { repo }
    }

    pub async fn get_price(
        &self,
        ctx: &RequestContext,
        provider: &str,
        model: &str,
    ) -> ModelPriceServiceResult<Option<ModelPrice>> {
        self.repo.get_model_price(ctx, provider, model).await
    }

    pub async fn estimate_cost(
        &self,
        ctx: &RequestContext,
        provider: &str,
        model: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
    ) -> ModelPriceServiceResult<EstimatedModelCost> {
        let price = self.get_price(ctx, provider, model).await?.ok_or_else(|| {
            ModelPriceServiceError::PriceNotFound {
                provider: provider.to_string(),
                model: model.to_string(),
            }
        })?;

        estimate_cost_from_price(&price, prompt_tokens, completion_tokens)
    }
}

pub fn estimate_cost_from_price(
    price: &ModelPrice,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> ModelPriceServiceResult<EstimatedModelCost> {
    let prompt_price = parse_decimal("prompt_price", &price.prompt_price)?;
    let completion_price = parse_decimal("completion_price", &price.completion_price)?;
    let denominator = price.unit.denominator();

    let prompt_cost = prompt_price * Decimal::from(prompt_tokens) / denominator;
    let completion_cost = completion_price * Decimal::from(completion_tokens) / denominator;
    let total_cost = prompt_cost + completion_cost;

    Ok(EstimatedModelCost {
        provider: price.provider.clone(),
        model: price.model.clone(),
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
        prompt_cost: decimal_to_string(prompt_cost),
        completion_cost: decimal_to_string(completion_cost),
        total_cost: decimal_to_string(total_cost),
        currency: price.currency.clone(),
    })
}

fn parse_decimal(field: &'static str, value: &str) -> ModelPriceServiceResult<Decimal> {
    Decimal::from_str(value).map_err(|_| ModelPriceServiceError::InvalidDecimal {
        field,
        value: value.to_string(),
    })
}

fn decimal_to_string(value: Decimal) -> String {
    value.normalize().to_string()
}

#[cfg(test)]
#[derive(Debug, Default)]
struct FakeModelPriceRepo {
    prices: Mutex<Vec<ModelPrice>>,
}

#[cfg(test)]
impl FakeModelPriceRepo {
    fn new(prices: Vec<ModelPrice>) -> Self {
        Self {
            prices: Mutex::new(prices),
        }
    }

    fn lock(&self) -> ModelPriceServiceResult<std::sync::MutexGuard<'_, Vec<ModelPrice>>> {
        self.prices
            .lock()
            .map_err(|_| ModelPriceServiceError::LockPoisoned)
    }
}

#[cfg(test)]
#[async_trait]
impl ModelPriceRepo for FakeModelPriceRepo {
    async fn get_model_price(
        &self,
        _ctx: &RequestContext,
        provider: &str,
        model: &str,
    ) -> ModelPriceServiceResult<Option<ModelPrice>> {
        Ok(self
            .lock()?
            .iter()
            .find(|price| price.provider == provider && price.model == model)
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use conduit_db::{PolicyContext, Principal, RequestContext};

    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    fn service(prices: Vec<ModelPrice>) -> ModelPriceService {
        ModelPriceService::new(Arc::new(FakeModelPriceRepo::new(prices)))
    }

    #[tokio::test]
    async fn get_price_matches_provider_and_model() -> ModelPriceServiceResult<()> {
        let expected = ModelPrice::new(
            "openai",
            "gpt-test",
            "0.15",
            "0.60",
            PriceUnit::PerMillionTokens,
            "USD",
        );
        let service = service(vec![
            ModelPrice::new(
                "openai",
                "gpt-other",
                "1.00",
                "2.00",
                PriceUnit::PerMillionTokens,
                "USD",
            ),
            expected.clone(),
        ]);

        let actual = service.get_price(&ctx(), "openai", "gpt-test").await?;

        assert_eq!(actual, Some(expected));
        Ok(())
    }

    #[tokio::test]
    async fn estimate_cost_applies_price_unit_to_prompt_and_completion_tokens()
    -> ModelPriceServiceResult<()> {
        let service = service(vec![ModelPrice::new(
            "openai",
            "gpt-test",
            "2",
            "8",
            PriceUnit::PerThousandTokens,
            "USD",
        )]);

        let estimate = service
            .estimate_cost(&ctx(), "openai", "gpt-test", 1_500, 250)
            .await?;

        assert_eq!(
            estimate,
            EstimatedModelCost {
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
                prompt_tokens: 1_500,
                completion_tokens: 250,
                total_tokens: 1_750,
                prompt_cost: "3".to_string(),
                completion_cost: "2".to_string(),
                total_cost: "5".to_string(),
                currency: "USD".to_string(),
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn estimate_cost_returns_not_found_for_missing_price() {
        let err = service(Vec::new())
            .estimate_cost(&ctx(), "openai", "gpt-test", 1, 1)
            .await;

        assert!(matches!(
            err,
            Err(ModelPriceServiceError::PriceNotFound { provider, model })
                if provider == "openai" && model == "gpt-test"
        ));
    }

    #[tokio::test]
    async fn invalid_decimal_price_is_rejected() {
        let err = service(vec![ModelPrice::new(
            "openai",
            "gpt-test",
            "not-decimal",
            "8",
            PriceUnit::PerMillionTokens,
            "USD",
        )])
        .estimate_cost(&ctx(), "openai", "gpt-test", 1, 1)
        .await;

        assert!(matches!(
            err,
            Err(ModelPriceServiceError::InvalidDecimal {
                field: "prompt_price",
                value,
            }) if value == "not-decimal"
        ));
    }
}
