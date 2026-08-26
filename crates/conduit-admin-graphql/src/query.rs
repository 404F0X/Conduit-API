//! S05/S06 — pure parsers for admin GraphQL `WhereInput` / `Order` filters.
//!
//! The Go side generates per-type `*WhereInput` and `*Order` gqlgen inputs via
//! ent (see `internal/ent/gql_where_input.go` and `gql_pagination.go`); the
//! shapes are fixed by the snapshot at
//! `tests/contracts/admin_graphql_schema.graphql` (e.g. `APIKeyWhereInput`,
//! `APIKeyOrder`, `OrderDirection`). The pure helpers below translate an
//! `async_graphql::Value` (or any GraphQL-coerced JSON-like value) into a
//! typed predicate tree / ordering, without depending on a concrete entity
//! type — resolvers later lower these into repository/ent predicates. This
//! keeps the surface unit-testable and matches the Go contract.
//!
//! Mirrors:
//! - ent predicate suffixes: `eq` (bare field), `NEQ`, `In`, `NotIn`, `GT`,
//!   `GTE`, `LT`, `LTE`, `Contains`, `HasPrefix`, `HasSuffix`, `EqualFold`,
//!   `ContainsFold`, `IsNil`, `NotNil` (snapshot lines 1626+).
//! - boolean composition: `not: <WhereInput>`, `and: [<WhereInput>!]`,
//!   `or: [<WhereInput>!]`.
//! - edge predicates: `has<Edge>: Boolean`, `has<Edge>With: [<WhereInput>!]`.
//! - `enum OrderDirection { ASC DESC }` (snapshot line 4072).
//! - `input <Type>Order { direction: OrderDirection! = ASC, field: <Type>OrderField! }`
//!   (snapshot line 1434) where the field enum value is a SCREAMING_SNAKE
//!   case string (`CREATED_AT`, `UPDATED_AT`, `NAME`, …).

use async_graphql::Value;

// =====================================================================
// S05 — WhereInput predicate tree.
// =====================================================================

/// One per-field predicate. The `field` is the GraphQL field name (e.g.
/// `createdAt`, `userID`); the `op` selects the ent suffix semantics; `value`
/// is the raw scalar/array argument carried verbatim from the GraphQL value.
///
/// For `IsNil`/`NotNil`/`HasEdge` the `value` is a `Value::Boolean`.
/// For `HasEdgeWith` the `value` is a `Value::List` of nested WhereInputs.
#[derive(Debug, Clone, PartialEq)]
pub struct Predicate {
    pub field: String,
    pub op: PredicateOp,
    pub value: Value,
}

/// ent predicate operator grammar — the suffix set every `*WhereInput` field
/// exposes in the snapshot. Order matters only for stable matching of the
/// bare-field `Eq` case (no suffix).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredicateOp {
    /// Bare field name (`id`, `name`) → equality.
    Eq,
    /// `<field>NEQ`
    NotEqual,
    /// `<field>In` (value: list)
    In,
    /// `<field>NotIn` (value: list)
    NotIn,
    /// `<field>GT`
    GreaterThan,
    /// `<field>GTE`
    GreaterThanOrEqual,
    /// `<field>LT`
    LessThan,
    /// `<field>LTE`
    LessThanOrEqual,
    /// `<field>Contains` (substring)
    Contains,
    /// `<field>HasPrefix`
    HasPrefix,
    /// `<field>HasSuffix`
    HasSuffix,
    /// `<field>EqualFold` (case-insensitive equality)
    EqualFold,
    /// `<field>ContainsFold` (case-insensitive substring)
    ContainsFold,
    /// `<field>IsNil` (value: Boolean)
    IsNil,
    /// `<field>NotNil` (value: Boolean)
    NotNil,
    /// `has<Edge>` (value: Boolean) — edge existence.
    HasEdge,
    /// `has<Edge>With` (value: list of nested WhereInputs) — edge filter.
    HasEdgeWith,
}

impl PredicateOp {
    /// Returns the canonical GraphQL suffix (without the leading field name)
    /// for this op, or the empty string for the bare `Eq` op.
    pub fn suffix(self) -> &'static str {
        match self {
            PredicateOp::Eq => "",
            PredicateOp::NotEqual => "NEQ",
            PredicateOp::In => "In",
            PredicateOp::NotIn => "NotIn",
            PredicateOp::GreaterThan => "GT",
            PredicateOp::GreaterThanOrEqual => "GTE",
            PredicateOp::LessThan => "LT",
            PredicateOp::LessThanOrEqual => "LTE",
            PredicateOp::Contains => "Contains",
            PredicateOp::HasPrefix => "HasPrefix",
            PredicateOp::HasSuffix => "HasSuffix",
            PredicateOp::EqualFold => "EqualFold",
            PredicateOp::ContainsFold => "ContainsFold",
            PredicateOp::IsNil => "IsNil",
            PredicateOp::NotNil => "NotNil",
            PredicateOp::HasEdge => "",
            PredicateOp::HasEdgeWith => "With",
        }
    }
}

/// A typed WhereInput predicate tree — the Rust analogue of an ent
/// `<Type>WhereInput` GraphQL value. Boolean combinators mirror the `not` /
/// `and` / `or` keys every generated WhereInput exposes (snapshot line 1627).
#[derive(Debug, Clone, PartialEq)]
pub enum Where {
    /// Empty match-all (e.g. an empty input object). Resolves to the ent
    /// `Worker` no-op predicate.
    Empty,
    Leaf(Predicate),
    Not(Box<Where>),
    And(Vec<Where>),
    Or(Vec<Where>),
}

/// Errors raised while parsing a `WhereInput` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhereParseError {
    /// The top-level (or `not`/`and`/`or` element) value was not an Object.
    NotAnObject,
    /// A nested `not`/`and[i]`/`or[i]` failed to parse.
    Nested(String),
    /// A boolean combinator carried the wrong value kind.
    /// (e.g. `and` was given a non-list).
    InvalidCombinator { combinator: &'static str },
}

/// Parse a `WhereInput` GraphQL value into a typed [`Where`] tree.
///
/// Mirrors ent's per-type `WhereInput` semantics (snapshot lines 1524+, 1626+):
/// the three boolean combinators `not` / `and` / `or` are reserved and lowered
/// to [`Where::Not`] / [`Where::And`] / [`Where::Or`]; every other key is a
/// field predicate whose operator is determined by the trailing suffix. An
/// empty object yields [`Where::Empty`] (matches Go ent's empty-filter
/// behaviour). Multiple predicates at one level are implicitly AND-ed.
///
/// This is a **pure** function — no I/O, no schema lookup. The field name and
/// operator are captured as data; downstream code checks them against the
/// concrete entity's allowed fields (the snapshot's `<Type>WhereInput`
/// declarations are the authoritative allow-list per type).
pub fn parse_where(input: &Value) -> Result<Where, WhereParseError> {
    let object = match input {
        Value::Object(map) => map,
        Value::Null => return Ok(Where::Empty),
        _ => return Err(WhereParseError::NotAnObject),
    };

    let mut leaves: Vec<Where> = Vec::new();

    for (name, value) in object.iter() {
        let key = name.as_str();
        match key {
            "not" => {
                let inner = parse_where(value)
                    .map_err(|e| WhereParseError::Nested(format!("not: {e:?}")))?;
                leaves.push(Where::Not(Box::new(inner)));
            }
            "and" => {
                let items = collect_where_list(value, "and")?;
                leaves.push(Where::And(items));
            }
            "or" => {
                let items = collect_where_list(value, "or")?;
                leaves.push(Where::Or(items));
            }
            other => {
                let predicate = parse_predicate(other, value.clone())?;
                leaves.push(Where::Leaf(predicate));
            }
        }
    }

    Ok(reduce_and(leaves))
}

fn collect_where_list(
    value: &Value,
    combinator: &'static str,
) -> Result<Vec<Where>, WhereParseError> {
    let list = match value {
        Value::List(list) => list,
        Value::Null => return Ok(Vec::new()),
        _ => {
            return Err(WhereParseError::InvalidCombinator { combinator });
        }
    };

    let mut out = Vec::with_capacity(list.len());
    for (index, element) in list.iter().enumerate() {
        let parsed = parse_where(element)
            .map_err(|e| WhereParseError::Nested(format!("{combinator}[{index}]: {e:?}")))?;
        out.push(parsed);
    }
    Ok(out)
}

/// Reduce a list of conjuncts into the simplest equivalent tree:
/// zero items → `Empty`, one item → that item, many → `And`.
fn reduce_and(mut leaves: Vec<Where>) -> Where {
    match leaves.len() {
        0 => Where::Empty,
        1 => leaves.remove(0),
        _ => Where::And(leaves),
    }
}

/// Split a GraphQL input key into `(field, op)` according to the ent predicate
/// suffix grammar. The longest matching suffix wins so that `keyContainsFold`
/// is not mis-parsed as `keyContains`. The `has<Edge>` / `has<Edge>With`
/// edge-predicate keys are special-cased before suffix matching.
///
/// Returns `None` for keys that do not match any known suffix (the snapshot
/// shows every non-combinator key always ends in one of these suffixes).
fn parse_predicate(key: &str, value: Value) -> Result<Predicate, WhereParseError> {
    // Order longest-suffix-first so `ContainsFold` wins over `Contains` and
    // `EqualFold` is preferred over the bare `Eq` form.
    const SUFFIXES: &[(PredicateOp, &str)] = &[
        (PredicateOp::ContainsFold, "ContainsFold"),
        (PredicateOp::EqualFold, "EqualFold"),
        (PredicateOp::HasPrefix, "HasPrefix"),
        (PredicateOp::HasSuffix, "HasSuffix"),
        (PredicateOp::NotIn, "NotIn"),
        (PredicateOp::NotNil, "NotNil"),
        (PredicateOp::Contains, "Contains"),
        (PredicateOp::NotEqual, "NEQ"),
        (PredicateOp::GreaterThanOrEqual, "GTE"),
        (PredicateOp::LessThanOrEqual, "LTE"),
        (PredicateOp::GreaterThan, "GT"),
        (PredicateOp::LessThan, "LT"),
        (PredicateOp::In, "In"),
        (PredicateOp::IsNil, "IsNil"),
    ];

    // Edge existence: `has<Edge>` carries a Boolean; `has<Edge>With` carries
    // a list of nested WhereInputs. These are emitted by ent for every edge.
    if let Some(rest) = key.strip_prefix("has") {
        if let Some(edge) = rest.strip_suffix("With")
            && !edge.is_empty()
        {
            return Ok(Predicate {
                field: edge.to_owned(),
                op: PredicateOp::HasEdgeWith,
                value,
            });
        }
        if !rest.is_empty() {
            return Ok(Predicate {
                field: rest.to_owned(),
                op: PredicateOp::HasEdge,
                value,
            });
        }
    }

    for (op, suffix) in SUFFIXES {
        if let Some(field) = key.strip_suffix(suffix)
            && !field.is_empty()
        {
            return Ok(Predicate {
                field: field.to_owned(),
                op: *op,
                value,
            });
        }
    }

    // Bare field name → equality. The snapshot always exposes this form for
    // every typed field.
    Ok(Predicate {
        field: key.to_owned(),
        op: PredicateOp::Eq,
        value,
    })
}

// =====================================================================
// S06 — Order parse.
// =====================================================================

/// Mirrors `enum OrderDirection { ASC DESC }` (snapshot line 4072). The Go
/// `entgql.OrderDirection` type validates to exactly these two values; gqlgen
/// rejects any other string at the input-coercion layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderDirection {
    Asc,
    Desc,
}

impl OrderDirection {
    /// GraphQL enum value name as declared in the snapshot.
    pub fn as_graphql(self) -> &'static str {
        match self {
            OrderDirection::Asc => "ASC",
            OrderDirection::Desc => "DESC",
        }
    }

    /// Parse from the GraphQL enum value name. Returns `None` for any value
    /// other than `ASC`/`DESC` so callers can surface a gqlgen-style
    /// validation error.
    pub fn from_graphql(value: &str) -> Option<Self> {
        match value {
            "ASC" => Some(OrderDirection::Asc),
            "DESC" => Some(OrderDirection::Desc),
            _ => None,
        }
    }
}

/// Mirrors `input <Type>Order { direction: OrderDirection! = ASC, field: <Type>OrderField! }`
/// (snapshot line 1434). `field` carries the SCREAMING_SNAKE field-enum value
/// (e.g. `CREATED_AT`, `NAME`); validation against a concrete type's allowed
/// enum values is the resolver's job, matching the per-type `OrderField`
/// declarations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub direction: OrderDirection,
    pub field: String,
}

impl Order {
    /// Default used when a caller omits `orderBy` entirely — matches Go ent's
    /// `Default<Type>Order` (ASC by ID, snapshot line 412 for default
    /// `direction: OrderDirection! = ASC`). The field default is type-specific
    /// so callers must pass it in.
    pub fn default_for(field: impl Into<String>) -> Self {
        Self {
            direction: OrderDirection::Asc,
            field: field.into(),
        }
    }
}

/// Errors raised while parsing an `Order` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderParseError {
    /// The value was not an Object.
    NotAnObject,
    /// Required `field` was missing or not a string.
    MissingOrInvalidField,
    /// `direction` was present but not `ASC`/`DESC`.
    InvalidDirection,
}

/// Parse a `<Type>Order` GraphQL value into an [`Order`].
///
/// Mirrors gqlgen's parsing of `input APIKeyOrder { direction: OrderDirection! = ASC, field: APIKeyOrderField! }`
/// (snapshot line 1434): `field` is required and must be a non-empty string;
/// `direction` defaults to `ASC` when absent; an unknown direction is an error
/// because gqlgen rejects unknown enum values at the input layer.
pub fn parse_order(input: &Value) -> Result<Order, OrderParseError> {
    let object = match input {
        Value::Object(map) => map,
        Value::Null => return Err(OrderParseError::NotAnObject),
        _ => return Err(OrderParseError::NotAnObject),
    };

    let field = object
        .iter()
        .find(|(name, _)| name.as_str() == "field")
        .and_then(|(_, value)| match value {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        })
        .ok_or(OrderParseError::MissingOrInvalidField)?;

    let direction = match object
        .iter()
        .find(|(name, _)| name.as_str() == "direction")
        .map(|(_, value)| value)
    {
        None => OrderDirection::Asc,
        Some(Value::String(s)) => {
            OrderDirection::from_graphql(s).ok_or(OrderParseError::InvalidDirection)?
        }
        // gqlgen coerces enum values to strings; a non-string direction is
        // treated as invalid just like an unknown enum name.
        Some(_) => return Err(OrderParseError::InvalidDirection),
    };

    Ok(Order { direction, field })
}

// =====================================================================
// Tests — assert Where/Order match snapshot semantics. The snapshot at
// tests/contracts/admin_graphql_schema.graphql is the contract; these tests
// pin the suffix grammar, boolean combinators, edge predicates, and the
// OrderDirection enum to the snapshot-declared shapes.
// =====================================================================

#[cfg(test)]
mod tests {
    use super::{
        Order, OrderDirection, OrderParseError, PredicateOp, Where, WhereParseError, parse_order,
        parse_where,
    };
    use async_graphql::{Name, Value};

    fn obj(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
        Value::Object(
            pairs
                .into_iter()
                .map(|(field, value)| (Name::new(field), value))
                .collect(),
        )
    }

    fn str(s: impl Into<String>) -> Value {
        Value::String(s.into())
    }

    fn list(values: impl IntoIterator<Item = Value>) -> Value {
        Value::List(values.into_iter().collect())
    }

    // ----- S05 WhereInput parse: field predicates --------------------

    #[test]
    fn parse_where_bare_field_is_equality() -> Result<(), WhereParseError> {
        let input = obj([("name", str("alice"))]);
        let parsed = parse_where(&input)?;

        match parsed {
            Where::Leaf(predicate) => {
                assert_eq!(predicate.field, "name");
                assert_eq!(predicate.op, PredicateOp::Eq);
                assert_eq!(predicate.value, str("alice"));
            }
            other => panic!("expected Leaf, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn parse_where_recognises_every_ent_suffix() -> Result<(), WhereParseError> {
        // Mirrors the suffix set declared on `APIKeyWhereInput` (snapshot
        // lines 1626+): NEQ/In/NotIn/GT/GTE/LT/LTE/Contains/HasPrefix/
        // HasSuffix/EqualFold/ContainsFold plus bare equality.
        let cases = &[
            ("nameNEQ", PredicateOp::NotEqual),
            ("nameIn", PredicateOp::In),
            ("nameNotIn", PredicateOp::NotIn),
            ("nameGT", PredicateOp::GreaterThan),
            ("nameGTE", PredicateOp::GreaterThanOrEqual),
            ("nameLT", PredicateOp::LessThan),
            ("nameLTE", PredicateOp::LessThanOrEqual),
            ("nameContains", PredicateOp::Contains),
            ("nameHasPrefix", PredicateOp::HasPrefix),
            ("nameHasSuffix", PredicateOp::HasSuffix),
            ("nameEqualFold", PredicateOp::EqualFold),
            ("nameContainsFold", PredicateOp::ContainsFold),
        ];

        for (key, expected_op) in cases {
            let input = obj([(*key, str("x"))]);
            let parsed = parse_where(&input)?;

            match parsed {
                Where::Leaf(predicate) => {
                    assert_eq!(
                        predicate.field, "name",
                        "field for {key} should strip the suffix"
                    );
                    assert_eq!(predicate.op, *expected_op, "op for {key}");
                }
                other => panic!("expected Leaf for {key}, got {other:?}"),
            }
        }
        Ok(())
    }

    #[test]
    fn parse_where_contains_fold_does_not_degrade_to_contains() -> Result<(), WhereParseError> {
        // Longest-suffix-first must win: `nameContainsFold` should yield
        // `ContainsFold`, not `Contains` with field `nameF`.
        let input = obj([("nameContainsFold", str("alic"))]);
        match parse_where(&input)? {
            Where::Leaf(predicate) => {
                assert_eq!(predicate.field, "name");
                assert_eq!(predicate.op, PredicateOp::ContainsFold);
            }
            other => panic!("got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn parse_where_is_nil_keeps_boolean_value() -> Result<(), WhereParseError> {
        let input = obj([("userIDIsNil", Value::Boolean(true))]);
        match parse_where(&input)? {
            Where::Leaf(predicate) => {
                assert_eq!(predicate.field, "userID");
                assert_eq!(predicate.op, PredicateOp::IsNil);
                assert_eq!(predicate.value, Value::Boolean(true));
            }
            other => panic!("got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn parse_where_in_carries_list_value_verbatim() -> Result<(), WhereParseError> {
        let input = obj([("idIn", list([str("a"), str("b")]))]);
        match parse_where(&input)? {
            Where::Leaf(predicate) => {
                assert_eq!(predicate.field, "id");
                assert_eq!(predicate.op, PredicateOp::In);
                match predicate.value {
                    Value::List(items) => assert_eq!(items.len(), 2),
                    other => panic!("expected List, got {other:?}"),
                }
            }
            other => panic!("got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn parse_where_has_edge_predicate() -> Result<(), WhereParseError> {
        // `hasUser: Boolean` from APIKeyWhereInput (snapshot line 1716).
        let input = obj([("hasUser", Value::Boolean(true))]);
        match parse_where(&input)? {
            Where::Leaf(predicate) => {
                assert_eq!(predicate.field, "User");
                assert_eq!(predicate.op, PredicateOp::HasEdge);
            }
            other => panic!("got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn parse_where_has_edge_with_predicate() -> Result<(), WhereParseError> {
        // `hasUserWith: [UserWhereInput!]` from APIKeyWhereInput
        // (snapshot line 1718).
        let nested = obj([("username", str("alice"))]);
        let input = obj([("hasUserWith", list([nested]))]);
        match parse_where(&input)? {
            Where::Leaf(predicate) => {
                assert_eq!(predicate.field, "User");
                assert_eq!(predicate.op, PredicateOp::HasEdgeWith);
                match predicate.value {
                    Value::List(items) => assert_eq!(items.len(), 1),
                    other => panic!("expected List, got {other:?}"),
                }
            }
            other => panic!("got {other:?}"),
        }
        Ok(())
    }

    // ----- S05 WhereInput parse: boolean combinators ----------------

    #[test]
    fn parse_where_not_combinator() -> Result<(), WhereParseError> {
        let inner = obj([("name", str("alice"))]);
        let input = obj([("not", inner)]);
        match parse_where(&input)? {
            Where::Not(boxed) => match *boxed {
                Where::Leaf(predicate) => {
                    assert_eq!(predicate.field, "name");
                    assert_eq!(predicate.op, PredicateOp::Eq);
                }
                other => panic!("inner of Not should be Leaf, got {other:?}"),
            },
            other => panic!("got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn parse_where_and_combinator() -> Result<(), WhereParseError> {
        let a = obj([("name", str("alice"))]);
        let b = obj([("status", str("active"))]);
        let input = obj([("and", list([a, b]))]);
        match parse_where(&input)? {
            Where::And(items) => assert_eq!(items.len(), 2),
            other => panic!("got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn parse_where_or_combinator() -> Result<(), WhereParseError> {
        let a = obj([("name", str("alice"))]);
        let input = obj([("or", list([a]))]);
        match parse_where(&input)? {
            Where::Or(items) => assert_eq!(items.len(), 1),
            other => panic!("got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn parse_where_multiple_leaves_are_anded() -> Result<(), WhereParseError> {
        let input = obj([
            ("nameContains", str("ali")),
            ("status", str("active")),
            ("createdAtGTE", str("2024-01-01T00:00:00Z")),
        ]);
        match parse_where(&input)? {
            Where::And(items) => assert_eq!(items.len(), 3),
            other => panic!("multiple leaves should AND, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn parse_where_empty_object_is_empty_match_all() -> Result<(), WhereParseError> {
        let input = obj([]);
        assert_eq!(parse_where(&input)?, Where::Empty);
        Ok(())
    }

    #[test]
    fn parse_where_null_is_empty_match_all() -> Result<(), WhereParseError> {
        assert_eq!(parse_where(&Value::Null)?, Where::Empty);
        Ok(())
    }

    #[test]
    fn parse_where_non_object_is_error() {
        assert_eq!(parse_where(&str("oops")), Err(WhereParseError::NotAnObject));
    }

    #[test]
    fn parse_where_and_with_non_list_value_errors() {
        let input = obj([("and", str("nope"))]);
        assert_eq!(
            parse_where(&input),
            Err(WhereParseError::InvalidCombinator { combinator: "and" })
        );
    }

    #[test]
    fn parse_where_nested_not_non_object_errors() {
        let input = obj([("not", str("nope"))]);
        match parse_where(&input) {
            Err(WhereParseError::Nested(_)) => {}
            other => panic!("expected Nested error, got {other:?}"),
        }
    }

    #[test]
    fn parse_where_all_caps_acronym_field_preserved() -> Result<(), WhereParseError> {
        // `userIDIn` / `projectIDIn` — Go json camelCase keeps the trailing
        // acronym capitalised, and the WhereInput snapshot field names
        // inherit that (snapshot lines 1660-1665). The parser must not mangle
        // the field name and must still detect the `In` suffix.
        let input = obj([("userIDIn", list([str("u1"), str("u2")]))]);
        match parse_where(&input)? {
            Where::Leaf(predicate) => {
                assert_eq!(predicate.field, "userID");
                assert_eq!(predicate.op, PredicateOp::In);
            }
            other => panic!("got {other:?}"),
        }
        Ok(())
    }

    // ----- S06 Order parse ------------------------------------------

    #[test]
    fn parse_order_asc_with_field() -> Result<(), OrderParseError> {
        // Mirrors `input APIKeyOrder { direction: OrderDirection! = ASC, field: ... }`
        // (snapshot line 1434).
        let input = obj([("direction", str("ASC")), ("field", str("CREATED_AT"))]);
        let parsed = parse_order(&input)?;

        assert_eq!(
            parsed,
            Order {
                direction: OrderDirection::Asc,
                field: "CREATED_AT".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn parse_order_desc_direction() -> Result<(), OrderParseError> {
        let input = obj([("direction", str("DESC")), ("field", str("NAME"))]);
        let parsed = parse_order(&input)?;

        assert_eq!(parsed.direction, OrderDirection::Desc);
        assert_eq!(parsed.field, "NAME");
        Ok(())
    }

    #[test]
    fn parse_order_direction_defaults_to_asc_when_absent() -> Result<(), OrderParseError> {
        // Snapshot: `direction: OrderDirection! = ASC` (snapshot line 1438).
        let input = obj([("field", str("CREATED_AT"))]);
        let parsed = parse_order(&input)?;

        assert_eq!(parsed.direction, OrderDirection::Asc);
        Ok(())
    }

    #[test]
    fn parse_order_missing_field_errors() {
        let input = obj([("direction", str("ASC"))]);
        assert_eq!(
            parse_order(&input),
            Err(OrderParseError::MissingOrInvalidField)
        );
    }

    #[test]
    fn parse_order_empty_field_errors() {
        let input = obj([("direction", str("ASC")), ("field", str(""))]);
        assert_eq!(
            parse_order(&input),
            Err(OrderParseError::MissingOrInvalidField)
        );
    }

    #[test]
    fn parse_order_invalid_direction_errors() {
        // gqlgen rejects unknown enum values at the input-coercion layer;
        // the parser surfaces the same error.
        let input = obj([("direction", str("SIDEWAYS")), ("field", str("CREATED_AT"))]);
        assert_eq!(parse_order(&input), Err(OrderParseError::InvalidDirection));
    }

    #[test]
    fn parse_order_non_object_errors() {
        assert_eq!(parse_order(&Value::Null), Err(OrderParseError::NotAnObject));
        assert_eq!(parse_order(&str("x")), Err(OrderParseError::NotAnObject));
    }

    #[test]
    fn order_direction_round_trips_through_graphql_names() {
        assert_eq!(OrderDirection::Asc.as_graphql(), "ASC");
        assert_eq!(OrderDirection::Desc.as_graphql(), "DESC");
        assert_eq!(
            OrderDirection::from_graphql("ASC"),
            Some(OrderDirection::Asc)
        );
        assert_eq!(
            OrderDirection::from_graphql("DESC"),
            Some(OrderDirection::Desc)
        );
        assert_eq!(OrderDirection::from_graphql("GREEN"), None);
    }
}
