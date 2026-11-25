use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use conduit_core::newapi_billing_expression::TokenFacts;

pub const BILLING_TOKEN_FACTS_KEY: &str = "new_api_billing_token_facts";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingTokenProtocol {
    Anthropic,
    OpenAiChat,
    OpenAiResponses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum BillingTokenCount {
    Present(u64),
    Missing,
    SemanticZero,
    Invalid,
}

impl BillingTokenCount {
    pub fn from_raw(value: Option<&Value>) -> Self {
        match value {
            Some(value) => value.as_u64().map_or(Self::Invalid, Self::Present),
            None => Self::Missing,
        }
    }

    fn checked(self, field: &str) -> Result<u64, String> {
        match self {
            Self::Present(value) => Ok(value),
            Self::SemanticZero => Ok(0),
            Self::Missing => Err(format!("Missing trusted billing token fact: {field}")),
            Self::Invalid => Err(format!("Invalid billing token fact: {field}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BillingTokenFacts {
    pub version: u8,
    pub protocol: BillingTokenProtocol,
    pub complete: bool,
    /// Raw provider input count before unified protocol conversion.
    pub input: BillingTokenCount,
    pub output: BillingTokenCount,
    pub cache_read: BillingTokenCount,
    pub cache_write_total: BillingTokenCount,
    pub cache_write_1h: BillingTokenCount,
    pub cache_write_12h: BillingTokenCount,
}

impl BillingTokenFacts {
    pub fn checked_token_facts(&self) -> Result<TokenFacts, String> {
        if self.version != 1 {
            return Err("Unsupported billing token facts version".into());
        }
        if !self.complete {
            return Err("Billing token facts require completed provider usage".into());
        }
        let input = self.input.checked("input")?;
        let output = self.output.checked("output")?;
        let cache_read = self.cache_read.checked("cache_read")?;
        let total_write = self.cache_write_total.checked("cache_write_total")?;
        let cache_write_1h = self.cache_write_1h.checked("cache_write_1h")?;
        let cache_write_12h = self.cache_write_12h.checked("cache_write_12h")?;
        let cache_write = total_write
            .checked_sub(cache_write_1h)
            .and_then(|remaining| remaining.checked_sub(cache_write_12h))
            .ok_or("Cache TTL write split exceeds aggregate writes")?;
        let input_includes_cache = self.protocol != BillingTokenProtocol::Anthropic;
        if input_includes_cache {
            if cache_read > input || total_write > input || cache_write_1h != 0 {
                return Err("Invalid OpenAI cache counts for reported input".into());
            }
        } else {
            if cache_write_12h != 0 {
                return Err("Invalid Anthropic 12-hour cache-write count".into());
            }
            input
                .checked_add(cache_read)
                .and_then(|input| input.checked_add(total_write))
                .ok_or("Anthropic input plus cache counts exceed the supported token range")?;
        }
        Ok(TokenFacts {
            input,
            output,
            cache_read,
            cache_write,
            cache_write_1h,
            cache_write_12h,
            input_includes_cache,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TokenDetails {
    #[serde(default)]
    pub cached_tokens: u64,
    #[serde(default)]
    pub write_cached_tokens: u64,
    // Go json tags are `write_cached_5min_tokens` / `write_cached_1hour_tokens`
    // (model.go:823/826) — explicit renames since the Go snake_case names differ
    // from the Rust field names.
    #[serde(default, rename = "write_cached_5min_tokens")]
    pub write_cached_tokens_5m: u64,
    #[serde(default, rename = "write_cached_1hour_tokens")]
    pub write_cached_tokens_1h: u64,
    #[serde(default)]
    pub audio_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub accepted_prediction_tokens: u64,
    #[serde(default)]
    pub rejected_prediction_tokens: u64,
    // Provider usage schemas drift; flatten keeps unknown token detail fields lossless.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl TokenDetails {
    pub fn zero() -> Self {
        Self::default()
    }

    pub fn is_zero(&self) -> bool {
        self.cached_tokens == 0
            && self.write_cached_tokens == 0
            && self.write_cached_tokens_5m == 0
            && self.write_cached_tokens_1h == 0
            && self.audio_tokens == 0
            && self.reasoning_tokens == 0
            && self.accepted_prediction_tokens == 0
            && self.rejected_prediction_tokens == 0
            && self.extra.is_empty()
    }

    pub fn merge(&mut self, other: &Self) {
        // Usage can be aggregated across retries or stream chunks; saturation avoids wraparound.
        self.cached_tokens = self.cached_tokens.saturating_add(other.cached_tokens);
        self.write_cached_tokens = self
            .write_cached_tokens
            .saturating_add(other.write_cached_tokens);
        self.write_cached_tokens_5m = self
            .write_cached_tokens_5m
            .saturating_add(other.write_cached_tokens_5m);
        self.write_cached_tokens_1h = self
            .write_cached_tokens_1h
            .saturating_add(other.write_cached_tokens_1h);
        self.audio_tokens = self.audio_tokens.saturating_add(other.audio_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(other.reasoning_tokens);
        self.accepted_prediction_tokens = self
            .accepted_prediction_tokens
            .saturating_add(other.accepted_prediction_tokens);
        self.rejected_prediction_tokens = self
            .rejected_prediction_tokens
            .saturating_add(other.rejected_prediction_tokens);
        self.extra.extend(other.extra.clone());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    // Go json tags are `prompt_tokens_details` / `completion_tokens_details`
    // (model.go:775/778) — explicit renames.
    #[serde(default, rename = "prompt_tokens_details")]
    pub prompt_details: TokenDetails,
    #[serde(default, rename = "completion_tokens_details")]
    pub completion_details: TokenDetails,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Usage {
    pub fn set_billing_token_facts(&mut self, facts: BillingTokenFacts) {
        self.metadata
            .insert(BILLING_TOKEN_FACTS_KEY.into(), serde_json::json!(facts));
    }

    pub fn billing_token_facts(&self) -> Result<BillingTokenFacts, String> {
        let raw = self
            .metadata
            .get(BILLING_TOKEN_FACTS_KEY)
            .ok_or("Missing trusted provider billing token facts")?;
        serde_json::from_value(raw.clone())
            .map_err(|err| format!("Invalid provider billing token facts: {err}"))
    }

    pub fn new_api_token_facts(&self) -> Result<TokenFacts, String> {
        let envelope = self.billing_token_facts()?;
        let facts = envelope.checked_token_facts()?;
        // Anthropic stream legacy usage can omit the early cache fields on the
        // final delta. Its separately assembled native input is already exclusive.
        if (facts.input_includes_cache && self.prompt_tokens != facts.input)
            || self.completion_tokens != facts.output
        {
            return Err("Provider billing token facts disagree with unified usage".into());
        }
        Ok(facts)
    }
    pub fn zero() -> Self {
        Self::default()
    }

    pub fn is_zero(&self) -> bool {
        self.prompt_tokens == 0
            && self.completion_tokens == 0
            && self.total_tokens == 0
            && self.prompt_details.is_zero()
            && self.completion_details.is_zero()
            && self.metadata.is_empty()
            && self.extra.is_empty()
    }

    pub fn nonzero(self) -> Option<Self> {
        (!self.is_zero()).then_some(self)
    }

    pub fn merge(&mut self, other: &Self) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        self.prompt_details.merge(&other.prompt_details);
        self.completion_details.merge(&other.completion_details);
        self.metadata.extend(other.metadata.clone());
        self.metadata.remove(BILLING_TOKEN_FACTS_KEY);
        self.extra.extend(other.extra.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_round_trip_preserves_token_details() -> Result<(), serde_json::Error> {
        let input = json!({
            "prompt_tokens": 10,
            "completion_tokens": 11,
            "total_tokens": 21,
            "prompt_tokens_details": {
                "cached_tokens": 2,
                "write_cached_tokens": 3,
                "write_cached_5min_tokens": 4,
                "write_cached_1hour_tokens": 5,
                "audio_tokens": 6
            },
            "completion_tokens_details": {
                "audio_tokens": 7,
                "reasoning_tokens": 8,
                "accepted_prediction_tokens": 9,
                "rejected_prediction_tokens": 10,
                "provider_detail": {"kept": true}
            },
            "metadata": {"source": "openai"},
            "provider_usage": {"foo": "bar"}
        });

        let usage: Usage = serde_json::from_value(input)?;
        let round_trip = serde_json::to_value(&usage)?;
        // Go wire keys: `prompt_tokens_details` / `completion_tokens_details`
        // (model.go:775/778) and `write_cached_5min_tokens` /
        // `write_cached_1hour_tokens` (model.go:823/826) — assert the typed
        // fields carry the values (not just the `extra` flatten fallback).
        assert_eq!(round_trip["prompt_tokens_details"]["cached_tokens"], 2);
        assert_eq!(
            round_trip["prompt_tokens_details"]["write_cached_tokens"],
            3
        );
        assert_eq!(
            round_trip["prompt_tokens_details"]["write_cached_5min_tokens"],
            4
        );
        assert_eq!(
            round_trip["prompt_tokens_details"]["write_cached_1hour_tokens"],
            5
        );
        assert_eq!(round_trip["prompt_tokens_details"]["audio_tokens"], 6);
        assert_eq!(
            round_trip["completion_tokens_details"]["reasoning_tokens"],
            8
        );
        assert_eq!(
            round_trip["completion_tokens_details"]["accepted_prediction_tokens"],
            9
        );
        assert_eq!(
            round_trip["completion_tokens_details"]["rejected_prediction_tokens"],
            10
        );
        assert_eq!(
            round_trip["completion_tokens_details"]["provider_detail"]["kept"],
            true
        );
        assert_eq!(round_trip["provider_usage"]["foo"], "bar");
        Ok(())
    }

    #[test]
    fn usage_merge_saturates_and_keeps_extensions() {
        let mut left = Usage {
            prompt_tokens: u64::MAX,
            ..Usage::default()
        };
        let mut right = Usage {
            prompt_tokens: 1,
            ..Usage::default()
        };
        right.extra.insert("provider".to_string(), json!("x"));

        left.merge(&right);
        assert_eq!(left.prompt_tokens, u64::MAX);
        assert_eq!(left.extra.get("provider"), Some(&json!("x")));
    }
}
