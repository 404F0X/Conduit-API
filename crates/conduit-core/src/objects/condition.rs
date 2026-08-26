//! Condition tree evaluator, ported from `conduit/internal/objects/condition.go`.
//!
//! The Go implementation compiles each condition into an [expr-lang] boolean
//! expression and runs it against a data map. Per `RUST-P9-002` the Rust port
//! may use a self-contained evaluator as long as the semantics match; this
//! module evaluates the tree directly without an expression engine.
//!
//! Verified against `conduit/internal/objects/condition_test.go`:
//! - A group with no children evaluates to `true`.
//! - Group logic defaults to AND; `or` (case-insensitive) selects OR.
//! - Leaf operators: `lt`/`<`, `lte`/`<=`, `gt`/`>`, `gte`/`>=`, `eq`/`=`/`==`,
//!   `ne`/`!=`/`<>` (case-insensitive, trimmed). Unknown operators are invalid.
//! - `daily_time` is special: `within`/`not_within` over an `HH:mm-HH:mm` range
//!   read the wall-clock time from `data["now"]` (RFC 3339). Without `now` the
//!   leaf is `false`, matching Go where `now` is supplied by the caller.
//! - Any structural problem (empty field, missing value, unknown operator)
//!   collapses the whole evaluation to `false`, matching Go where `ToExpr` and
//!   `Run` errors map to `false`.
//!
//! # Asymmetry faithfully reproduced
//! Go treats an *omitted* `type` differently depending on position: at the
//! top level `ToExpr` reads it as a group, while nested children via
//! `nodeToExpr` read it as a leaf. See [`classify`].
//!
//! [expr-lang]: https://github.com/expr-lang/expr

use chrono::{DateTime, FixedOffset, Timelike};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::fmt;

/// Whether a node is a single condition or a group, decoded from the JSON
/// `type` field.
///
/// Mirrors Go `ConditionType`. The JSON value `""`, a missing field, or any
/// unknown value decodes to [`ConditionType::Omitted`], whose meaning depends
/// on node position (see [`classify`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConditionType {
    /// JSON `""`, omitted, or unknown. Group at the top level, leaf when nested.
    #[default]
    Omitted,
    /// JSON `"condition"`. Always a leaf.
    Condition,
    /// JSON `"group"`. Always a group.
    Group,
}

impl Serialize for ConditionType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            Self::Omitted => "",
            Self::Condition => "condition",
            Self::Group => "group",
        })
    }
}

impl<'de> Deserialize<'de> for ConditionType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ConditionType;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("condition type")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(match value {
                    "condition" => ConditionType::Condition,
                    "group" => ConditionType::Group,
                    // "" and any unknown value behave like an omitted type.
                    _ => ConditionType::Omitted,
                })
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ConditionType::Omitted)
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// Structural validation failure for a [`Condition`] tree.
///
/// Mirrors the error conditions of Go `ToExpr`/`literalExpr`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConditionError {
    #[error("field is required")]
    FieldRequired,
    #[error("value is required")]
    ValueRequired,
    #[error("unsupported operator {0:?}")]
    UnsupportedOperator(String),
    #[error("unsupported operator {0:?} for daily_time")]
    UnsupportedDailyOperator(String),
    #[error("unsupported value type")]
    UnsupportedValueType,
}

/// A condition tree node, ported 1:1 from Go `objects.Condition`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Condition {
    /// `condition` or `group`; omitted/unknown decodes positionally (see
    /// [`classify`]). Serializes as JSON `type`.
    #[serde(default, rename = "type")]
    pub r#type: ConditionType,
    /// Group join logic: `and` (default) or `or`. Ignored for leaves.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub logic: String,
    /// Children of a group node.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// Field name compared for a leaf.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub field: String,
    /// Comparison operator for a leaf.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub operator: String,
    /// Comparison value for a leaf; `null`/absent makes the node invalid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl Condition {
    /// Evaluate against a JSON data object. Convenience wrapper for [`evaluate`].
    pub fn evaluate(&self, data: &Value) -> bool {
        evaluate(self, data)
    }
}

/// Evaluate a condition tree against a JSON data object.
///
/// Returns `false` for any structural or runtime problem, matching Go
/// `objects.Evaluate`.
pub fn evaluate(condition: &Condition, data: &Value) -> bool {
    validate(condition).is_ok() && run(condition, data)
}

/// Validate the structure of a condition tree without evaluating it.
///
/// Mirrors Go `ToExpr` error conditions. Useful for config validation;
/// [`evaluate`] collapses any error to `false`.
pub fn validate(condition: &Condition) -> Result<(), ConditionError> {
    validate_node(condition, true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Leaf,
    Group,
}

/// Classify a node, reproducing Go's position-dependent handling of an omitted
/// `type`: top-level omitted -> group, nested omitted -> leaf.
fn classify(kind: ConditionType, top: bool) -> NodeKind {
    match kind {
        ConditionType::Condition => NodeKind::Leaf,
        ConditionType::Group => NodeKind::Group,
        ConditionType::Omitted => {
            if top {
                NodeKind::Group
            } else {
                NodeKind::Leaf
            }
        }
    }
}

fn validate_node(condition: &Condition, top: bool) -> Result<(), ConditionError> {
    match classify(condition.r#type, top) {
        NodeKind::Leaf => validate_leaf(condition),
        NodeKind::Group => {
            for child in &condition.conditions {
                validate_node(child, false)?;
            }
            Ok(())
        }
    }
}

fn validate_leaf(condition: &Condition) -> Result<(), ConditionError> {
    let field = condition.field.trim();
    if field.is_empty() {
        return Err(ConditionError::FieldRequired);
    }
    let Some(value) = &condition.value else {
        return Err(ConditionError::ValueRequired);
    };
    if field.eq_ignore_ascii_case("daily_time") {
        return match normalize_daily_operator(&condition.operator) {
            Some(_) => Ok(()),
            None => Err(ConditionError::UnsupportedDailyOperator(
                condition.operator.clone(),
            )),
        };
    }
    if normalize_operator(&condition.operator).is_none() {
        return Err(ConditionError::UnsupportedOperator(
            condition.operator.clone(),
        ));
    }
    if !is_literal_value(value) {
        return Err(ConditionError::UnsupportedValueType);
    }
    Ok(())
}

fn run(condition: &Condition, data: &Value) -> bool {
    run_node(condition, data, true)
}

fn run_node(condition: &Condition, data: &Value, top: bool) -> bool {
    match classify(condition.r#type, top) {
        NodeKind::Leaf => run_leaf(condition, data),
        NodeKind::Group => {
            if condition.conditions.is_empty() {
                return true;
            }
            let or = is_or(&condition.logic);
            for child in &condition.conditions {
                let matched = run_node(child, data, false);
                if or {
                    if matched {
                        return true;
                    }
                } else if !matched {
                    return false;
                }
            }
            // Loop completed: OR -> all false, AND -> all true.
            !or
        }
    }
}

fn run_leaf(condition: &Condition, data: &Value) -> bool {
    let field = condition.field.trim();
    let Some(value) = &condition.value else {
        return false;
    };
    if field.eq_ignore_ascii_case("daily_time") {
        let Some(within) = normalize_daily_operator(&condition.operator) else {
            return false;
        };
        let Some(range) = value.as_str() else {
            return false;
        };
        let Some(now_minutes) = now_minutes(data) else {
            return false;
        };
        return daily_time_within(now_minutes, range) == within;
    }
    let Some(actual) = data.get(field) else {
        return false;
    };
    let Some(op) = normalize_operator(&condition.operator) else {
        return false;
    };
    apply_operator(op, actual, value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operator {
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
    Ne,
}

fn normalize_operator(operator: &str) -> Option<Operator> {
    match operator.trim().to_ascii_lowercase().as_str() {
        "lt" | "<" => Some(Operator::Lt),
        "lte" | "<=" => Some(Operator::Lte),
        "gt" | ">" => Some(Operator::Gt),
        "gte" | ">=" => Some(Operator::Gte),
        "eq" | "=" | "==" => Some(Operator::Eq),
        "ne" | "!=" | "<>" => Some(Operator::Ne),
        _ => None,
    }
}

/// Returns `Some(true)` for `within`, `Some(false)` for `not_within`.
fn normalize_daily_operator(operator: &str) -> Option<bool> {
    match operator.trim().to_ascii_lowercase().as_str() {
        "within" => Some(true),
        "not_within" => Some(false),
        _ => None,
    }
}

fn is_or(logic: &str) -> bool {
    logic.trim().eq_ignore_ascii_case("or")
}

fn is_literal_value(value: &Value) -> bool {
    matches!(value, Value::String(_) | Value::Bool(_) | Value::Number(_))
}

fn apply_operator(op: Operator, actual: &Value, expected: &Value) -> bool {
    match op {
        Operator::Lt | Operator::Lte | Operator::Gt | Operator::Gte => {
            match (actual.as_f64(), expected.as_f64()) {
                (Some(a), Some(e)) => match op {
                    Operator::Lt => a < e,
                    Operator::Lte => a <= e,
                    Operator::Gt => a > e,
                    Operator::Gte => a >= e,
                    _ => false,
                },
                _ => false,
            }
        }
        Operator::Eq => values_equal(actual, expected),
        Operator::Ne => !values_equal(actual, expected),
    }
}

fn values_equal(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(e)) => a.as_f64() == e.as_f64(),
        (Value::String(a), Value::String(e)) => a == e,
        (Value::Bool(a), Value::Bool(e)) => a == e,
        _ => false,
    }
}

/// Minutes-of-day for `data["now"]` (RFC 3339), using its wall-clock hour/minute
/// to match Go `now.Hour()`/`now.Minute()` in the time's own location.
fn now_minutes(data: &Value) -> Option<i32> {
    let raw = data.get("now")?.as_str()?;
    let parsed = DateTime::<FixedOffset>::parse_from_rfc3339(raw).ok()?;
    let time = parsed.time();
    Some(time.hour() as i32 * 60 + time.minute() as i32)
}

/// Ported from `xtime.DailyTimeWithin`: half-open `[start, end)`, crossing
/// midnight when `start > end`.
fn daily_time_within(now_minutes: i32, range: &str) -> bool {
    let Some((start, end)) = parse_daily_range(range) else {
        return false;
    };
    if start == end {
        return false;
    }
    if start > end {
        now_minutes >= start || now_minutes < end
    } else {
        now_minutes >= start && now_minutes < end
    }
}

/// Ported from `xtime.ParseDailyTimeRange`: `HH:mm-HH:mm`.
fn parse_daily_range(value: &str) -> Option<(i32, i32)> {
    let (start_raw, end_raw) = value.split_once('-')?;
    Some((parse_daily_clock(start_raw)?, parse_daily_clock(end_raw)?))
}

/// Ported from `xtime.parseDailyClock` (`time.Parse("15:04")`).
fn parse_daily_clock(value: &str) -> Option<i32> {
    let mut parts = value.trim().split(':');
    let hours: i32 = parts.next()?.trim().parse().ok()?;
    let minutes: i32 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() || !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some(hours * 60 + minutes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn leaf(field: &str, operator: &str, value: Value) -> Condition {
        Condition {
            field: field.to_string(),
            operator: operator.to_string(),
            value: Some(value),
            ..Default::default()
        }
    }

    fn group(logic: &str, conditions: Vec<Condition>) -> Condition {
        Condition {
            logic: logic.to_string(),
            conditions,
            ..Default::default()
        }
    }

    #[test]
    fn empty_group_matches() {
        assert!(evaluate(&Condition::default(), &json!({})));
    }

    #[test]
    fn gt_matches() {
        let cond = group("", vec![leaf("promptTokens", "gt", json!(100))]);
        assert!(evaluate(&cond, &json!({"promptTokens": 101})));
    }

    #[test]
    fn lt_does_not_match() {
        let cond = group("", vec![leaf("promptTokens", "lt", json!(100))]);
        assert!(!evaluate(&cond, &json!({"promptTokens": 100})));
    }

    #[test]
    fn lte_matches() {
        let cond = group("", vec![leaf("promptTokens", "lte", json!(100))]);
        assert!(evaluate(&cond, &json!({"promptTokens": 100})));
    }

    #[test]
    fn gte_matches() {
        let cond = group("", vec![leaf("promptTokens", "gte", json!(100))]);
        assert!(evaluate(&cond, &json!({"promptTokens": 100})));
    }

    #[test]
    fn eq_matches() {
        let cond = group("", vec![leaf("model", "eq", json!("gpt-4o"))]);
        assert!(evaluate(&cond, &json!({"model": "gpt-4o"})));
    }

    #[test]
    fn ne_matches() {
        let cond = group("", vec![leaf("model", "ne", json!("gpt-4o"))]);
        assert!(evaluate(&cond, &json!({"model": "claude-3-7-sonnet"})));
    }

    #[test]
    fn and_matches() {
        let cond = group(
            "and",
            vec![
                leaf("promptTokens", "gt", json!(100)),
                leaf("model", "eq", json!("gpt-4o")),
            ],
        );
        assert!(evaluate(
            &cond,
            &json!({"promptTokens": 101, "model": "gpt-4o"})
        ));
    }

    #[test]
    fn or_matches() {
        let cond = group(
            "or",
            vec![
                leaf("promptTokens", "lt", json!(100)),
                leaf("model", "eq", json!("gpt-4o")),
            ],
        );
        assert!(evaluate(
            &cond,
            &json!({"promptTokens": 1000, "model": "gpt-4o"})
        ));
    }

    #[test]
    fn and_does_not_match_when_one_branch_false() {
        let cond = group(
            "and",
            vec![
                leaf("promptTokens", "gt", json!(100)),
                leaf("model", "eq", json!("gpt-4o")),
            ],
        );
        assert!(!evaluate(
            &cond,
            &json!({"promptTokens": 101, "model": "claude"})
        ));
    }

    #[test]
    fn invalid_empty_field_returns_false() {
        let cond = group("", vec![leaf("", "eq", json!("gpt-4o"))]);
        assert!(!evaluate(&cond, &json!({"model": "gpt-4o"})));
    }

    #[test]
    fn daily_time_within_matches_across_midnight() {
        let cond = group("", vec![leaf("daily_time", "within", json!("22:00-06:00"))]);
        assert!(evaluate(&cond, &json!({"now": "2026-05-25T23:30:00Z"})));
    }

    #[test]
    fn daily_time_within_does_not_match_outside_range() {
        let cond = group("", vec![leaf("daily_time", "within", json!("22:00-06:00"))]);
        assert!(!evaluate(&cond, &json!({"now": "2026-05-25T12:00:00Z"})));
    }

    #[test]
    fn daily_time_not_within_matches_outside_range() {
        let cond = group(
            "",
            vec![leaf("daily_time", "not_within", json!("09:00-17:00"))],
        );
        assert!(evaluate(&cond, &json!({"now": "2026-05-25T18:00:00Z"})));
    }

    #[test]
    fn daily_time_without_now_returns_false() {
        let cond = group("", vec![leaf("daily_time", "within", json!("09:00-17:00"))]);
        assert!(!evaluate(&cond, &json!({})));
    }

    #[test]
    fn validate_rejects_unsupported_operator() {
        let cond = group("", vec![leaf("promptTokens", "contains", json!("1"))]);
        match validate(&cond) {
            Err(err) => assert!(
                err.to_string()
                    .contains(r#"unsupported operator "contains""#),
                "got: {err}"
            ),
            Ok(()) => panic!("expected validate to reject unsupported operator"),
        }
    }

    #[test]
    fn validate_accepts_empty_group() {
        assert!(validate(&Condition::default()).is_ok());
    }

    #[test]
    fn validate_accepts_daily_time_within() {
        let cond = group("", vec![leaf("daily_time", "within", json!("22:00-06:00"))]);
        assert!(validate(&cond).is_ok());
    }

    #[test]
    fn deserialize_integer_value() -> Result<(), serde_json::Error> {
        let cond: Condition = serde_json::from_str(
            r#"{"type":"condition","field":"promptTokens","operator":"gt","value":100}"#,
        )?;
        assert_eq!(cond.value.as_ref().and_then(Value::as_i64), Some(100));
        assert_eq!(cond.r#type, ConditionType::Condition);
        Ok(())
    }

    #[test]
    fn deserialize_omitted_type_evaluates_as_top_level_group() -> Result<(), serde_json::Error> {
        let cond: Condition = serde_json::from_str(r#"{"logic":"and","conditions":[]}"#)?;
        assert_eq!(cond.r#type, ConditionType::Omitted);
        assert!(evaluate(&cond, &json!({})));
        Ok(())
    }

    #[test]
    fn deserialize_nested_object_values() -> Result<(), serde_json::Error> {
        let cond: Condition = serde_json::from_str(
            r#"{"logic":"and","conditions":[
                {"type":"condition","field":"promptTokens","operator":"gte","value":100},
                {"type":"condition","field":"metadata","operator":"eq","value":{"maxTokens":2048,"weights":[1,2,3]}}
            ]}"#,
        )?;
        assert_eq!(
            cond.conditions[0].value.as_ref().and_then(Value::as_i64),
            Some(100)
        );
        assert_eq!(
            cond.conditions[1].value,
            Some(json!({"maxTokens": 2048, "weights": [1, 2, 3]}))
        );
        Ok(())
    }
}
