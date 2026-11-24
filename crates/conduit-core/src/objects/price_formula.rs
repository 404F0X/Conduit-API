//! Bounded decimal arithmetic for persisted base retail prices.
use std::collections::BTreeMap;
use std::str::FromStr;

use rust_decimal::{Decimal, RoundingStrategy};

use super::pricing::{ModelPrice, Pricing};

#[derive(Debug, Clone)]
enum Node {
    Number(Decimal),
    Cost,
    Constant(String),
    Negative(Box<Node>),
    Binary(u8, Box<Node>, Box<Node>),
}

#[derive(Debug, Clone)]
pub struct PriceFormula {
    root: Node,
    constants: BTreeMap<String, Decimal>,
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    nodes: usize,
}

impl Parser<'_> {
    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.pos += 1;
        }
    }
    fn expression(&mut self, depth: usize, minimum: u8) -> Result<Node, String> {
        if depth > 24 || self.nodes >= 128 {
            return Err("formula exceeds 24 nesting levels or 128 nodes".into());
        }
        self.nodes += 1;
        self.whitespace();
        let mut left = match self.bytes.get(self.pos).copied() {
            Some(b'(') => {
                self.pos += 1;
                let value = self.expression(depth + 1, 0)?;
                self.whitespace();
                if self.bytes.get(self.pos) != Some(&b')') {
                    return Err("missing closing parenthesis".into());
                }
                self.pos += 1;
                value
            }
            Some(b'-') => {
                self.pos += 1;
                Node::Negative(Box::new(self.expression(depth + 1, 3)?))
            }
            Some(ch) if ch.is_ascii_digit() => {
                let start = self.pos;
                while self
                    .bytes
                    .get(self.pos)
                    .is_some_and(|ch| ch.is_ascii_digit() || *ch == b'.')
                {
                    self.pos += 1;
                }
                let text = std::str::from_utf8(&self.bytes[start..self.pos])
                    .map_err(|_| "invalid number")?;
                let value = Decimal::from_str(text).map_err(|_| "invalid decimal literal")?;
                if value.scale() > 12 {
                    return Err("decimal literals support at most 12 fractional digits".into());
                }
                Node::Number(value)
            }
            Some(ch) if ch.is_ascii_alphabetic() || ch == b'_' => {
                let start = self.pos;
                while self
                    .bytes
                    .get(self.pos)
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == b'_')
                {
                    self.pos += 1;
                }
                let name = std::str::from_utf8(&self.bytes[start..self.pos])
                    .map_err(|_| "invalid name")?;
                if name == "cost" {
                    Node::Cost
                } else {
                    Node::Constant(name.into())
                }
            }
            _ => return Err("expected a decimal, cost, constant or parenthesis".into()),
        };
        loop {
            self.whitespace();
            let Some(op @ (b'+' | b'-' | b'*' | b'/')) = self.bytes.get(self.pos).copied() else {
                break;
            };
            let precedence = if matches!(op, b'*' | b'/') { 2 } else { 1 };
            if precedence < minimum {
                break;
            }
            self.pos += 1;
            self.nodes += 1;
            if self.nodes > 128 {
                return Err("formula exceeds 128 nodes".into());
            }
            let right = self.expression(depth + 1, precedence + 1)?;
            left = Node::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }
}

impl PriceFormula {
    pub fn parse(expression: &str, parameters: &BTreeMap<String, String>) -> Result<Self, String> {
        if expression.is_empty()
            || expression.len() > 1024
            || !expression.is_ascii()
            || parameters.len() > 32
        {
            return Err("formula must be 1–1024 ASCII bytes with at most 32 constants".into());
        }
        let mut constants = BTreeMap::new();
        for (name, text) in parameters {
            if text.len() > 64 || !text.is_ascii() {
                return Err(format!("constant {name} must be a bounded decimal string"));
            }
            if name == "cost"
                || name.is_empty()
                || name.len() > 32
                || !name
                    .bytes()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
                || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            {
                return Err(format!("invalid constant name {name}"));
            }
            let value =
                Decimal::from_str(text).map_err(|_| format!("invalid decimal constant {name}"))?;
            if value.scale() > 12 {
                return Err(format!("constant {name} exceeds 12 fractional digits"));
            }
            constants.insert(name.clone(), value);
        }
        let mut parser = Parser {
            bytes: expression.as_bytes(),
            pos: 0,
            nodes: 0,
        };
        let root = parser.expression(0, 0)?;
        parser.whitespace();
        if parser.pos != parser.bytes.len() {
            return Err("unexpected formula text".into());
        }
        fn validate(node: &Node, constants: &BTreeMap<String, Decimal>) -> Result<(), String> {
            match node {
                Node::Constant(name) if !constants.contains_key(name) => {
                    Err(format!("unknown constant {name}"))
                }
                Node::Negative(value) => validate(value, constants),
                Node::Binary(_, a, b) => {
                    validate(a, constants)?;
                    validate(b, constants)
                }
                _ => Ok(()),
            }
        }
        validate(&root, &constants)?;
        Ok(Self { root, constants })
    }

    pub fn evaluate(&self, cost: Decimal) -> Result<Decimal, String> {
        fn eval(
            node: &Node,
            cost: Decimal,
            constants: &BTreeMap<String, Decimal>,
        ) -> Result<Decimal, String> {
            match node {
                Node::Number(value) => Ok(*value),
                Node::Cost => Ok(cost),
                Node::Constant(name) => constants
                    .get(name)
                    .copied()
                    .ok_or_else(|| format!("unknown constant {name}")),
                Node::Negative(value) => Decimal::ZERO
                    .checked_sub(eval(value, cost, constants)?)
                    .ok_or_else(|| "decimal overflow".into()),
                Node::Binary(op, a, b) => {
                    let a = eval(a, cost, constants)?;
                    let b = eval(b, cost, constants)?;
                    if *op == b'/' && b.is_zero() {
                        return Err("division by zero".into());
                    }
                    match op {
                        b'+' => a.checked_add(b),
                        b'-' => a.checked_sub(b),
                        b'*' => a.checked_mul(b),
                        b'/' => a.checked_div(b),
                        _ => None,
                    }
                    .ok_or_else(|| "decimal overflow".into())
                }
            }
        }
        if cost.is_sign_negative() {
            return Err("procurement cost is negative".into());
        }
        let value = eval(&self.root, cost, &self.constants)?;
        if value.is_sign_negative() || value > Decimal::from(1_000_000_000_000_i64) {
            return Err("retail result must be between 0 and 1000000000000".into());
        }
        Ok(value
            .round_dp_with_strategy(12, RoundingStrategy::MidpointAwayFromZero)
            .normalize())
    }

    /// Each price leaf uses its own cost in the original unit. Cache variants,
    /// tier boundaries, item codes and request fees retain their structure.
    pub fn apply(&self, source: &ModelPrice) -> Result<ModelPrice, String> {
        fn apply_pricing(formula: &PriceFormula, price: &mut Pricing) -> Result<(), String> {
            match price.mode.as_str() {
                "flat_fee" => {
                    price.flat_fee =
                        Some(formula.evaluate(price.flat_fee.ok_or("flat fee cost is missing")?)?)
                }
                "usage_per_unit" => {
                    price.usage_per_unit = Some(
                        formula.evaluate(price.usage_per_unit.ok_or("unit cost is missing")?)?,
                    )
                }
                "usage_tiered" | "usage_volume" => {
                    let tiers = &mut price
                        .usage_tiered
                        .as_mut()
                        .ok_or("tier cost is missing")?
                        .tiers;
                    if tiers.is_empty() {
                        return Err("tier cost is empty".into());
                    }
                    for tier in tiers {
                        tier.price_per_unit = formula.evaluate(tier.price_per_unit)?;
                    }
                }
                _ => return Err("unsupported procurement pricing mode".into()),
            }
            Ok(())
        }
        if source.items.is_empty() {
            return Err("procurement price is missing".into());
        }
        let mut result = source.clone();
        for item in &mut result.items {
            apply_pricing(self, &mut item.pricing)?;
            for variant in &mut item.prompt_write_cache_variants {
                apply_pricing(self, &mut variant.pricing)?;
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_decimal_precedence_errors_and_limits() {
        let formula = PriceFormula::parse(
            "(cost + margin) * 1.2 / 3",
            &BTreeMap::from([("margin".into(), "0.1".into())]),
        )
        .unwrap();
        assert_eq!(
            formula
                .evaluate(Decimal::from_str("0.2").unwrap())
                .unwrap()
                .to_string(),
            "0.12"
        );
        for (text, cost) in [
            ("cost / 0", "1"),
            ("cost - 1", "0"),
            ("cost * 79228162514264337593543950335", "2"),
        ] {
            assert!(
                PriceFormula::parse(text, &BTreeMap::new())
                    .unwrap()
                    .evaluate(Decimal::from_str(cost).unwrap())
                    .is_err()
            );
        }
        for text in ["eval(cost)", "missing + cost", "1.0000000000001", "cost;1"] {
            assert!(PriceFormula::parse(text, &BTreeMap::new()).is_err());
        }
        assert!(
            PriceFormula::parse(
                &format!("{}cost{}", "(".repeat(25), ")".repeat(25)),
                &BTreeMap::new()
            )
            .is_err()
        );
        assert_eq!(
            PriceFormula::parse("cost / 3", &BTreeMap::new())
                .unwrap()
                .evaluate(Decimal::ONE)
                .unwrap()
                .to_string(),
            "0.333333333333"
        );
    }
    #[test]
    fn preserves_cache_tiers_request_units_and_zero() {
        let source: ModelPrice = serde_json::from_value(serde_json::json!({"items":[{"itemCode":"prompt_write_cached_tokens","pricing":{"mode":"usage_tiered","usageTiered":{"tiers":[{"upTo":1000,"pricePerUnit":"0"},{"pricePerUnit":"2"}]}},"promptWriteCacheVariants":[{"variantCode":"one_hour","pricing":{"mode":"usage_per_unit","usagePerUnit":"3"}}]},{"itemCode":"request","pricing":{"mode":"flat_fee","flatFee":"0.01"}}]})).unwrap();
        let result = PriceFormula::parse("cost * 2", &BTreeMap::new())
            .unwrap()
            .apply(&source)
            .unwrap();
        assert_eq!(
            result.items[0].pricing.usage_tiered.as_ref().unwrap().tiers[0].up_to,
            Some(1000)
        );
        assert_eq!(
            result.items[0].pricing.usage_tiered.as_ref().unwrap().tiers[0].price_per_unit,
            Decimal::ZERO
        );
        assert_eq!(
            result.items[0].prompt_write_cache_variants[0]
                .pricing
                .usage_per_unit,
            Some(Decimal::from(6))
        );
        assert_eq!(
            result.items[1].pricing.flat_fee.unwrap().to_string(),
            "0.02"
        );
        assert!(
            PriceFormula::parse("cost", &BTreeMap::new())
                .unwrap()
                .apply(&ModelPrice::default())
                .is_err()
        );
    }
}
