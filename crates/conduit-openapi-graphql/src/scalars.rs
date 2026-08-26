//! Wire-level scalar shims for the OpenAPI GraphQL surface.
//!
//! Mirrors the three custom scalars declared by the frozen snapshot
//! `tests/contracts/openapi_graphql_schema.graphql` (`Decimal`, `DecimalInput`,
//! `Time`) plus the `ID` wire format. In Go the bindings live in
//! `internal/server/gql/openapi/gqlgen.yml`:
//!
//! * `ID`      → `internal/objects.GUID` — `"gid://conduit/<Type>/<id>"`.
//! * `Decimal` / `DecimalInput` → `internal/objects.Decimal`
//!   (`objects/decimal.go`: marshals as a RAW JSON number token, unmarshals
//!   from string / number).
//! * `Time`    → gqlgen's built-in `graphql.Time` (RFC3339 string).
//!
//! On the Rust side `ID` stays `async_graphql::ID` (so the SDL renders `ID`
//! verbatim) and the `gid://` string is parsed by [`OpenApiGuid::parse`] inside
//! the resolvers — the same position where Go's `UnmarshalGQL` + `guidID`
//! type-check run before any lookup.

use std::fmt;

use async_graphql::{InputValueResult, Scalar, ScalarType, Value};
use chrono::{DateTime, SecondsFormat, Utc};
use conduit_core::{ConduitError, ErrorKind};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

// gqlgen `Time` scalar shim. Wire format: RFC3339 string (Go marshals with
// `time.RFC3339Nano`, parses with `time.RFC3339Nano`). No doc-comment on the
// struct: the snapshot declares the scalar without a description and the SDL
// contract test compares descriptions too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GqlTime(pub DateTime<Utc>);

#[Scalar(name = "Time")]
impl ScalarType for GqlTime {
    fn parse(value: Value) -> InputValueResult<Self> {
        match value {
            Value::String(s) => {
                // Mirrors gqlgen `graphql.UnmarshalTime` (RFC3339 parse).
                let dt = DateTime::parse_from_rfc3339(&s)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|err| {
                        async_graphql::InputValueError::custom(format!(
                            "invalid Time scalar: {err}"
                        ))
                    })?;
                Ok(GqlTime(dt))
            }
            other => Err(async_graphql::InputValueError::custom(format!(
                "Time scalar expects a string, got {other}"
            ))),
        }
    }

    fn to_value(&self) -> Value {
        // Go emits `time.RFC3339Nano` (UTC → trailing `Z`, fractional digits
        // trimmed). chrono's `AutoSi` trims in groups of three, which is the
        // closest stable equivalent; the contract only requires an RFC3339
        // string.
        Value::String(self.0.to_rfc3339_opts(SecondsFormat::AutoSi, true))
    }
}

// ---------------------------------------------------------------------------
// Decimal / DecimalInput
// ---------------------------------------------------------------------------

// Shared unmarshal mirroring Go `objects.UnmarshalDecimal`
// (`objects/decimal.go:19-32`): accepts string, JSON number (int/float);
// everything else fails with the Go error shape.
fn parse_decimal_value(value: Value) -> Result<Decimal, String> {
    match value {
        Value::String(s) => s
            .parse::<Decimal>()
            .map_err(|err| format!("failed to decode decimal: {err}")),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(Decimal::from(i))
            } else if let Some(u) = n.as_u64() {
                Ok(Decimal::from(u))
            } else {
                n.as_f64()
                    .and_then(Decimal::from_f64)
                    .ok_or_else(|| format!("failed to decode decimal: {n}"))
            }
        }
        other => Err(format!("failed to decode decimal: {other}")),
    }
}

// Shared marshal mirroring Go `objects.MarshalDecimal`
// (`objects/decimal.go:13-17`): Go writes `d.String()` as a RAW (unquoted)
// JSON number token — the e2e contract asserts `totalCost` arrives as `2`, not
// `"2"`. shopspring's `String()` trims trailing zeros, hence `normalize()`.
//
// Precision caveat: Go emits the exact decimal token; `serde_json::Number`
// (without `arbitrary_precision`) holds i64/u64/f64, so values beyond ~15
// significant fractional digits round. Quota costs are far below that bound.
fn decimal_to_value(d: Decimal) -> Value {
    let d = d.normalize();
    if d.scale() == 0
        && let Some(i) = d.to_i64()
    {
        return Value::Number(i.into());
    }
    match d.to_f64().and_then(async_graphql::Number::from_f64) {
        Some(n) => Value::Number(n),
        // Unreachable for finite decimals; fall back to the exact string
        // rather than lose the value entirely.
        None => Value::String(d.to_string()),
    }
}

// `Decimal` output scalar (snapshot: `scalar Decimal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GqlDecimal(pub Decimal);

#[Scalar(name = "Decimal")]
impl ScalarType for GqlDecimal {
    fn parse(value: Value) -> InputValueResult<Self> {
        parse_decimal_value(value)
            .map(GqlDecimal)
            .map_err(async_graphql::InputValueError::custom)
    }

    fn to_value(&self) -> Value {
        decimal_to_value(self.0)
    }
}

// `DecimalInput` input scalar (snapshot: `scalar DecimalInput`). Go binds both
// scalar names to the same `objects.Decimal`; the split names exist so inputs
// and outputs can evolve independently in the SDL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GqlDecimalInput(pub Decimal);

#[Scalar(name = "DecimalInput")]
impl ScalarType for GqlDecimalInput {
    fn parse(value: Value) -> InputValueResult<Self> {
        parse_decimal_value(value)
            .map(GqlDecimalInput)
            .map_err(async_graphql::InputValueError::custom)
    }

    fn to_value(&self) -> Value {
        decimal_to_value(self.0)
    }
}

// ---------------------------------------------------------------------------
// GUID (`ID` wire format)
// ---------------------------------------------------------------------------

/// Prefix of every Conduit API GUID, mirrored from Go `objects.GUID.MarshalGQL`
/// (`internal/objects/GUID.go:19`).
pub const GUID_PREFIX: &str = "gid://conduit/";

/// A parsed `gid://conduit/<Type>/<id>` carried by an OpenAPI resolver input.
///
/// Mirrors Go `objects.GUID` (`internal/objects/GUID.go:13-16`) as it reaches
/// the resolver layer: only the type tag and numeric id are material to the
/// OpenAPI guard logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenApiGuid {
    /// ent type tag, e.g. `"APIKey"` / `"APIKeyProfileTemplate"`.
    pub type_tag: String,
    /// Numeric row id (Go `int` → Rust `i64`).
    pub id: i64,
}

impl OpenApiGuid {
    pub fn new(type_tag: impl Into<String>, id: i64) -> Self {
        Self {
            type_tag: type_tag.into(),
            id,
        }
    }

    /// Parse a GUID wire string, mirroring Go `objects.GUID.UnmarshalGQL`
    /// (`GUID.go:22-54`) error-for-error:
    /// empty → "guid is empty"; wrong prefix → "guid must start with
    /// gid://conduit/"; missing separator → "guid must contain type and id";
    /// non-numeric id → the integer parse error.
    pub fn parse(s: &str) -> Result<Self, ConduitError> {
        if s.is_empty() {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "guid is empty",
            ));
        }

        let Some(rest) = s.strip_prefix(GUID_PREFIX) else {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "guid must start with gid://conduit/",
            ));
        };

        let Some((type_tag, id_str)) = rest.split_once('/') else {
            return Err(ConduitError::new(
                ErrorKind::InvalidRequest,
                "guid must contain type and id",
            ));
        };

        let id = id_str.parse::<i64>().map_err(|err| {
            ConduitError::new(ErrorKind::InvalidRequest, format!("invalid guid id: {err}"))
        })?;

        Ok(Self {
            type_tag: type_tag.to_string(),
            id,
        })
    }
}

impl fmt::Display for OpenApiGuid {
    // Mirrors Go `MarshalGQL`: `gid://conduit/<Type>/<id>` (unquoted here; the
    // GraphQL layer quotes it as a string value).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{GUID_PREFIX}{}/{}", self.type_tag, self.id)
    }
}

/// ent type tag for an API key GUID, mirrored from Go
/// `r.resolveAPIKey(..., ent.TypeAPIKey, ...)` (`openapi/helper.go:17`).
pub const GUID_TYPE_API_KEY: &str = "APIKey";
/// ent type tag for an API key profile template GUID, mirrored from Go
/// `r.resolveTemplate(..., ent.TypeAPIKeyProfileTemplate, ...)`
/// (`openapi/helper.go:27`).
pub const GUID_TYPE_API_KEY_PROFILE_TEMPLATE: &str = "APIKeyProfileTemplate";

/// Validate a caller-supplied GUID's ent type and extract its numeric id.
///
/// Mirrors Go `guidID` (`internal/server/gql/openapi/helper.go:40-50`): a nil
/// GUID passes through as `None` so the caller can hand the result straight to
/// the exactly-one-of validation in the biz `GetForRead` helpers; a GUID of the
/// wrong ent type is rejected with a `BadRequest` error before any DB lookup,
/// so a `gid://conduit/Channel/12` handed to the `apiKey(id:)` resolver never
/// reaches the privacy layer.
pub fn validate_guid_type(
    guid: Option<&OpenApiGuid>,
    expected_type: &str,
) -> Result<Option<i64>, ConduitError> {
    match guid {
        None => Ok(None),
        Some(g) => {
            if g.type_tag != expected_type {
                return Err(ConduitError::new(
                    ErrorKind::InvalidRequest,
                    format!(
                        "invalid id: expected a {expected_type} GUID, got {}",
                        g.type_tag
                    ),
                ));
            }
            Ok(Some(g.id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------
    // Time
    // -------------------------------------------------------------------

    #[test]
    fn time_scalar_round_trips_rfc3339() -> Result<(), Box<dyn std::error::Error>> {
        let parsed = match GqlTime::parse(Value::String("2024-06-28T12:34:56.789Z".to_string())) {
            Ok(t) => t,
            Err(err) => return Err(format!("parse failed: {err:?}").into()),
        };
        match parsed.to_value() {
            Value::String(out) => {
                // Round-trip must preserve the instant (formatting of the
                // fractional part may differ from the input).
                let back = DateTime::parse_from_rfc3339(&out)?;
                assert_eq!(back.with_timezone(&Utc), parsed.0);
                assert!(out.ends_with('Z'), "UTC must render with a Z suffix");
            }
            other => panic!("Time must render a string, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn time_scalar_rejects_non_string() {
        assert!(GqlTime::parse(Value::Boolean(true)).is_err());
    }

    // -------------------------------------------------------------------
    // Decimal — Go objects.MarshalDecimal emits a RAW number token.
    // -------------------------------------------------------------------

    #[test]
    fn decimal_scalar_marshals_integer_as_raw_number() {
        // Mirrors the Go e2e assertion `require.Equal(t, "2",
        // string(u.Usage.TotalCost))` — the wire token is the number 2.
        let v = GqlDecimal(Decimal::from(2)).to_value();
        assert_eq!(v, Value::Number(2i64.into()));
    }

    #[test]
    fn decimal_scalar_normalizes_trailing_zeros_like_shopspring() -> Result<(), rust_decimal::Error>
    {
        // shopspring String() renders 2.00 as "2"; normalize() mirrors that.
        let d = "2.00".parse::<Decimal>()?;
        assert_eq!(GqlDecimal(d).to_value(), Value::Number(2i64.into()));
        Ok(())
    }

    #[test]
    fn decimal_scalar_marshals_fraction_as_number() -> Result<(), rust_decimal::Error> {
        let d = "12.5".parse::<Decimal>()?;
        match GqlDecimal(d).to_value() {
            Value::Number(n) => assert_eq!(n.as_f64(), Some(12.5)),
            other => panic!("expected a number, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn decimal_scalar_parses_string_and_number() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go UnmarshalDecimal: string and number are both accepted.
        match GqlDecimal::parse(Value::String("12.34".to_string())) {
            Ok(d) => assert_eq!(d.0, "12.34".parse::<Decimal>()?),
            Err(err) => return Err(format!("string parse failed: {err:?}").into()),
        }
        match GqlDecimalInput::parse(Value::Number(7i64.into())) {
            Ok(d) => assert_eq!(d.0, Decimal::from(7)),
            Err(err) => return Err(format!("number parse failed: {err:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn decimal_scalar_rejects_other_kinds() {
        // Mirrors Go's `failed to decode decimal: %v %T` fallthrough.
        assert!(GqlDecimal::parse(Value::Boolean(true)).is_err());
        assert!(GqlDecimalInput::parse(Value::Null).is_err());
    }

    // -------------------------------------------------------------------
    // GUID parse/format — mirrors objects.GUID UnmarshalGQL/MarshalGQL.
    // -------------------------------------------------------------------

    #[test]
    fn guid_parse_round_trips() -> Result<(), ConduitError> {
        let g = OpenApiGuid::parse("gid://conduit/APIKey/42")?;
        assert_eq!(g.type_tag, "APIKey");
        assert_eq!(g.id, 42);
        assert_eq!(g.to_string(), "gid://conduit/APIKey/42");
        Ok(())
    }

    #[test]
    fn guid_parse_mirrors_go_error_messages() {
        match OpenApiGuid::parse("") {
            Err(err) => assert_eq!(err.message, "guid is empty"),
            Ok(_) => panic!("empty guid must fail"),
        }
        match OpenApiGuid::parse("urn:whatever") {
            Err(err) => assert_eq!(err.message, "guid must start with gid://conduit/"),
            Ok(_) => panic!("wrong prefix must fail"),
        }
        match OpenApiGuid::parse("gid://conduit/APIKeyOnly") {
            Err(err) => assert_eq!(err.message, "guid must contain type and id"),
            Ok(_) => panic!("missing id must fail"),
        }
        match OpenApiGuid::parse("gid://conduit/APIKey/not-a-number") {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::InvalidRequest);
                assert!(err.message.contains("invalid guid id"));
            }
            Ok(_) => panic!("non-numeric id must fail"),
        }
    }

    // -------------------------------------------------------------------
    // validate_guid_type — mirrors Go helper.go:guidID.
    // -------------------------------------------------------------------

    /// A nil GUID passes through as `None`, mirroring Go `guidID(nil, ...)`.
    #[test]
    fn validate_guid_type_none_passes_through() {
        match validate_guid_type(None, GUID_TYPE_API_KEY) {
            Ok(None) => {}
            other => panic!("nil GUID must pass through as None, got {other:?}"),
        }
    }

    /// A correctly-typed GUID yields its numeric id (`helper.go:17`).
    #[test]
    fn validate_guid_type_correct_type_returns_id() {
        let g = OpenApiGuid::new(GUID_TYPE_API_KEY, 42);
        match validate_guid_type(Some(&g), GUID_TYPE_API_KEY) {
            Ok(Some(42)) => {}
            other => panic!("correct-type GUID must return Some(42), got {other:?}"),
        }
    }

    /// A wrong-type GUID is rejected as `InvalidRequest` (HTTP 400) before any
    /// DB lookup, mirroring Go `guidID`'s error branch (`helper.go:45-47`).
    #[test]
    fn validate_guid_type_wrong_type_rejected_with_400() {
        let g = OpenApiGuid::new("Channel", 12);
        match validate_guid_type(Some(&g), GUID_TYPE_API_KEY) {
            Err(err) => {
                assert_eq!(err.kind, ErrorKind::InvalidRequest);
                assert_eq!(err.http_status, 400);
                assert!(
                    err.message.contains("APIKey") && err.message.contains("Channel"),
                    "error message must name expected and actual type, got: {}",
                    err.message
                );
            }
            Ok(_) => panic!("wrong-type GUID must be rejected"),
        }
    }

    /// The template resolver path uses the `APIKeyProfileTemplate` tag
    /// (`helper.go:27`); a template-tagged GUID must be accepted by that path
    /// and rejected by the APIKey path.
    #[test]
    fn validate_guid_type_template_tag_round_trip() {
        let tmpl = OpenApiGuid::new(GUID_TYPE_API_KEY_PROFILE_TEMPLATE, 7);
        match validate_guid_type(Some(&tmpl), GUID_TYPE_API_KEY_PROFILE_TEMPLATE) {
            Ok(Some(7)) => {}
            other => panic!("template-tagged GUID must return Some(7), got {other:?}"),
        }
        match validate_guid_type(Some(&tmpl), GUID_TYPE_API_KEY) {
            Err(err) => assert_eq!(err.kind, ErrorKind::InvalidRequest),
            Ok(_) => panic!("template GUID must be rejected by the APIKey path"),
        }
    }
}
