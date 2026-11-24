//! Versioned retail documents and bounded, deterministic condition evaluation.
use super::pricing::ModelPrice;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const RETAIL_PRICE_SCHEMA: &str = "group-conditional-price-v1";
pub const FACTS_VERSION: &str = "normalized-usage-v1";
pub const MAX_CONDITION_INTEGER: i64 = 9_007_199_254_740_991;
const FACTS: &[&str] = &[
    "prompt_tokens",
    "completion_tokens",
    "prompt_cached_tokens",
    "prompt_write_cached_tokens",
    "total_tokens",
    "hour",
    "minute",
    "weekday",
    "month",
    "day",
    "date",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetailPriceDocument {
    pub schema: String,
    pub global: Option<ModelPrice>,
    pub groups: Vec<GroupPriceBranch>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupPriceBranch {
    #[serde(rename = "modelGroupID")]
    pub model_group_id: String,
    pub rules: Vec<GroupPriceRule>,
    pub base: Option<ModelPrice>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupPriceRule {
    pub id: String,
    pub label: String,
    pub condition: PriceCondition,
    pub price: ModelPrice,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PriceCondition {
    Text(String),
    Ast(ConditionNode),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConditionNode {
    Compare {
        fact: String,
        cmp: Comparison,
        value: i64,
    },
    And {
        left: Box<Self>,
        right: Box<Self>,
    },
    Or {
        left: Box<Self>,
        right: Box<Self>,
    },
    Not {
        value: Box<Self>,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Lt,
    Le,
    Eq,
    Ne,
    Ge,
    Gt,
}

impl PriceCondition {
    pub fn compile(&self) -> Result<ConditionNode, String> {
        let node = match self {
            Self::Ast(node) => node.clone(),
            Self::Text(text) => {
                if text.len() > 4096 {
                    return Err("condition exceeds 4096 bytes".into());
                }
                let mut parser = Parser {
                    text: text.as_bytes(),
                    pos: 0,
                    nodes: 0,
                };
                let node = parser.expression(0, false)?;
                parser.space();
                if parser.pos != parser.text.len() {
                    return Err("unexpected condition input".into());
                }
                node
            }
        };
        node.validate(0, &mut 0)?;
        Ok(node)
    }
}
impl ConditionNode {
    fn known_result(&self, facts: &BTreeMap<String, i64>) -> Result<Option<bool>, String> {
        Ok(match self {
            Self::Compare { fact, .. } => {
                if facts.contains_key(fact) {
                    Some(self.eval(facts)?)
                } else {
                    None
                }
            }
            Self::Not { value } => value.known_result(facts)?.map(|v| !v),
            Self::And { left, right } => {
                match (left.known_result(facts)?, right.known_result(facts)?) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                }
            }
            Self::Or { left, right } => {
                match (left.known_result(facts)?, right.known_result(facts)?) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                }
            }
        })
    }
    fn validate(&self, depth: usize, count: &mut usize) -> Result<(), String> {
        *count += 1;
        if depth > 24 || *count > 128 {
            return Err("condition exceeds 24 levels or 128 nodes".into());
        }
        match self {
            Self::Compare { fact, value, .. } => {
                if !FACTS.contains(&fact.as_str())
                    || !(-MAX_CONDITION_INTEGER..=MAX_CONDITION_INTEGER).contains(value)
                {
                    return Err("unknown fact or comparison integer outside safe JSON range".into());
                }
            }
            Self::And { left, right } | Self::Or { left, right } => {
                left.validate(depth + 1, count)?;
                right.validate(depth + 1, count)?;
            }
            Self::Not { value } => value.validate(depth + 1, count)?,
        }
        Ok(())
    }
    pub fn evaluate(&self, facts: &BTreeMap<String, i64>) -> Result<bool, String> {
        // Validate every required fact, including short-circuited branches.
        self.validate_facts(facts)?;
        self.eval(facts)
    }
    fn validate_facts(&self, facts: &BTreeMap<String, i64>) -> Result<(), String> {
        match self {
            Self::Compare { fact, .. } => {
                if facts.get(fact).is_none_or(|v| *v < 0) {
                    return Err(format!("missing or invalid fact {fact}"));
                }
            }
            Self::And { left, right } | Self::Or { left, right } => {
                left.validate_facts(facts)?;
                right.validate_facts(facts)?;
            }
            Self::Not { value } => value.validate_facts(facts)?,
        }
        Ok(())
    }
    fn eval(&self, facts: &BTreeMap<String, i64>) -> Result<bool, String> {
        Ok(match self {
            Self::Compare { fact, cmp, value } => {
                let actual = facts.get(fact).ok_or("missing fact")?;
                match cmp {
                    Comparison::Lt => actual < value,
                    Comparison::Le => actual <= value,
                    Comparison::Eq => actual == value,
                    Comparison::Ne => actual != value,
                    Comparison::Ge => actual >= value,
                    Comparison::Gt => actual > value,
                }
            }
            Self::And { left, right } => left.eval(facts)? && right.eval(facts)?,
            Self::Or { left, right } => left.eval(facts)? || right.eval(facts)?,
            Self::Not { value } => !value.eval(facts)?,
        })
    }
}
struct Parser<'a> {
    text: &'a [u8],
    pos: usize,
    nodes: usize,
}
impl Parser<'_> {
    fn space(&mut self) {
        while self.text.get(self.pos).is_some_and(u8::is_ascii_whitespace) {
            self.pos += 1;
        }
    }
    fn take(&mut self, token: &[u8]) -> bool {
        self.space();
        if self.text[self.pos..].starts_with(token) {
            self.pos += token.len();
            true
        } else {
            false
        }
    }
    fn expression(&mut self, depth: usize, and: bool) -> Result<ConditionNode, String> {
        if depth > 24 || self.nodes >= 128 {
            return Err("condition too complex".into());
        }
        self.nodes += 1;
        let mut node = if and {
            self.atom(depth + 1)?
        } else {
            self.expression(depth + 1, true)?
        };
        while self.take(if and { b"&&" } else { b"||" }) {
            let right = Box::new(if and {
                self.atom(depth + 1)?
            } else {
                self.expression(depth + 1, true)?
            });
            node = if and {
                ConditionNode::And {
                    left: Box::new(node),
                    right,
                }
            } else {
                ConditionNode::Or {
                    left: Box::new(node),
                    right,
                }
            };
        }
        Ok(node)
    }
    fn atom(&mut self, depth: usize) -> Result<ConditionNode, String> {
        if depth > 24 {
            return Err("condition too deep".into());
        }
        if self.take(b"!") {
            return Ok(ConditionNode::Not {
                value: Box::new(self.atom(depth + 1)?),
            });
        }
        if self.take(b"(") {
            let node = self.expression(depth + 1, false)?;
            if !self.take(b")") {
                return Err("missing closing parenthesis".into());
            }
            return Ok(node);
        }
        self.space();
        let start = self.pos;
        while self
            .text
            .get(self.pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            self.pos += 1;
        }
        let fact = std::str::from_utf8(&self.text[start..self.pos])
            .map_err(|_| "invalid fact")?
            .to_owned();
        let cmp = if self.take(b"<=") {
            Comparison::Le
        } else if self.take(b">=") {
            Comparison::Ge
        } else if self.take(b"==") {
            Comparison::Eq
        } else if self.take(b"!=") {
            Comparison::Ne
        } else if self.take(b"<") {
            Comparison::Lt
        } else if self.take(b">") {
            Comparison::Gt
        } else {
            return Err("expected comparison".into());
        };
        self.space();
        let start = self.pos;
        if self.text.get(self.pos) == Some(&b'-') {
            self.pos += 1;
        }
        while self.text.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
        }
        let value = std::str::from_utf8(&self.text[start..self.pos])
            .map_err(|_| "invalid number")?
            .parse::<i64>()
            .map_err(|_| "expected signed integer")?;
        Ok(ConditionNode::Compare { fact, cmp, value })
    }
}

impl RetailPriceDocument {
    pub fn parse(value: &Value) -> Result<Self, String> {
        let object = value.as_object().ok_or("price must be an object")?;
        let mut document = if object.contains_key("items") {
            Self {
                schema: RETAIL_PRICE_SCHEMA.into(),
                global: Some(strict_price(value)?),
                groups: vec![],
            }
        } else {
            let document: Self = serde_json::from_value(value.clone())
                .map_err(|e| format!("invalid retail document: {e}"))?;
            if document.schema != RETAIL_PRICE_SCHEMA {
                return Err("unknown retail price schema".into());
            }
            if let Some(global) = value.get("global").filter(|v| !v.is_null()) {
                strict_price(global)?;
            }
            if let Some(groups) = value.get("groups").and_then(Value::as_array) {
                for group in groups {
                    if let Some(base) = group.get("base").filter(|v| !v.is_null()) {
                        strict_price(base)?;
                    }
                    if let Some(rules) = group.get("rules").and_then(Value::as_array) {
                        for rule in rules {
                            strict_price(&rule["price"])?;
                        }
                    }
                }
            }
            document
        };
        if document.groups.len() > 128 {
            return Err("too many model groups".into());
        }
        let mut ids = BTreeSet::new();
        for group in &mut document.groups {
            group.model_group_id = uuid::Uuid::parse_str(&group.model_group_id)
                .map_err(|_| "invalid modelGroupID")?
                .to_string();
            if !ids.insert(group.model_group_id.clone()) || group.rules.len() > 128 {
                return Err("duplicate group or too many rules".into());
            }
            let mut rules = BTreeSet::new();
            for rule in &group.rules {
                if rule.id.is_empty()
                    || rule.id.len() > 128
                    || rule.label.len() > 256
                    || !rules.insert(&rule.id)
                {
                    return Err("invalid or duplicate rule ID/label".into());
                }
                rule.condition.compile()?;
            }
            if group.base.is_none() && group.rules.is_empty() {
                return Err("empty model group price branch".into());
            }
        }
        if document.leaves().is_empty() {
            return Err("retail document has no prices".into());
        }
        if object.contains_key("schema") {
            for price in document.leaves() {
                for item in &price.items {
                    if item.pricing.mode != "flat_fee"
                        && ![
                            "prompt_tokens",
                            "completion_tokens",
                            "prompt_cached_tokens",
                            "prompt_write_cached_tokens",
                        ]
                        .contains(&item.item_code.as_str())
                    {
                        return Err(format!(
                            "unsupported retail usage dimension {}",
                            item.item_code
                        ));
                    }
                }
            }
        }
        Ok(document)
    }
    pub fn leaves(&self) -> Vec<&ModelPrice> {
        self.global
            .iter()
            .chain(
                self.groups
                    .iter()
                    .flat_map(|g| g.base.iter().chain(g.rules.iter().map(|r| &r.price))),
            )
            .collect()
    }
    pub fn select<'a>(
        &'a self,
        group: Option<&str>,
        facts: &BTreeMap<String, i64>,
    ) -> Result<Option<&'a ModelPrice>, String> {
        if let Some(branch) = self
            .groups
            .iter()
            .find(|g| Some(g.model_group_id.as_str()) == group)
        {
            let compiled = branch
                .rules
                .iter()
                .map(|r| r.condition.compile())
                .collect::<Result<Vec<_>, _>>()?;
            for condition in &compiled {
                condition.validate_facts(facts)?;
            }
            for (rule, condition) in branch.rules.iter().zip(compiled) {
                if condition.evaluate(facts)? {
                    return Ok(Some(&rule.price));
                }
            }
            if let Some(base) = &branch.base {
                return Ok(Some(base));
            }
        }
        Ok(self.global.as_ref())
    }
    pub fn replace_global(value: Option<&Value>, price: ModelPrice) -> Result<Value, String> {
        if let Some(value) = value {
            let mut document = Self::parse(value)?;
            if value.get("schema").is_some() {
                document.global = Some(price);
                return serde_json::to_value(document).map_err(|e| e.to_string());
            }
        }
        serde_json::to_value(price).map_err(|e| e.to_string())
    }
}

fn fields(value: &Value, allowed: &[&str]) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("price component must be an object")?;
    if let Some(key) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("unknown price field {key}"));
    }
    Ok(())
}
fn strict_pricing(value: &Value) -> Result<(), String> {
    fields(value, &["mode", "flatFee", "usagePerUnit", "usageTiered"])?;
    if let Some(tiered) = value.get("usageTiered").filter(|v| !v.is_null()) {
        fields(tiered, &["tiers"])?;
        for tier in tiered["tiers"].as_array().ok_or("tiers must be an array")? {
            fields(tier, &["upTo", "pricePerUnit"])?;
            if tier.get("pricePerUnit").is_none() {
                return Err("pricePerUnit is required".into());
            }
        }
    }
    let pricing: super::pricing::Pricing =
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let rate = match pricing.mode.as_str() {
        "flat_fee" => pricing.flat_fee.ok_or("flatFee is required")?,
        "usage_per_unit" => pricing.usage_per_unit.ok_or("usagePerUnit is required")?,
        "usage_tiered" | "usage_volume" => {
            let tiers = &pricing.usage_tiered.ok_or("usageTiered is required")?.tiers;
            if tiers.is_empty() {
                return Err("tiers must not be empty".into());
            }
            let mut previous = 0;
            for (index, tier) in tiers.iter().enumerate() {
                if tier.price_per_unit < Decimal::ZERO {
                    return Err("negative tier price".into());
                }
                if index + 1 == tiers.len() {
                    if tier.up_to.is_some() {
                        return Err("last tier must be unbounded".into());
                    }
                } else {
                    let upper = tier.up_to.ok_or("non-final tier must be bounded")?;
                    if upper <= previous {
                        return Err("tiers must have increasing positive bounds".into());
                    }
                    previous = upper;
                }
            }
            Decimal::ZERO
        }
        _ => return Err("unknown pricing mode".into()),
    };
    if rate < Decimal::ZERO {
        return Err("price must not be negative".into());
    }
    Ok(())
}
fn strict_price(value: &Value) -> Result<ModelPrice, String> {
    fields(value, &["items"])?;
    for item in value["items"].as_array().ok_or("items must be an array")? {
        fields(item, &["itemCode", "pricing", "promptWriteCacheVariants"])?;
        item["itemCode"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("itemCode is required")?;
        strict_pricing(&item["pricing"])?;
        if let Some(variants) = item.get("promptWriteCacheVariants") {
            for variant in variants.as_array().ok_or("variants must be an array")? {
                fields(variant, &["variantCode", "pricing"])?;
                variant["variantCode"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("variantCode is required")?;
                strict_pricing(&variant["pricing"])?;
            }
        }
    }
    let price: ModelPrice = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    if price.items.is_empty() {
        return Err("price must contain at least one item".into());
    }
    Ok(price)
}

/// Stored once in the existing durable reservation JSON; recovery evaluates this tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenRetailPlan {
    pub facts_version: String,
    pub document: RetailPriceDocument,
    pub model_group_id: Option<String>,
    pub model_group_revision: Option<i64>,
    pub price_version_id: i64,
    pub price_reference: String,
    pub effective_at: chrono::DateTime<chrono::Utc>,
    pub timezone_offset_seconds: i32,
    pub project_multiplier: Decimal,
    pub project_adjustment_source: String,
}
impl FrozenRetailPlan {
    pub fn freeze(mut self) -> Result<Self, String> {
        self.document = RetailPriceDocument::parse(
            &serde_json::to_value(&self.document).map_err(|e| e.to_string())?,
        )?;
        if self.facts_version != FACTS_VERSION
            || self.project_multiplier < Decimal::ZERO
            || chrono::FixedOffset::east_opt(self.timezone_offset_seconds).is_none()
            || self.model_group_id.is_some() != self.model_group_revision.is_some()
            || self.model_group_revision.is_some_and(|v| v <= 0)
        {
            return Err("invalid frozen retail plan context".into());
        }
        if let Some(id) = &mut self.model_group_id {
            *id = uuid::Uuid::parse_str(id)
                .map_err(|_| "invalid trusted model group ID")?
                .to_string();
        }
        self.document
            .groups
            .retain(|g| Some(&g.model_group_id) == self.model_group_id.as_ref());
        for group in &mut self.document.groups {
            for rule in &mut group.rules {
                rule.condition = PriceCondition::Ast(rule.condition.compile()?);
            }
        }
        Ok(self)
    }
    pub fn select_price(
        &self,
        actual: &BTreeMap<String, i64>,
    ) -> Result<Option<&ModelPrice>, String> {
        let mut facts = actual.clone();
        facts.extend(self.time_facts()?);
        self.document.select(self.model_group_id.as_deref(), &facts)
    }
    fn time_facts(&self) -> Result<BTreeMap<String, i64>, String> {
        use chrono::{Datelike, Timelike};
        if self.facts_version != FACTS_VERSION || self.document.schema != RETAIL_PRICE_SCHEMA {
            return Err("unsupported frozen facts or price document version".into());
        }
        let offset = chrono::FixedOffset::east_opt(self.timezone_offset_seconds)
            .ok_or("invalid frozen timezone offset")?;
        let time = self.effective_at.with_timezone(&offset);
        let mut facts = BTreeMap::new();
        for (key, value) in [
            ("hour", i64::from(time.hour())),
            ("minute", i64::from(time.minute())),
            ("weekday", i64::from(time.weekday().num_days_from_monday())),
            ("month", i64::from(time.month())),
            ("day", i64::from(time.day())),
            (
                "date",
                i64::from(time.year()) * 10000
                    + i64::from(time.month()) * 100
                    + i64::from(time.day()),
            ),
        ] {
            facts.insert(key.into(), value);
        }
        Ok(facts)
    }
    /// Preserves the existing estimate policy: unknown token conditions remain reachable.
    /// This is a maximum estimated charge, not a bound on final usage or charge.
    pub fn maximum_estimated_charge<F>(&self, mut estimate: F) -> Result<Decimal, String>
    where
        F: FnMut(&ModelPrice) -> Result<Decimal, String>,
    {
        let mut maximum = Decimal::ZERO;
        for leaf in self.reachable_prices()? {
            let amount = estimate(leaf)?;
            if amount < Decimal::ZERO {
                return Err("negative estimated price".into());
            }
            maximum = maximum.max(amount);
        }
        maximum
            .checked_mul(self.project_multiplier)
            .ok_or_else(|| "project estimate overflow".into())
    }
    pub fn reachable_prices(&self) -> Result<Vec<&ModelPrice>, String> {
        let facts = self.time_facts()?;
        let mut prices = Vec::new();
        if let Some(group) = self
            .document
            .groups
            .iter()
            .find(|g| Some(g.model_group_id.as_str()) == self.model_group_id.as_deref())
        {
            for rule in &group.rules {
                match rule.condition.compile()?.known_result(&facts)? {
                    Some(false) => {}
                    Some(true) => {
                        prices.push(&rule.price);
                        return Ok(prices);
                    }
                    None => prices.push(&rule.price),
                }
            }
            if let Some(base) = &group.base {
                prices.push(base);
                return Ok(prices);
            }
        }
        prices.extend(self.document.global.iter());
        Ok(prices)
    }
    /// Proves coverage using frozen time only; token conditions remain unknown.
    pub fn has_complete_coverage(&self) -> Result<bool, String> {
        let facts = self.time_facts()?;
        if self.document.global.is_some() {
            return Ok(true);
        }
        if let Some(group) = self
            .document
            .groups
            .iter()
            .find(|g| Some(g.model_group_id.as_str()) == self.model_group_id.as_deref())
        {
            if group.base.is_some() {
                return Ok(true);
            }
            for rule in &group.rules {
                if rule.condition.compile()?.known_result(&facts)? == Some(true) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn price(rate: &str) -> Value {
        json!({"items":[{"itemCode":"prompt_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":rate}}]})
    }
    fn document() -> Value {
        json!({"schema":RETAIL_PRICE_SCHEMA,"global":price("1"),"groups":[{"modelGroupID":"00000000-0000-0000-0000-000000000001","rules":[{"id":"free","label":"free","condition":"prompt_tokens >= 10","price":price("0")},{"id":"expensive","label":"large","condition":"completion_tokens > 100","price":price("100")}],"base":price("2")}]})
    }
    #[test]
    fn first_match_zero_base_global_and_errors() {
        let doc = RetailPriceDocument::parse(&document()).unwrap();
        let group = Some("00000000-0000-0000-0000-000000000001");
        let mut facts = BTreeMap::from([
            ("prompt_tokens".into(), 10),
            ("completion_tokens".into(), 200),
        ]);
        assert_eq!(
            doc.select(group, &facts).unwrap().unwrap(),
            &strict_price(&price("0")).unwrap()
        );
        facts.insert("prompt_tokens".into(), 0);
        facts.insert("completion_tokens".into(), 0);
        assert_eq!(
            doc.select(group, &facts).unwrap().unwrap(),
            &strict_price(&price("2")).unwrap()
        );
        assert_eq!(
            doc.select(None, &facts).unwrap().unwrap(),
            &strict_price(&price("1")).unwrap()
        );
        facts.remove("completion_tokens");
        assert!(doc.select(group, &facts).is_err());
    }
    #[test]
    fn strict_shapes_and_derived_groups() {
        let original = document();
        let replaced = RetailPriceDocument::replace_global(
            Some(&original),
            strict_price(&price("3")).unwrap(),
        )
        .unwrap();
        assert_eq!(replaced["groups"], original["groups"]);
        for bad in [
            json!({"items":[],"groups":[]}),
            json!({"schema":"future","global":price("1"),"groups":[]}),
            json!({"items":[{"itemCode":"prompt_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":"1","script":"x"}}]}),
        ] {
            assert!(RetailPriceDocument::parse(&bad).is_err());
        }
        assert!(
            RetailPriceDocument::parse(&price("1"))
                .unwrap()
                .groups
                .is_empty()
        );
        let unknown = json!({"items":[{"itemCode":"web_search_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":"1"}}]});
        assert!(
            RetailPriceDocument::parse(&unknown).is_ok(),
            "legacy unknown dimensions preserve existing behavior"
        );
        assert!(
            RetailPriceDocument::parse(
                &json!({"schema":RETAIL_PRICE_SCHEMA,"global":unknown,"groups":[]})
            )
            .is_err()
        );
        assert!(RetailPriceDocument::parse(&json!({"schema":RETAIL_PRICE_SCHEMA,"global":{"items":[{"itemCode":"request","pricing":{"mode":"flat_fee","flatFee":"1"}}]},"groups":[]})).is_ok());
    }
    #[test]
    fn duplicate_price_items_and_variants_preserve_order_in_legacy_and_document_leaves() {
        let item = json!({"itemCode":"prompt_write_cached_tokens","pricing":{"mode":"usage_per_unit","usagePerUnit":"1"},"promptWriteCacheVariants":[{"variantCode":"5m","pricing":{"mode":"usage_per_unit","usagePerUnit":"2"}},{"variantCode":"5m","pricing":{"mode":"usage_per_unit","usagePerUnit":"3"}}]});
        let price = json!({"items":[item.clone(),item]});
        let legacy = RetailPriceDocument::parse(&price).unwrap();
        assert_eq!(serde_json::to_value(legacy.global.unwrap()).unwrap(), price);
        let wrapped = json!({"schema":RETAIL_PRICE_SCHEMA,"global":price,"groups":[{"modelGroupID":"00000000-0000-0000-0000-000000000001","base":price,"rules":[{"id":"duplicate-compatible","label":"compatible","condition":"hour >= 0","price":price}]}]});
        let parsed = RetailPriceDocument::parse(&wrapped).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), wrapped);
    }
    #[test]
    fn text_ast_same_and_reject_untrusted_facts() {
        let text = PriceCondition::Text(
            "!(hour < 9) && (prompt_tokens >= 10 || completion_tokens == 0)".into(),
        );
        let ast = text.compile().unwrap();
        let value = serde_json::to_value(&ast).unwrap();
        assert_eq!(
            serde_json::from_value::<PriceCondition>(value)
                .unwrap()
                .compile()
                .unwrap(),
            ast
        );
        assert!(
            PriceCondition::Text("header >= 1".into())
                .compile()
                .is_err()
        );
        assert!(
            PriceCondition::Text("hour > 1; execute()".into())
                .compile()
                .is_err()
        );
        assert!(
            PriceCondition::Text("hour > 1 || completion_tokens > 1".into())
                .compile()
                .unwrap()
                .evaluate(&BTreeMap::from([("hour".into(), 12)]))
                .is_err()
        );
    }
    #[test]
    fn conditions_reject_lossy_json_integer_boundaries_in_text_and_ast() {
        for value in [-MAX_CONDITION_INTEGER, MAX_CONDITION_INTEGER] {
            assert!(
                PriceCondition::Text(format!("prompt_tokens > {value}"))
                    .compile()
                    .is_ok()
            );
            assert!(
                PriceCondition::Ast(ConditionNode::Compare {
                    fact: "prompt_tokens".into(),
                    cmp: Comparison::Gt,
                    value
                })
                .compile()
                .is_ok()
            );
        }
        for value in [
            i64::MIN,
            -MAX_CONDITION_INTEGER - 1,
            MAX_CONDITION_INTEGER + 1,
            i64::MAX,
        ] {
            assert!(
                PriceCondition::Text(format!("prompt_tokens > {value}"))
                    .compile()
                    .is_err()
            );
            assert!(
                PriceCondition::Ast(ConditionNode::Compare {
                    fact: "prompt_tokens".into(),
                    cmp: Comparison::Gt,
                    value
                })
                .compile()
                .is_err()
            );
        }
    }
    #[test]
    fn reservation_estimate_covers_unselected_high_leaf() {
        let frozen = FrozenRetailPlan {
            facts_version: FACTS_VERSION.into(),
            document: RetailPriceDocument::parse(&document()).unwrap(),
            model_group_id: Some("00000000-0000-0000-0000-000000000001".into()),
            model_group_revision: Some(1),
            price_version_id: 1,
            price_reference: "published".into(),
            effective_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            timezone_offset_seconds: 0,
            project_multiplier: Decimal::from(2),
            project_adjustment_source: "project".into(),
        };
        assert_eq!(
            frozen
                .maximum_estimated_charge(|price| Ok(price.items[0]
                    .pricing
                    .usage_per_unit
                    .unwrap()))
                .unwrap(),
            Decimal::from(200)
        );
        let frozen = frozen.freeze().unwrap();
        let mut incomplete = frozen.clone();
        incomplete.document.global = None;
        incomplete.document.groups[0].base = None;
        assert!(
            !incomplete.has_complete_coverage().unwrap(),
            "unknown token conditions cannot prove coverage"
        );
        incomplete.document.groups[0].base = Some(strict_price(&price("0")).unwrap());
        assert!(
            incomplete.has_complete_coverage().unwrap(),
            "zero is a complete default"
        );
        assert!(matches!(
            frozen.document.groups[0].rules[0].condition,
            PriceCondition::Ast(_)
        ));
        let mut document = frozen.document.clone();
        document.groups[0].rules[0].condition=PriceCondition::Text("hour == 1 && minute == 0 && weekday == 3 && month == 1 && day == 1 && date == 19700101".into());
        let frozen = FrozenRetailPlan {
            document,
            timezone_offset_seconds: 3600,
            ..frozen
        }
        .freeze()
        .unwrap();
        let facts = BTreeMap::from([
            ("prompt_tokens".into(), 10),
            ("completion_tokens".into(), 200),
            ("hour".into(), 99),
        ]);
        assert_eq!(
            frozen.select_price(&facts).unwrap().unwrap(),
            &strict_price(&price("0")).unwrap()
        );
        assert_eq!(
            frozen
                .maximum_estimated_charge(|price| Ok(price.items[0]
                    .pricing
                    .usage_per_unit
                    .unwrap()))
                .unwrap(),
            Decimal::ZERO,
            "a guaranteed frozen-time first match makes later leaves unreachable"
        );
        let mut time_covered = frozen;
        time_covered.document.global = None;
        time_covered.document.groups[0].base = None;
        assert!(
            time_covered.has_complete_coverage().unwrap(),
            "a guaranteed frozen-time match covers usage without a default"
        );
    }
}
