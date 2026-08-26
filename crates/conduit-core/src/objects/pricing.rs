//! Pricing objects ported 1:1 from `conduit/internal/objects/price.go`.
//!
//! Covers the price-domain types used by cost calculation: the
//! [`PricingMode`] / [`Pricing`] / [`TieredPricing`] / [`PriceTier`] family,
//! the [`PriceItemCode`] and [`PromptWriteCacheVariantCode`] string-newtypes
//! (with their const variants), [`PromptWriteCacheVariant`],
//! [`ModelPriceItem`], and the top-level [`ModelPrice`] container.
//!
//! All field names, JSON tags, and `omitempty` semantics mirror the Go source
//! exactly. Pointer fields become `Option<T>` with
//! `skip_serializing_if = "Option::is_none"`. The Go `decimal.Decimal` type is
//! mapped to [`rust_decimal::Decimal`] (the crate-level convention; see
//! `Cargo.toml`).

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Pricing mode. Ported 1:1 from the Go `PricingMode` string newtype and its
/// `flat_fee` / `usage_per_unit` / `usage_tiered` / `usage_volume` constants.
///
/// Unknown wire values round-trip without error because this is a
/// `String` alias rather than a closed enum.
pub type PricingMode = String;

/// `PricingMode = "flat_fee"` — the request is charged a fixed fee.
pub const PRICING_MODE_FLAT_FEE: &str = "flat_fee";
/// `PricingMode = "usage_per_unit"` — request is charged a per-token fee
/// (e.g. $0.01 per token; 1,500 tokens => $15.00).
pub const PRICING_MODE_USAGE_PER_UNIT: &str = "usage_per_unit";
/// `PricingMode = "usage_tiered"` — each tier segment is billed separately at
/// its own rate (e.g. tiers `[{upTo:1000, $0.01},{upTo:nil, $0.02}]` with
/// 1,500 tokens => `(1000/1e6)*$0.01 + (500/1e6)*$0.02`).
pub const PRICING_MODE_TIERED: &str = "usage_tiered";
/// `PricingMode = "usage_volume"` — the tier matched by the *total* token
/// count determines the unit price for *all* tokens (e.g. tiers
/// `[{upTo:1000, $0.01},{upTo:nil, $0.02}]` with 1,500 tokens =>
/// `(1500/1e6)*$0.02`).
pub const PRICING_MODE_VOLUME: &str = "usage_volume";

/// Price item code. Ported 1:1 from the Go `PriceItemCode` string newtype;
/// known variants live in the [`price_item_code`] module. Unknown wire values
/// round-trip without error.
pub type PriceItemCode = String;

/// Known values of [`PriceItemCode`]. Mirrors the Go `PriceItemCode*`
/// constants.
pub mod price_item_code {
    /// `PriceItemCode = "prompt_tokens"` — token usage.
    pub const USAGE: &str = "prompt_tokens";
    /// `PriceItemCode = "completion_tokens"` — token completion.
    pub const COMPLETION: &str = "completion_tokens";
    /// `PriceItemCode = "prompt_cached_tokens"` — cached token usage.
    pub const PROMPT_CACHED_TOKEN: &str = "prompt_cached_tokens";
    /// `PriceItemCode = "prompt_write_cached_tokens"` — cached token write.
    pub const WRITE_CACHED_TOKENS: &str = "prompt_write_cached_tokens";
}

/// Prompt-write-cache variant code. Ported 1:1 from the Go
/// `PromptWriteCacheVariantCode` string newtype; known variants live in the
/// [`prompt_write_cache_variant_code`] module. Unknown wire values round-trip
/// without error.
pub type PromptWriteCacheVariantCode = String;

/// Known values of [`PromptWriteCacheVariantCode`]. Mirrors the Go
/// `PromptWriteCacheVariantCode*` constants.
pub mod prompt_write_cache_variant_code {
    /// `PromptWriteCacheVariantCode = "five_min"` — 5-minute cache lifetime.
    pub const FIVE_MIN: &str = "five_min";
    /// `PromptWriteCacheVariantCode = "one_hour"` — 1-hour cache lifetime.
    pub const ONE_HOUR: &str = "one_hour";
}

/// Price tier for tiered / volume pricing. Ported 1:1 from Go `PriceTier`.
///
/// `up_to` mirrors Go `*int64` with `omitempty`: an absent/`null` upper bound
/// means "no upper bound" and is only valid on the final tier.
/// `price_per_unit` is always present (no `omitempty` in Go).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PriceTier {
    /// Upper bound of the token usage for this tier. `None` means no upper
    /// bound (must be the last tier). Mirrors Go `upTo,omitempty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to: Option<i64>,
    /// Price per token for this tier. Mirrors Go `pricePerUnit`.
    #[serde(default)]
    pub price_per_unit: Decimal,
}

/// Tiered / volume pricing data. Ported 1:1 from Go `TieredPricing`. The same
/// structure backs both `usage_tiered` and `usage_volume` modes; they differ
/// only in calculation logic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TieredPricing {
    /// Ordered tiers. The last tier must have `up_to == None`.
    #[serde(default)]
    pub tiers: Vec<PriceTier>,
}

/// Pricing configuration. Ported 1:1 from Go `Pricing`. Only the branch
/// matching [`PricingMode`] is populated; the others are `None` and omitted
/// from the wire form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Pricing {
    /// Pricing mode selector. Always present (no `omitempty` in Go).
    #[serde(default)]
    pub mode: PricingMode,
    /// Fixed fee. Populated when `mode == "flat_fee"`. Mirrors Go
    /// `flatFee,omitempty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flat_fee: Option<Decimal>,
    /// Price per token. Populated when `mode == "usage_per_unit"`. Mirrors Go
    /// `usagePerUnit,omitempty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_per_unit: Option<Decimal>,
    /// Tier data. Populated when `mode` is `usage_tiered` or `usage_volume`.
    /// Mirrors Go `usageTiered,omitempty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_tiered: Option<TieredPricing>,
}

/// Prompt-write-cache variant. Ported 1:1 from Go `PromptWriteCacheVariant`.
/// Both fields are always present (no `omitempty` in Go).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromptWriteCacheVariant {
    /// Variant code (e.g. `"five_min"`, `"one_hour"`). Mirrors Go
    /// `variantCode`.
    #[serde(default)]
    pub variant_code: PromptWriteCacheVariantCode,
    /// Pricing for the variant. Mirrors Go `pricing`.
    #[serde(default)]
    pub pricing: Pricing,
}

/// Price item for a single token class. Ported 1:1 from Go `ModelPriceItem`.
/// `prompt_write_cache_variants` mirrors Go `omitempty` (elided when empty).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelPriceItem {
    /// Item code (e.g. `"prompt_tokens"`). Mirrors Go `itemCode`.
    #[serde(default)]
    pub item_code: PriceItemCode,
    /// Pricing for the item. Mirrors Go `pricing`.
    #[serde(default)]
    pub pricing: Pricing,
    /// Variants for prompt-write cached tokens. Mirrors Go
    /// `promptWriteCacheVariants,omitempty`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_write_cache_variants: Vec<PromptWriteCacheVariant>,
}

/// Top-level price container for a model. Ported 1:1 from Go `ModelPrice`.
/// `items` is always present (no `omitempty` in Go).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelPrice {
    /// Ordered price items. Mirrors Go `items`.
    #[serde(default)]
    pub items: Vec<ModelPriceItem>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_str, to_value};

    #[test]
    fn pricing_mode_and_item_code_constants_match_go() {
        assert_eq!(PRICING_MODE_FLAT_FEE, "flat_fee");
        assert_eq!(PRICING_MODE_USAGE_PER_UNIT, "usage_per_unit");
        assert_eq!(PRICING_MODE_TIERED, "usage_tiered");
        assert_eq!(PRICING_MODE_VOLUME, "usage_volume");

        assert_eq!(price_item_code::USAGE, "prompt_tokens");
        assert_eq!(price_item_code::COMPLETION, "completion_tokens");
        assert_eq!(price_item_code::PROMPT_CACHED_TOKEN, "prompt_cached_tokens");
        assert_eq!(
            price_item_code::WRITE_CACHED_TOKENS,
            "prompt_write_cached_tokens"
        );

        assert_eq!(prompt_write_cache_variant_code::FIVE_MIN, "five_min");
        assert_eq!(prompt_write_cache_variant_code::ONE_HOUR, "one_hour");
    }

    #[test]
    fn flat_fee_pricing_round_trip() -> Result<(), serde_json::Error> {
        let json = r#"{"mode":"flat_fee","flatFee":"0.50"}"#;
        let pricing: Pricing = from_str(json)?;
        assert_eq!(pricing.mode, PRICING_MODE_FLAT_FEE);
        assert!(pricing.usage_per_unit.is_none());
        assert!(pricing.usage_tiered.is_none());
        // Decimal deserializes from the JSON string form.
        assert_eq!(
            pricing.flat_fee,
            Some(Decimal::try_from(0.50_f64).ok()).flatten()
        );

        let value = to_value(&pricing)?;
        assert_eq!(value.get("mode").and_then(|v| v.as_str()), Some("flat_fee"));
        assert!(value.get("usagePerUnit").is_none());
        assert!(value.get("usageTiered").is_none());
        assert!(value.get("flatFee").is_some());

        let re: Pricing = serde_json::from_value(value)?;
        assert_eq!(re, pricing);
        Ok(())
    }

    #[test]
    fn model_price_with_tiered_item_round_trip() -> Result<(), serde_json::Error> {
        // Covers PriceItemCode, ModelPriceItem, Pricing, TieredPricing,
        // PriceTier, and ModelPrice in a single realistic payload.
        let json = r#"{
            "items": [
                {
                    "itemCode": "prompt_tokens",
                    "pricing": {
                        "mode": "usage_tiered",
                        "usageTiered": {
                            "tiers": [
                                {"upTo": 1000, "pricePerUnit": "0.01"},
                                {"upTo": null, "pricePerUnit": "0.02"}
                            ]
                        }
                    },
                    "promptWriteCacheVariants": [
                        {
                            "variantCode": "five_min",
                            "pricing": {"mode": "flat_fee", "flatFee": "0.10"}
                        }
                    ]
                },
                {
                    "itemCode": "completion_tokens",
                    "pricing": {"mode": "usage_per_unit", "usagePerUnit": "0.03"}
                }
            ]
        }"#;
        let model_price: ModelPrice = from_str(json)?;
        assert_eq!(model_price.items.len(), 2);

        let first = &model_price.items[0];
        assert_eq!(first.item_code, price_item_code::USAGE);
        assert_eq!(first.pricing.mode, PRICING_MODE_TIERED);
        let tiered = match &first.pricing.usage_tiered {
            Some(t) => t,
            None => return Ok(()), // unreachable but avoids panic
        };
        assert_eq!(tiered.tiers.len(), 2);
        assert_eq!(tiered.tiers[0].up_to, Some(1000));
        assert_eq!(tiered.tiers[1].up_to, None);
        assert_eq!(first.prompt_write_cache_variants.len(), 1);
        assert_eq!(
            first.prompt_write_cache_variants[0].variant_code,
            prompt_write_cache_variant_code::FIVE_MIN
        );

        let second = &model_price.items[1];
        assert_eq!(second.item_code, price_item_code::COMPLETION);
        assert_eq!(second.pricing.mode, PRICING_MODE_USAGE_PER_UNIT);
        assert!(second.pricing.usage_per_unit.is_some());
        // omitempty: an empty variants slice is elided on the wire.
        assert!(second.prompt_write_cache_variants.is_empty());

        // Full round-trip through Serialize -> Deserialize must be stable.
        let value = to_value(&model_price)?;
        let re: ModelPrice = serde_json::from_value(value)?;
        assert_eq!(re, model_price);
        Ok(())
    }

    #[test]
    fn unknown_price_item_code_round_trips() -> Result<(), serde_json::Error> {
        // Parity guarantee: an unknown PriceItemCode wire value must survive
        // a round-trip unchanged (the Go side accepts arbitrary strings).
        let json =
            r#"{"itemCode":"web_search_tokens","pricing":{"mode":"flat_fee","flatFee":"1.00"}}"#;
        let item: ModelPriceItem = from_str(json)?;
        assert_eq!(item.item_code, "web_search_tokens");
        let value = to_value(&item)?;
        assert_eq!(
            value.get("itemCode").and_then(|v| v.as_str()),
            Some("web_search_tokens")
        );
        // promptWriteCacheVariants omitted because of `Vec::is_empty` skip.
        assert!(value.get("promptWriteCacheVariants").is_none());
        let re: ModelPriceItem = serde_json::from_value(value)?;
        assert_eq!(re, item);
        Ok(())
    }

    #[test]
    fn default_pricing_omits_optional_fields() -> Result<(), serde_json::Error> {
        let pricing = Pricing::default();
        assert_eq!(pricing.mode, "");
        let value = to_value(&pricing)?;
        // mode has no omitempty so it is emitted as ""; optional branches are
        // absent.
        assert_eq!(value.get("mode").and_then(|v| v.as_str()), Some(""));
        assert!(value.get("flatFee").is_none());
        assert!(value.get("usagePerUnit").is_none());
        assert!(value.get("usageTiered").is_none());
        Ok(())
    }
}
