//! Restricted NEW API v1 pricing for text token usage and fixed HTTP request prices.
//! Supports decimal literals, arithmetic, numeric comparisons, boolean conditions,
//! ternaries, tier labels and min/max/abs/ceil/floor. Runtime values are USD before
//! a caller applies any source or group multiplier. Request, time, task and media
//! expressions and the cc12h variable are outside this profile.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const PROFILE: &str = "conduit-newapi-token-v1";
pub const UNSUPPORTED_CC12H_UNTRUSTED_USAGE: &str = "UNSUPPORTED_CC12H_UNTRUSTED_USAGE";
const MAX_SOURCE_BYTES: usize = 4096;
const MAX_NODES: usize = 256;
const MAX_PARSE_DEPTH: usize = 32;
const USD_SCALE: u64 = 1_000_000;

type Result<T> = std::result::Result<T, ExpressionError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionError {
    code: Option<&'static str>,
    message: String,
}

impl ExpressionError {
    pub fn code(&self) -> Option<&'static str> {
        self.code
    }
}

impl fmt::Display for ExpressionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(code) = self.code {
            write!(formatter, "{code}: ")?;
        }
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ExpressionError {}

fn error(message: impl Into<String>) -> ExpressionError {
    ExpressionError {
        code: None,
        message: message.into(),
    }
}

/// Trusted raw quantities. Writes are split into disjoint buckets: short-only,
/// Claude 1h, and OpenAI 12h. The caller must establish the applicable semantics
/// and supply zeros only for known absent categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenFacts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cache_write_1h: u64,
    pub cache_write_12h: u64,
    /// OpenAI totals include cache. Claude input excludes reads and writes.
    pub input_includes_cache: bool,
}

/// Persist this identity with the price. Recompile only after checking the
/// persisted profile and version against the supported interpreter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompiledExpression {
    source: String,
    profile: &'static str,
    version: u8,
    #[serde(skip)]
    root: Node,
    #[serde(skip)]
    references: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variable {
    Input,
    Output,
    Length,
    Read,
    Write,
    Write1h,
}

impl Variable {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "p" => Self::Input,
            "c" => Self::Output,
            "len" => Self::Length,
            "cr" => Self::Read,
            "cc" => Self::Write,
            "cc1h" => Self::Write1h,
            _ => return None,
        })
    }

    fn flag(self) -> u8 {
        1 << self as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Number(Decimal),
    Boolean(bool),
    Variable(Variable),
    Unary(Unary, Box<Node>),
    Binary(Binary, Box<Node>, Box<Node>),
    Select(Box<Node>, Box<Node>, Box<Node>),
    Tier(String, Box<Node>),
    Fixed(Decimal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unary {
    Negate,
    Not,
    Abs,
    Ceil,
    Floor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Binary {
    Add,
    Subtract,
    Multiply,
    Divide,
    Min,
    Max,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

impl Node {
    fn boolean(&self) -> bool {
        match self {
            Self::Boolean(_) | Self::Unary(Unary::Not, _) => true,
            Self::Binary(op, _, _) => op.boolean_result(),
            Self::Select(_, yes, _) => yes.boolean(),
            _ => false,
        }
    }

    fn has_marker(&self, include_tier: bool) -> bool {
        match self {
            Self::Fixed(_) => true,
            Self::Tier(_, _) if include_tier => true,
            Self::Unary(_, arg) | Self::Tier(_, arg) => arg.has_marker(include_tier),
            Self::Binary(_, a, b) => a.has_marker(include_tier) || b.has_marker(include_tier),
            Self::Select(cond, a, b) => {
                cond.has_marker(include_tier)
                    || a.has_marker(include_tier)
                    || b.has_marker(include_tier)
            }
            _ => false,
        }
    }

    fn valid_fixed_tree(&self) -> bool {
        match self {
            Self::Select(cond, a, b) => {
                !cond.has_marker(true) && a.valid_fixed_tree() && b.valid_fixed_tree()
            }
            Self::Tier(_, value) => matches!(**value, Self::Fixed(_)) || !value.has_marker(true),
            _ => false,
        }
    }
}

impl Binary {
    fn boolean_result(self) -> bool {
        matches!(
            self,
            Self::Lt | Self::Le | Self::Gt | Self::Ge | Self::Eq | Self::Ne | Self::And | Self::Or
        )
    }
}

pub fn compile(source: &str) -> Result<CompiledExpression> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(error("Expression exceeds 4096 bytes"));
    }
    let trimmed = source.trim();
    let body = trimmed.strip_prefix("v1:").unwrap_or(trimmed);
    if let Some((prefix, _)) = body.split_once(':')
        && prefix.starts_with('v')
        && prefix.len() > 1
        && prefix[1..].bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(error(format!(
            "Unsupported NEW API expression version {prefix}"
        )));
    }

    let mut parser = Parser {
        source: body,
        rest: body,
        references: 0,
        nodes: 0,
    };
    let root = parser.expression(0, 0)?;
    if !parser.rest.trim().is_empty() {
        return Err(parser.fail("Unexpected trailing input"));
    }
    if root.boolean() {
        return Err(error("The expression must return a numeric price"));
    }
    if root.has_marker(false) && !root.valid_fixed_tree() {
        return Err(error(
            "fixed requires a literal USD amount as the complete price of a tier in a conditional pricing tree",
        ));
    }
    Ok(CompiledExpression {
        source: source.into(),
        profile: PROFILE,
        version: 1,
        root,
        references: parser.references,
    })
}

struct Parser<'a> {
    source: &'a str,
    rest: &'a str,
    references: u8,
    nodes: usize,
}

impl Parser<'_> {
    fn fail(&self, message: &str) -> ExpressionError {
        error(format!(
            "{message} at expression byte {}",
            self.source.len() - self.rest.len()
        ))
    }

    fn take(&mut self, token: &str) -> bool {
        self.rest = self.rest.trim_start();
        if let Some(rest) = self.rest.strip_prefix(token) {
            self.rest = rest;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, token: &str) -> Result<()> {
        if !self.take(token) {
            return Err(self.fail(&format!("Expected {token:?}")));
        }
        Ok(())
    }

    fn count(&mut self) -> Result<()> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(self.fail("Expression is too complex"));
        }
        Ok(())
    }

    fn expression(&mut self, min_precedence: u8, depth: usize) -> Result<Node> {
        if depth > MAX_PARSE_DEPTH {
            return Err(self.fail("Expression nesting exceeds 32 levels"));
        }
        self.count()?;
        let mut left = self.primary(depth)?;
        const OPERATORS: &[(&str, u8, Binary)] = &[
            ("||", 1, Binary::Or),
            ("&&", 2, Binary::And),
            ("==", 3, Binary::Eq),
            ("!=", 3, Binary::Ne),
            ("<=", 4, Binary::Le),
            (">=", 4, Binary::Ge),
            ("<", 4, Binary::Lt),
            (">", 4, Binary::Gt),
            ("+", 5, Binary::Add),
            ("-", 5, Binary::Subtract),
            ("*", 6, Binary::Multiply),
            ("/", 6, Binary::Divide),
        ];
        loop {
            self.rest = self.rest.trim_start();
            let Some(&(symbol, precedence, op)) = OPERATORS
                .iter()
                .find(|(symbol, _, _)| self.rest.starts_with(symbol))
            else {
                break;
            };
            if precedence < min_precedence {
                break;
            }
            self.expect(symbol)?;
            let right = self.expression(precedence + 1, depth + 1)?;
            let valid = match op {
                Binary::And | Binary::Or => left.boolean() && right.boolean(),
                Binary::Eq | Binary::Ne => left.boolean() == right.boolean(),
                _ => !left.boolean() && !right.boolean(),
            };
            if !valid {
                return Err(self.fail(&format!("Invalid operand types for {symbol}")));
            }
            left = Node::Binary(op, Box::new(left), Box::new(right));
        }
        if min_precedence == 0 && self.take("?") {
            let yes = self.expression(0, depth + 1)?;
            self.expect(":")?;
            let no = self.expression(0, depth + 1)?;
            if !left.boolean() || yes.boolean() != no.boolean() {
                return Err(self.fail("Expected a boolean condition and matching branch types"));
            }
            left = Node::Select(Box::new(left), Box::new(yes), Box::new(no));
        }
        Ok(left)
    }

    fn primary(&mut self, depth: usize) -> Result<Node> {
        if self.take("(") {
            let value = self.expression(0, depth + 1)?;
            self.expect(")")?;
            return Ok(value);
        }
        for (symbol, op) in [("-", Unary::Negate), ("!", Unary::Not)] {
            if self.take(symbol) {
                let value = self.expression(7, depth + 1)?;
                if value.boolean() != (op == Unary::Not) {
                    return Err(self.fail(&format!("Invalid operand type for {symbol}")));
                }
                return Ok(Node::Unary(op, Box::new(value)));
            }
        }

        let start = self.rest.as_bytes().first().copied().unwrap_or_default();
        if start.is_ascii_digit() || start == b'.' {
            return self.number().map(Node::Number);
        }
        let length = self
            .rest
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            .count();
        if length == 0 {
            return Err(self.fail("Expected a number, supported variable, or function"));
        }
        let (name, rest) = self.rest.split_at(length);
        self.rest = rest;
        if name == "cc12h" {
            return Err(ExpressionError {
                code: Some(UNSUPPORTED_CC12H_UNTRUSTED_USAGE),
                message: "The cc12h variable requires trusted 12-hour cache-write usage".into(),
            });
        }
        if self.take("(") {
            return self.call(name, depth);
        }
        match name {
            "true" => Ok(Node::Boolean(true)),
            "false" => Ok(Node::Boolean(false)),
            _ => {
                let var = Variable::parse(name)
                    .ok_or_else(|| self.fail(&format!("Unsupported variable {name:?}")))?;
                self.references |= var.flag();
                Ok(Node::Variable(var))
            }
        }
    }

    fn number(&mut self) -> Result<Decimal> {
        self.rest = self.rest.trim_start();
        let length = self
            .rest
            .bytes()
            .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
            .count();
        let (literal, rest) = self.rest.split_at(length);
        self.rest = rest;
        Decimal::from_str_exact(literal)
            .map_err(|_| self.fail("Invalid or unrepresentable decimal literal"))
    }

    fn call(&mut self, name: &str, depth: usize) -> Result<Node> {
        let node = match name {
            "tier" => {
                self.expect("\"")?;
                let end = self
                    .rest
                    .find('"')
                    .ok_or_else(|| self.fail("Unterminated tier label"))?;
                let (label, after) = self.rest.split_at(end);
                if label.chars().any(|ch| ch.is_control() || ch == '\\') {
                    return Err(self.fail("Unsupported escape or control character in tier label"));
                }
                self.rest = &after[1..];
                self.expect(",")?;
                Node::Tier(label.into(), Box::new(self.numeric_argument(depth)?))
            }
            "fixed" => {
                let amount = self.number()?;
                Node::Fixed(
                    amount
                        .checked_mul(Decimal::from(USD_SCALE))
                        .ok_or_else(|| self.fail("Fixed USD amount exceeds the supported range"))?,
                )
            }
            "min" | "max" => {
                let a = self.numeric_argument(depth)?;
                self.expect(",")?;
                let b = self.numeric_argument(depth)?;
                let op = if name == "min" {
                    Binary::Min
                } else {
                    Binary::Max
                };
                Node::Binary(op, Box::new(a), Box::new(b))
            }
            "abs" | "ceil" | "floor" => {
                let op = match name {
                    "abs" => Unary::Abs,
                    "ceil" => Unary::Ceil,
                    _ => Unary::Floor,
                };
                Node::Unary(op, Box::new(self.numeric_argument(depth)?))
            }
            _ => return Err(self.fail(&format!("Unsupported function {name:?}"))),
        };
        self.expect(")")?;
        Ok(node)
    }

    fn numeric_argument(&mut self, depth: usize) -> Result<Node> {
        let node = self.expression(0, depth + 1)?;
        if node.boolean() {
            return Err(self.fail("Expected a numeric function argument"));
        }
        Ok(node)
    }
}

impl CompiledExpression {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn profile(&self) -> &'static str {
        self.profile
    }

    pub fn version(&self) -> u8 {
        self.version
    }

    /// Canonical fingerprint input: profile UTF-8, one version byte, and exact
    /// source UTF-8, each preceded by its byte length as a big-endian u64.
    pub fn identity_bytes(&self) -> Vec<u8> {
        identity_bytes(self.profile, self.version, &self.source)
    }

    pub fn references(&self, name: &str) -> bool {
        Variable::parse(name).is_some_and(|var| self.references & var.flag() != 0)
    }

    /// Evaluate trusted facts for one request, returning USD before any price
    /// multiplier. Inconsistent cache facts, negative amounts, division by zero,
    /// and arithmetic outside the Decimal range return errors.
    pub fn evaluate(&self, usage: TokenFacts) -> Result<Decimal> {
        let facts = self.normalize(usage, false)?;
        let raw = self.root.range(&facts.map(Interval::point))?.high;
        usd(raw)
    }

    /// Bound successful evaluation over all valid raw facts whose quantities
    /// are independently between zero and these limits, using the same protocol.
    /// Correlations (e.g. cache hits and total input) may widen the result.
    /// Arithmetic overflow or a reachable denominator interval containing
    /// zero causes analysis to fail.
    pub fn upper_bound(&self, upper: TokenFacts) -> Result<Decimal> {
        let facts = self.normalize(upper, true)?;
        let raw = self.root.range(&facts.map(|high| Interval {
            low: Decimal::ZERO,
            high,
        }))?;
        usd(raw.high)
    }

    fn normalize(&self, facts: TokenFacts, caps: bool) -> Result<[Decimal; 6]> {
        if (facts.input_includes_cache && facts.cache_write_1h != 0)
            || (!facts.input_includes_cache && facts.cache_write_12h != 0)
        {
            return Err(error(
                "Cache write TTL does not match the input usage protocol",
            ));
        }
        let [input, output, read, short, one_hour, twelve_hours] = [
            facts.input,
            facts.output,
            facts.cache_read,
            facts.cache_write,
            facts.cache_write_1h,
            facts.cache_write_12h,
        ]
        .map(Decimal::from);
        let length = if facts.input_includes_cache {
            if !caps && (read > input || short + twelve_hours > input) {
                return Err(error("A cache count exceeds the reported total input"));
            }
            input
        } else {
            input + read + short + one_hour
        };

        let priced_read = if self.references("cr") {
            read
        } else {
            Decimal::ZERO
        };
        let priced_1h = if self.references("cc1h") {
            one_hour
        } else {
            Decimal::ZERO
        };
        let merged_1h = one_hour - priced_1h;
        let twelve_hours = twelve_hours;
        let write = short + merged_1h * Decimal::new(16, 1) + twelve_hours * Decimal::TWO;
        let price_write = self.references("cc");

        let prompt = if facts.input_includes_cache {
            let premium = if price_write {
                Decimal::ZERO
            } else {
                twelve_hours
            };
            if caps {
                input + premium
            } else {
                let write_deduction = if price_write {
                    short + twelve_hours
                } else {
                    Decimal::ZERO
                };
                // Upstream's unadjusted write prefix may overlap cache reads.
                (input + premium - priced_read - write_deduction).max(Decimal::ZERO)
            }
        } else {
            let unpriced_write = if price_write { Decimal::ZERO } else { write };
            input + read - priced_read + unpriced_write
        };
        Ok([prompt, output, length, read, write, priced_1h])
    }
}

fn identity_bytes(profile: &str, version: u8, source: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(3 * size_of::<u64>() + profile.len() + 1 + source.len());
    for field in [profile.as_bytes(), &[version], source.as_bytes()] {
        bytes.extend_from_slice(&(field.len() as u64).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    bytes
}

fn usd(raw: Decimal) -> Result<Decimal> {
    if raw < Decimal::ZERO {
        return Err(error("Expression returned a negative charge"));
    }
    raw.checked_div(Decimal::from(USD_SCALE))
        .ok_or_else(arithmetic_error)
}

fn arithmetic_error() -> ExpressionError {
    error("Expression arithmetic exceeds the supported decimal range")
}

#[derive(Clone, Copy)]
struct Interval {
    low: Decimal,
    high: Decimal,
}

impl Interval {
    fn point(value: Decimal) -> Self {
        Self {
            low: value,
            high: value,
        }
    }

    fn truth(can_be_false: bool, can_be_true: bool) -> Self {
        Self {
            low: Decimal::from(u8::from(!can_be_false)),
            high: Decimal::from(u8::from(can_be_true)),
        }
    }

    fn union(self, other: Self) -> Self {
        Self {
            low: self.low.min(other.low),
            high: self.high.max(other.high),
        }
    }

    fn contains_zero(self) -> bool {
        self.low <= Decimal::ZERO && self.high >= Decimal::ZERO
    }

    fn corners(
        self,
        other: Self,
        operation: fn(Decimal, Decimal) -> Option<Decimal>,
    ) -> Result<Self> {
        let corner = |a, b| {
            operation(a, b)
                .map(Self::point)
                .ok_or_else(arithmetic_error)
        };
        Ok(corner(self.low, other.low)?
            .union(corner(self.low, other.high)?)
            .union(corner(self.high, other.low)?)
            .union(corner(self.high, other.high)?))
    }
}

impl Node {
    fn range(&self, facts: &[Interval; 6]) -> Result<Interval> {
        let value = match self {
            Self::Number(value) | Self::Fixed(value) => Interval::point(*value),
            Self::Boolean(value) => Interval::truth(!value, *value),
            Self::Variable(var) => facts[*var as usize],
            Self::Tier(_, value) => return value.range(facts),
            Self::Unary(op, arg) => {
                let Interval { low, high } = arg.range(facts)?;
                match op {
                    Unary::Negate => Interval {
                        low: -high,
                        high: -low,
                    },
                    Unary::Not => Interval::truth(high == Decimal::ONE, low == Decimal::ZERO),
                    Unary::Abs => Interval {
                        low: if low <= Decimal::ZERO && high >= Decimal::ZERO {
                            Decimal::ZERO
                        } else {
                            low.abs().min(high.abs())
                        },
                        high: low.abs().max(high.abs()),
                    },
                    Unary::Ceil => Interval {
                        low: low.ceil(),
                        high: high.ceil(),
                    },
                    Unary::Floor => Interval {
                        low: low.floor(),
                        high: high.floor(),
                    },
                }
            }
            Self::Binary(op, a, b) => {
                let a = a.range(facts)?;
                match op {
                    Binary::And if a.high == Decimal::ZERO => return Ok(a),
                    Binary::Or if a.low == Decimal::ONE => return Ok(a),
                    _ => {}
                }
                return op.range(a, b.range(facts)?);
            }
            Self::Select(cond, yes, no) => {
                let cond = cond.range(facts)?;
                if cond.high == Decimal::ZERO {
                    return no.range(facts);
                }
                let yes = yes.range(facts)?;
                if cond.low == Decimal::ONE {
                    return Ok(yes);
                }
                yes.union(no.range(facts)?)
            }
        };
        Ok(value)
    }
}

impl Binary {
    fn range(self, a: Interval, b: Interval) -> Result<Interval> {
        let value = match self {
            Self::Multiply => return a.corners(b, Decimal::checked_mul),
            Self::Divide => {
                if b.contains_zero() {
                    return Err(error("Expression denominator can be zero"));
                }
                return a.corners(b, Decimal::checked_div);
            }
            Self::Add => Interval {
                low: a.low.checked_add(b.low).ok_or_else(arithmetic_error)?,
                high: a.high.checked_add(b.high).ok_or_else(arithmetic_error)?,
            },
            Self::Subtract => Interval {
                low: a.low.checked_sub(b.high).ok_or_else(arithmetic_error)?,
                high: a.high.checked_sub(b.low).ok_or_else(arithmetic_error)?,
            },
            Self::Min => Interval {
                low: a.low.min(b.low),
                high: a.high.min(b.high),
            },
            Self::Max => Interval {
                low: a.low.max(b.low),
                high: a.high.max(b.high),
            },
            Self::Lt => Interval::truth(a.high >= b.low, a.low < b.high),
            Self::Le => Interval::truth(a.high > b.low, a.low <= b.high),
            Self::Gt => return Self::Lt.range(b, a),
            Self::Ge => return Self::Le.range(b, a),
            Self::Eq | Self::Ne => {
                let equal = a.low <= b.high && b.low <= a.high;
                let unequal = a.low != b.high || a.high != b.low;
                if self == Self::Eq {
                    Interval::truth(unequal, equal)
                } else {
                    Interval::truth(equal, unequal)
                }
            }
            Self::And => Interval {
                low: a.low.min(b.low),
                high: a.high.min(b.high),
            },
            Self::Or => Interval {
                low: a.low.max(b.low),
                high: a.high.max(b.high),
            },
        };
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> TokenFacts {
        TokenFacts {
            input: 1000,
            output: 100,
            cache_read: 200,
            cache_write: 100,
            cache_write_1h: 0,
            cache_write_12h: 0,
            input_includes_cache: true,
        }
    }

    fn amount(source: &str, usage: TokenFacts) -> Decimal {
        compile(source).unwrap().evaluate(usage).unwrap()
    }

    #[test]
    fn openai_token_price_uses_raw_context_and_whole_tree_cache_references() {
        assert_eq!(amount("p * 3 + c * 15", facts()), Decimal::new(45, 4));
        assert_eq!(
            amount(
                r#"v1: len <= 1000 ? tier("standard", p * 3 + c * 15 + cr * 0.3) : tier("long", cc * 4)"#,
                facts()
            ),
            Decimal::new(366, 5)
        );
        let mut long = facts();
        long.input += 1;
        assert_eq!(
            amount("len <= 1000 ? p * 3 : cc * 4", long),
            Decimal::new(4, 4)
        );
    }

    #[test]
    fn cache_ttl_uses_pinned_fallback_multipliers() {
        let mut claude = facts();
        claude.input_includes_cache = false;
        claude.input = 650;
        claude.cache_write_1h = 50;
        assert_eq!(amount("len", claude), Decimal::new(1000, 6));
        assert_eq!(amount("p + cc * 2", claude), Decimal::new(1210, 6));
        assert_eq!(amount("p", claude), Decimal::new(1030, 6));
        assert_eq!(
            amount("p + cc * 2 + cc1h * 3", claude),
            Decimal::new(1200, 6)
        );

        let mut openai = facts();
        openai.cache_write_12h = 50;
        assert_eq!(amount("len", openai), Decimal::new(1000, 6));
        assert_eq!(amount("p + cc * 2", openai), Decimal::new(1250, 6));
        assert_eq!(amount("p", openai), Decimal::new(1050, 6));

        openai.cache_read = 950;
        assert_eq!(amount("p + cr * 0 + cc * 0", openai), Decimal::ZERO);
        openai.cache_read = 1001;
        assert!(compile("p").unwrap().evaluate(openai).is_err());
    }

    #[test]
    fn identity_encodes_profile_version_and_exact_source() {
        let source = " v1:tier(\"档\", p)\n";
        let compiled = compile(source).unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&23_u64.to_be_bytes());
        expected.extend_from_slice(b"conduit-newapi-token-v1");
        expected.extend_from_slice(&1_u64.to_be_bytes());
        expected.push(1);
        expected.extend_from_slice(&(source.len() as u64).to_be_bytes());
        expected.extend_from_slice(source.as_bytes());
        let identity = compiled.identity_bytes();
        assert_eq!(identity, expected);
        assert_ne!(identity, identity_bytes("other", 1, source));
        assert_ne!(identity, identity_bytes(PROFILE, 2, source));
        assert_ne!(identity, compile(source.trim()).unwrap().identity_bytes());
        assert_ne!(
            compile("v1:p").unwrap().identity_bytes(),
            compile("p").unwrap().identity_bytes()
        );
    }

    #[test]
    fn cc12h_is_rejected_even_in_an_unreachable_branch() {
        for source in ["cc12h", "v1: false ? cc12h : p"] {
            let err = compile(source).unwrap_err();
            assert_eq!(err.code(), Some(UNSUPPORTED_CC12H_UNTRUSTED_USAGE));
            assert!(err.to_string().contains(UNSUPPORTED_CC12H_UNTRUSTED_USAGE));
        }
        assert!(compile(r#"tier("cc12h", p)"#).is_ok());
    }

    #[test]
    fn fixed_is_pre_multiplier_usd_and_bad_branches_are_lazy() {
        let program = compile(
            r#"len <= 1000 || 1 / 0 > 0 ? tier("request", fixed(0.01)) : tier("tokens", p / 0)"#,
        )
        .unwrap();
        let pre_multiplier = program.evaluate(facts()).unwrap();
        assert_eq!(pre_multiplier, Decimal::new(1, 2));
        assert_eq!(pre_multiplier * Decimal::new(15, 1), Decimal::new(15, 3));
        assert_eq!(program.upper_bound(facts()).unwrap(), pre_multiplier);
        assert!(compile(r#"false ? tier("bad", fixed(0.01) * p) : tier("ok", p)"#).is_err());
    }

    #[test]
    fn unsupported_input_and_invalid_arithmetic_are_diagnostic() {
        for source in [
            "v2:p",
            "img_cr * 2",
            "param(\"x\")",
            "hour(\"UTC\")",
            "u(\"seconds\")",
            "p + false",
            "len ? 1 : 0",
            "p > 0",
            "p ||| when(true) * 2",
            r#"tier("invalid", fixed(-0.01))"#,
            "fixed(0.01)",
        ] {
            assert!(compile(source).is_err(), "{source}");
        }
        for source in ["p / 0", "-p", "p * 79228162514264337593543950335"] {
            assert!(
                compile(source).unwrap().evaluate(facts()).is_err(),
                "{source}"
            );
        }
        assert!(compile(&format!("{}1{}", "(".repeat(40), ")".repeat(40))).is_err());
    }

    #[test]
    fn reservation_bounds_all_valid_component_values() {
        let expression = compile(r#"len <= 10 ? tier("short", abs(p - cr) * 3 + ceil(c / 2)) : tier("long", max(2, p) * floor(2.4) + cc * 4)"#).unwrap();
        let mut cap = TokenFacts {
            input: 20,
            output: 7,
            cache_read: 6,
            cache_write: 5,
            cache_write_12h: 3,
            ..facts()
        };
        for includes_cache in [true, false] {
            cap.input_includes_cache = includes_cache;
            cap.cache_write_1h = if includes_cache { 0 } else { 3 };
            cap.cache_write_12h = if includes_cache { 3 } else { 0 };
            let bound = expression.upper_bound(cap).unwrap();
            for input in 0..=cap.input {
                for cache_read in 0..=cap.cache_read.min(input) {
                    for output in 0..=cap.output {
                        let usage = TokenFacts {
                            input,
                            cache_read,
                            output,
                            cache_write: cap.cache_write.min(input / 2),
                            cache_write_12h: cap.cache_write_12h.min(input / 2),
                            ..cap
                        };
                        assert!(expression.evaluate(usage).unwrap() <= bound, "{usage:?}");
                    }
                }
            }
        }
        assert!(compile("1 / (p - 1)").unwrap().upper_bound(cap).is_err());
        assert!(
            compile("p > 0 ? 1 / p : 0")
                .unwrap()
                .upper_bound(cap)
                .is_err()
        );
    }
}
