//! Cost objects ported from `conduit/internal/objects/cost.go`.
//!
//! Depends on the pricing string-newtypes [`PriceItemCode`] and
//! [`PromptWriteCacheVariantCode`] ported in `pricing.rs` (OBJ-07).

use crate::objects::pricing::{PriceItemCode, PromptWriteCacheVariantCode};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// A single tier within a tiered cost breakdown. Ported 1:1 from Go `TierCost`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TierCost {
    /// Upper bound of the tier (exclusive); `None` means unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to: Option<i64>,
    #[serde(default)]
    pub units: i64,
    #[serde(default)]
    pub subtotal: Decimal,
}

/// A priced cost item. Ported 1:1 from Go `CostItem`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CostItem {
    #[serde(default)]
    pub item_code: PriceItemCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_write_cache_variant_code: Option<PromptWriteCacheVariantCode>,
    #[serde(default)]
    pub quantity: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tier_breakdown: Vec<TierCost>,
    #[serde(default)]
    pub subtotal: Decimal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn cost_item_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let input = r#"{"itemCode":"usage","promptWriteCacheVariantCode":"5m","quantity":1200,"tierBreakdown":[{"upTo":1000,"units":1000,"subtotal":"0.10"},{"units":200,"subtotal":"0.04"}],"subtotal":"0.14"}"#;
        let item: CostItem = serde_json::from_str(input)?;
        assert_eq!(item.item_code, "usage");
        assert_eq!(item.prompt_write_cache_variant_code.as_deref(), Some("5m"));
        assert_eq!(item.quantity, 1200);
        assert_eq!(item.tier_breakdown.len(), 2);
        assert_eq!(item.tier_breakdown[0].up_to, Some(1000));
        assert_eq!(item.tier_breakdown[0].subtotal, Decimal::from_str("0.10")?);
        assert_eq!(item.subtotal, Decimal::from_str("0.14")?);

        // Round-trip preserves shape, including `omitempty` on the 2nd tier's `upTo`.
        let re = serde_json::to_value(&item)?;
        let tier1 = re
            .get("tierBreakdown")
            .and_then(|v| v.get(0))
            .and_then(|v| v.get("upTo"))
            .and_then(|v| v.as_i64());
        assert_eq!(tier1, Some(1000));
        let tier2_up_to = re
            .get("tierBreakdown")
            .and_then(|v| v.get(1))
            .and_then(|v| v.get("upTo"));
        assert!(tier2_up_to.is_none());
        Ok(())
    }

    #[test]
    fn cost_item_minimal_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let item: CostItem =
            serde_json::from_str(r#"{"itemCode":"completion","quantity":0,"subtotal":"0"}"#)?;
        let re = serde_json::to_value(&item)?;
        // omitempty drops the absent optional fields.
        assert!(re.get("promptWriteCacheVariantCode").is_none());
        assert!(re.get("tierBreakdown").is_none());
        assert_eq!(
            re.get("itemCode").and_then(|v| v.as_str()),
            Some("completion")
        );
        Ok(())
    }
}
