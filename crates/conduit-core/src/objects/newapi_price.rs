//! Approved NEW API expression price leaf shared by preview and billing.
use crate::newapi_billing_expression::{self as engine, CompiledExpression, TokenFacts};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewApiPrice {
    pub kind: String,
    pub profile: String,
    pub version: u8,
    pub expression: String,
    pub multiplier: String,
}

impl NewApiPrice {
    pub fn compile(&self) -> Result<(CompiledExpression, Decimal), String> {
        if self.kind != "NEW_API_EXPRESSION" || self.profile != engine::PROFILE || self.version != 1
        {
            return Err("unsupported NEW API price kind, profile or version".into());
        }
        if self.multiplier.len() > 32
            || !self
                .multiplier
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
        {
            return Err(
                "NEW API price multiplier is outside supported decimal syntax or precision".into(),
            );
        }
        let multiplier = Decimal::from_str_exact(&self.multiplier)
            .ok()
            .filter(|value| *value > Decimal::ZERO)
            .ok_or("NEW API price multiplier must be a positive decimal string")?;
        let compiled = engine::compile(&self.expression).map_err(|error| error.to_string())?;
        Ok((compiled, multiplier))
    }

    pub fn estimate_upper(&self, input: u64, output: u64) -> Result<Decimal, String> {
        let (compiled, multiplier) = self.compile()?;
        let openai = TokenFacts {
            input,
            output,
            cache_read: input,
            cache_write: input,
            cache_write_1h: 0,
            cache_write_12h: 0,
            input_includes_cache: true,
        };
        let anthropic = TokenFacts {
            cache_write_1h: input,
            input_includes_cache: false,
            ..openai
        };
        let bound = |facts| {
            compiled
                .upper_bound(facts)
                .map_err(|error| error.to_string())
        };
        bound(openai)?
            .max(bound(anthropic)?)
            .checked_mul(multiplier)
            .ok_or_else(|| "NEW API price bound overflow".into())
    }

    pub fn evaluate(&self, facts: TokenFacts) -> Result<Decimal, String> {
        let (compiled, multiplier) = self.compile()?;
        compiled
            .evaluate(facts)
            .map_err(|error| error.to_string())?
            .checked_mul(multiplier)
            .ok_or_else(|| "NEW API price amount overflow".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn strict_price_leaf_applies_multiplier_once() {
        let wire = json!({
            "kind":"NEW_API_EXPRESSION","profile":engine::PROFILE,"version":1,
            "expression":r#"true ? tier("flat", fixed(0.25)) : tier("usage", p)"#,"multiplier":"1.5"
        });
        let price: NewApiPrice = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            price
                .evaluate(TokenFacts {
                    input: 10,
                    output: 2,
                    cache_read: 0,
                    cache_write: 0,
                    cache_write_1h: 0,
                    cache_write_12h: 0,
                    input_includes_cache: true,
                })
                .unwrap(),
            Decimal::new(375, 3)
        );
        let mut invalid = wire;
        invalid["items"] = json!([]);
        assert!(serde_json::from_value::<NewApiPrice>(invalid).is_err());
    }
}
