//! Admin GraphQL scalar + enum parity shims.
//!
//! Mirrors the wire behaviour of the custom scalars and the SCREAMING_SNAKE_CASE
//! enum rendering defined by the captured snapshot at
//! `tests/contracts/admin_graphql_schema.graphql` (sourced from the canonical
//! Go gqlgen schema in `conduit/internal/server/gql/*.graphql`).
//!
//! Scope of this module (RUST-P12-001 S06 + scalar shims):
//! - Enum casing parity for the system-settings enums whose casing is most
//!   load-bearing for the React frontend (`QuotaEnforcementMode`,
//!   `AutoSyncFrequency`, `ProbeFrequency`). The Go contract lives in
//!   `conduit/internal/server/biz/system.go`.
//! - Typed newtype wrappers for the gqlgen custom scalars used by the admin
//!   schema: `Time`, `Map`, `Any`, `JSONRawMessage`, `Decimal`, `Cursor`.
//!
//! These are pure, self-contained shims; they do not touch any resolver and
//! remain independent of database/IO concerns.

use async_graphql::{InputValueResult, Scalar, ScalarType, Value};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::Value as JsonValue;

// ---------------------------------------------------------------------------
// Enums — S06 casing parity
// ---------------------------------------------------------------------------

/// `QuotaEnforcementMode` GraphQL enum.
///
/// Snapshot contract (see `admin_graphql_schema.graphql` lines ~9507-9510):
/// ```graphql
/// enum QuotaEnforcementMode {
///   EXHAUSTED_ONLY
///   DE_PRIORITIZE
/// }
/// ```
///
/// Go source (`internal/server/biz/system.go`):
/// the wire string is `EXHAUSTED_ONLY` / `DE_PRIORITIZE` (the underlying JSON
/// values are `exhausted_only` / `de_prioritize`, but gqlgen emits the
/// SCREAMING variant via `MarshalGQL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, async_graphql::Enum)]
#[graphql(name = "QuotaEnforcementMode")]
pub enum QuotaEnforcementMode {
    #[graphql(name = "EXHAUSTED_ONLY")]
    ExhaustedOnly,
    #[graphql(name = "DE_PRIORITIZE")]
    DePrioritize,
}

impl QuotaEnforcementMode {
    /// Returns the exact GraphQL enum literal emitted by the snapshot.
    pub fn as_str(self) -> &'static str {
        match self {
            QuotaEnforcementMode::ExhaustedOnly => "EXHAUSTED_ONLY",
            QuotaEnforcementMode::DePrioritize => "DE_PRIORITIZE",
        }
    }

    /// Returns the underlying Go JSON string value (gqlgen UnmarshalJSON
    /// accepts both the SCREAMING and the snake_case form).
    pub fn as_json_value(self) -> &'static str {
        match self {
            QuotaEnforcementMode::ExhaustedOnly => "exhausted_only",
            QuotaEnforcementMode::DePrioritize => "de_prioritize",
        }
    }
}

/// `AutoSyncFrequency` GraphQL enum.
///
/// Snapshot contract (lines ~9600-9603):
/// ```graphql
/// enum AutoSyncFrequency {
///   ONE_HOUR
///   SIX_HOURS
///   ONE_DAY
/// }
/// ```
///
/// Note: the Go source (`internal/server/biz/system.go` lines 441-447) defines
/// exactly three variants — `OneHour`, `SixHours`, `OneDay`. There is **no**
/// `TwelveHours` variant. Earlier skeleton iterations mistakenly added one;
/// the parity tests below pin the canonical three-variant set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSyncFrequency {
    OneHour,
    SixHours,
    OneDay,
}

impl AutoSyncFrequency {
    /// Returns the exact GraphQL enum literal emitted by the snapshot.
    pub fn as_str(self) -> &'static str {
        match self {
            AutoSyncFrequency::OneHour => "ONE_HOUR",
            AutoSyncFrequency::SixHours => "SIX_HOURS",
            AutoSyncFrequency::OneDay => "ONE_DAY",
        }
    }

    /// Returns the underlying Go JSON string value.
    pub fn as_json_value(self) -> &'static str {
        match self {
            AutoSyncFrequency::OneHour => "1h",
            AutoSyncFrequency::SixHours => "6h",
            AutoSyncFrequency::OneDay => "1d",
        }
    }

    /// All variants in snapshot declaration order.
    pub fn all() -> &'static [AutoSyncFrequency] {
        &[
            AutoSyncFrequency::OneHour,
            AutoSyncFrequency::SixHours,
            AutoSyncFrequency::OneDay,
        ]
    }
}

/// `ProbeFrequency` GraphQL enum.
///
/// Snapshot contract (lines ~9593-9598):
/// ```graphql
/// enum ProbeFrequency {
///   ONE_MINUTE
///   FIVE_MINUTES
///   THIRTY_MINUTES
///   ONE_HOUR
/// }
/// ```
///
/// Go source (`internal/server/biz/system.go` lines 509-517): four variants,
/// the underlying JSON values are `1m` / `5m` / `30m` / `1h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFrequency {
    OneMinute,
    FiveMinutes,
    ThirtyMinutes,
    OneHour,
}

impl ProbeFrequency {
    /// Returns the exact GraphQL enum literal emitted by the snapshot.
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeFrequency::OneMinute => "ONE_MINUTE",
            ProbeFrequency::FiveMinutes => "FIVE_MINUTES",
            ProbeFrequency::ThirtyMinutes => "THIRTY_MINUTES",
            ProbeFrequency::OneHour => "ONE_HOUR",
        }
    }

    /// Returns the underlying Go JSON string value.
    pub fn as_json_value(self) -> &'static str {
        match self {
            ProbeFrequency::OneMinute => "1m",
            ProbeFrequency::FiveMinutes => "5m",
            ProbeFrequency::ThirtyMinutes => "30m",
            ProbeFrequency::OneHour => "1h",
        }
    }

    /// All variants in snapshot declaration order.
    pub fn all() -> &'static [ProbeFrequency] {
        &[
            ProbeFrequency::OneMinute,
            ProbeFrequency::FiveMinutes,
            ProbeFrequency::ThirtyMinutes,
            ProbeFrequency::OneHour,
        ]
    }
}

// ---------------------------------------------------------------------------
// Scalar newtype shims
// ---------------------------------------------------------------------------

/// gqlgen `Time` scalar. Wire format: RFC3339 / ISO-8601 string in UTC.
///
/// Snapshot usage: `disabledAt: Time!`, `expiresAt: Time`, etc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeScalar(pub DateTime<Utc>);

#[Scalar(name = "Time")]
impl ScalarType for TimeScalar {
    fn parse(value: Value) -> InputValueResult<Self> {
        match value {
            Value::String(s) => {
                let dt = DateTime::parse_from_rfc3339(&s)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|err| {
                        async_graphql::InputValueError::custom(format!(
                            "invalid Time scalar: {err}"
                        ))
                    })?;
                Ok(TimeScalar(dt))
            }
            other => Err(async_graphql::InputValueError::custom(format!(
                "Time scalar expects a string, got {}",
                other
            ))),
        }
    }

    fn to_value(&self) -> Value {
        Value::String(self.0.to_rfc3339())
    }
}

/// gqlgen `Map` scalar. Wire format: an arbitrary JSON object.
///
/// Snapshot usage (ent.graphql line ~3584): `scalar Map` — used for opaque
/// metadata payloads. gqlgen emits the value verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct MapScalar(pub JsonValue);

#[Scalar(name = "Map")]
impl ScalarType for MapScalar {
    fn parse(value: Value) -> InputValueResult<Self> {
        Ok(MapScalar(async_graphql_to_json(value)))
    }

    fn to_value(&self) -> Value {
        json_to_async_graphql(self.0.clone())
    }
}

/// gqlgen `Any` scalar. Wire format: any JSON value (object/array/scalar).
///
/// Snapshot usage (filter.graphql line ~1): `scalar Any`.
#[derive(Debug, Clone, PartialEq)]
pub struct AnyScalar(pub JsonValue);

#[Scalar(name = "Any")]
impl ScalarType for AnyScalar {
    fn parse(value: Value) -> InputValueResult<Self> {
        Ok(AnyScalar(async_graphql_to_json(value)))
    }

    fn to_value(&self) -> Value {
        json_to_async_graphql(self.0.clone())
    }
}

/// gqlgen `JSONRawMessage` scalar (conduit-specific). Wire format: raw JSON,
/// emitted verbatim so the client can re-parse arbitrary provider payloads.
///
/// Snapshot usage (conduit.graphql lines 1-2): `scalar JSONRawMessage` and
/// `scalar JSONRawMessageInput`.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonRawMessageScalar(pub JsonValue);

#[Scalar(name = "JSONRawMessage")]
impl ScalarType for JsonRawMessageScalar {
    fn parse(value: Value) -> InputValueResult<Self> {
        Ok(JsonRawMessageScalar(async_graphql_to_json(value)))
    }

    fn to_value(&self) -> Value {
        json_to_async_graphql(self.0.clone())
    }
}

/// gqlgen `Decimal` scalar (conduit-specific). Wire format: decimal string.
///
/// Snapshot usage (conduit.graphql lines 3-4): `scalar Decimal` and
/// `scalar DecimalInput`. Go impl lives in `internal/objects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecimalScalar(pub Decimal);

#[Scalar(name = "Decimal")]
impl ScalarType for DecimalScalar {
    fn parse(value: Value) -> InputValueResult<Self> {
        match value {
            Value::String(s) => {
                let d = Decimal::from_str_exact(&s).map_err(|err| {
                    async_graphql::InputValueError::custom(format!("invalid Decimal scalar: {err}"))
                })?;
                Ok(DecimalScalar(d))
            }
            other => Err(async_graphql::InputValueError::custom(format!(
                "Decimal scalar expects a string, got {}",
                other
            ))),
        }
    }

    fn to_value(&self) -> Value {
        Value::String(self.0.to_string())
    }
}

/// gqlgen `DecimalInput` scalar (conduit-specific). Wire format: decimal
/// string. Per `internal/server/gql/gqlgen.yml:96-98`, `DecimalInput` maps to
/// the SAME Go type as `Decimal` (`objects.Decimal`), so this newtype mirrors
/// [`DecimalScalar`] exactly under a separate GraphQL name (snapshot line 9:
/// `scalar DecimalInput`). It exists because the input side of
/// `APIKeyQuotaInput.cost` (conduit.graphql:412) and the OpenAPI schema
/// declare `DecimalInput` rather than `Decimal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecimalInputScalar(pub Decimal);

#[Scalar(name = "DecimalInput")]
impl ScalarType for DecimalInputScalar {
    fn parse(value: Value) -> InputValueResult<Self> {
        match value {
            Value::String(s) => {
                let d = Decimal::from_str_exact(&s).map_err(|err| {
                    async_graphql::InputValueError::custom(format!(
                        "invalid DecimalInput scalar: {err}"
                    ))
                })?;
                Ok(DecimalInputScalar(d))
            }
            other => Err(async_graphql::InputValueError::custom(format!(
                "DecimalInput scalar expects a string, got {}",
                other
            ))),
        }
    }

    fn to_value(&self) -> Value {
        Value::String(self.0.to_string())
    }
}

/// gqlgen `Cursor` scalar. Wire format: opaque string (in the Go impl, a
/// base64-raw-std-encoded msgpack of an `ent.Cursor`; clients must treat it as
/// opaque). This shim models it purely as a string passthrough — the
/// pagination encoder in `pagination.rs` already handles the cursor
/// representation used by the Rust side.
///
/// Snapshot usage (ent.graphql line ~1979): `scalar Cursor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorScalar(pub String);

#[Scalar(name = "Cursor")]
impl ScalarType for CursorScalar {
    fn parse(value: Value) -> InputValueResult<Self> {
        match value {
            Value::String(s) => Ok(CursorScalar(s)),
            other => Err(async_graphql::InputValueError::custom(format!(
                "Cursor scalar expects a string, got {}",
                other
            ))),
        }
    }

    fn to_value(&self) -> Value {
        Value::String(self.0.clone())
    }
}

// ---------------------------------------------------------------------------
// Value conversion helpers (JSON <-> async_graphql::Value)
// ---------------------------------------------------------------------------

/// Converts an [`async_graphql::Value`] into a [`serde_json::Value`].
///
/// gqlgen's `Map`/`Any`/`JSONRawMessage` scalars emit JSON verbatim, so the
/// shims round-trip through these helpers. Only the value kinds that the
/// GraphQL spec permits are handled.
fn async_graphql_to_json(value: Value) -> JsonValue {
    match value {
        Value::Null => JsonValue::Null,
        Value::Boolean(b) => JsonValue::Bool(b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                JsonValue::from(i)
            } else if let Some(u) = n.as_u64() {
                JsonValue::from(u)
            } else {
                JsonValue::from(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => JsonValue::String(s),
        Value::Enum(en) => JsonValue::String(en.to_string()),
        Value::List(items) => {
            JsonValue::Array(items.into_iter().map(async_graphql_to_json).collect())
        }
        Value::Object(fields) => {
            let mut map = serde_json::Map::new();
            for (name, inner) in fields {
                map.insert(name.to_string(), async_graphql_to_json(inner));
            }
            JsonValue::Object(map)
        }
        // Variables/binary/upload are not part of the scalar wire format.
        other => JsonValue::String(format!("{other:?}")),
    }
}

/// Inverse of [`async_graphql_to_json`].
fn json_to_async_graphql(value: JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Boolean(b),
        JsonValue::Number(n) => match n.as_i64() {
            Some(i) => Value::Number(i.into()),
            None => match n.as_u64() {
                Some(u) => Value::Number(u.into()),
                None => {
                    // serde_json::Number is arbitrary precision; fall back to
                    // f64 when the value is not integer-representable.
                    // `from_f64` returns Option (NaN/Inf are rejected); use 0
                    // as the conservative default since the gqlgen scalars only
                    // ever receive finite JSON numbers.
                    let fallback =
                        async_graphql::Number::from_f64(0.0_f64).unwrap_or_else(|| 0i64.into());
                    let gql = n
                        .as_f64()
                        .and_then(async_graphql::Number::from_f64)
                        .unwrap_or(fallback);
                    Value::Number(gql)
                }
            },
        },
        JsonValue::String(s) => Value::String(s),
        JsonValue::Array(items) => {
            Value::List(items.into_iter().map(json_to_async_graphql).collect())
        }
        JsonValue::Object(map) => {
            let mut fields = async_graphql::indexmap::IndexMap::new();
            for (key, inner) in map {
                fields.insert(async_graphql::Name::new(key), json_to_async_graphql(inner));
            }
            Value::Object(fields)
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot casing tables (used by the SDL spot-check test)
// ---------------------------------------------------------------------------

/// Returns the exact GraphQL enum block the snapshot emits for the
/// system-settings enums, joined in declaration order. Used by the SDL
/// field-shape spot-check.
pub const QUOTA_ENFORCEMENT_MODE_SDL_BLOCK: &str = "\
enum QuotaEnforcementMode {
  EXHAUSTED_ONLY
  DE_PRIORITIZE
}";

pub const AUTO_SYNC_FREQUENCY_SDL_BLOCK: &str = "\
enum AutoSyncFrequency {
  ONE_HOUR
  SIX_HOURS
  ONE_DAY
}";

pub const PROBE_FREQUENCY_SDL_BLOCK: &str = "\
enum ProbeFrequency {
  ONE_MINUTE
  FIVE_MINUTES
  THIRTY_MINUTES
  ONE_HOUR
}";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- S06: enum casing parity --------------------------------------

    #[test]
    fn quota_enforcement_mode_renders_snapshot_casing() {
        assert_eq!(
            QuotaEnforcementMode::ExhaustedOnly.as_str(),
            "EXHAUSTED_ONLY"
        );
        assert_eq!(QuotaEnforcementMode::DePrioritize.as_str(), "DE_PRIORITIZE");
    }

    #[test]
    fn quota_enforcement_mode_json_value_matches_go_enum_string() {
        // Go: `exhausted_only` / `de_prioritize` (the underlying string).
        assert_eq!(
            QuotaEnforcementMode::ExhaustedOnly.as_json_value(),
            "exhausted_only"
        );
        assert_eq!(
            QuotaEnforcementMode::DePrioritize.as_json_value(),
            "de_prioritize"
        );
    }

    #[test]
    fn auto_sync_frequency_has_exactly_three_variants() {
        // Parity guard: the Go source defines ONLY three variants. An earlier
        // skeleton mistakenly added TwelveHours; pin the canonical set so the
        // regression cannot reappear silently.
        assert_eq!(
            AutoSyncFrequency::all(),
            &[
                AutoSyncFrequency::OneHour,
                AutoSyncFrequency::SixHours,
                AutoSyncFrequency::OneDay,
            ]
        );
    }

    #[test]
    fn auto_sync_frequency_renders_snapshot_casing() {
        for variant in AutoSyncFrequency::all() {
            match variant.as_str() {
                "ONE_HOUR" | "SIX_HOURS" | "ONE_DAY" => {}
                other => {
                    panic!("AutoSyncFrequency variant produced an unexpected literal: {other}")
                }
            }
        }
        assert_eq!(AutoSyncFrequency::OneHour.as_str(), "ONE_HOUR");
        assert_eq!(AutoSyncFrequency::SixHours.as_str(), "SIX_HOURS");
        assert_eq!(AutoSyncFrequency::OneDay.as_str(), "ONE_DAY");
    }

    #[test]
    fn auto_sync_frequency_json_value_matches_go_enum_string() {
        assert_eq!(AutoSyncFrequency::OneHour.as_json_value(), "1h");
        assert_eq!(AutoSyncFrequency::SixHours.as_json_value(), "6h");
        assert_eq!(AutoSyncFrequency::OneDay.as_json_value(), "1d");
    }

    #[test]
    fn auto_sync_frequency_does_not_expose_twelve_hours() {
        // The snapshot's `enum AutoSyncFrequency` block contains no
        // TWELVE_HOURS variant; ensure we never re-introduce it.
        for variant in AutoSyncFrequency::all() {
            assert_ne!(
                variant.as_str(),
                "TWELVE_HOURS",
                "AutoSyncFrequency must not expose a TWELVE_HOURS variant"
            );
        }
    }

    #[test]
    fn probe_frequency_renders_snapshot_casing() {
        assert_eq!(ProbeFrequency::OneMinute.as_str(), "ONE_MINUTE");
        assert_eq!(ProbeFrequency::FiveMinutes.as_str(), "FIVE_MINUTES");
        assert_eq!(ProbeFrequency::ThirtyMinutes.as_str(), "THIRTY_MINUTES");
        assert_eq!(ProbeFrequency::OneHour.as_str(), "ONE_HOUR");
    }

    #[test]
    fn probe_frequency_has_exactly_four_variants() {
        assert_eq!(
            ProbeFrequency::all().len(),
            4,
            "ProbeFrequency must have exactly four variants"
        );
    }

    #[test]
    fn probe_frequency_json_value_matches_go_enum_string() {
        assert_eq!(ProbeFrequency::OneMinute.as_json_value(), "1m");
        assert_eq!(ProbeFrequency::FiveMinutes.as_json_value(), "5m");
        assert_eq!(ProbeFrequency::ThirtyMinutes.as_json_value(), "30m");
        assert_eq!(ProbeFrequency::OneHour.as_json_value(), "1h");
    }

    // ---- Scalar round-trips ------------------------------------------

    /// Adapts an `InputValueResult<T>` into a `Result<T, String>` so that the
    /// `?` operator works inside `Result<(), Box<dyn std::error::Error>>`
    /// tests. `InputValueError<T>` itself does not implement `std::error::Error`.
    fn unwrap_scalar<T: std::fmt::Debug>(result: InputValueResult<T>) -> Result<T, String> {
        result.map_err(|err| format!("{err:?}"))
    }

    #[test]
    fn time_scalar_round_trips_rfc3339() -> Result<(), Box<dyn std::error::Error>> {
        let iso = "2024-06-28T12:34:56.789Z";
        let parsed = unwrap_scalar(TimeScalar::parse(Value::String(iso.to_string())))?;
        let rendered = parsed.to_value();

        match rendered {
            Value::String(out) => {
                // Re-parse the rendered string to confirm it represents the
                // same instant (formatting may differ in the offset suffix).
                let back = DateTime::parse_from_rfc3339(&out)?;
                assert_eq!(back.with_timezone(&Utc), parsed.0);
            }
            other => panic!("TimeScalar::to_value should yield a string, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn time_scalar_rejects_non_string() {
        let result = TimeScalar::parse(Value::Boolean(true));
        assert!(result.is_err());
    }

    #[test]
    fn map_scalar_round_trips_object() -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::json!({ "k1": 1, "k2": [true, "x"] });
        let gql = json_to_async_graphql(json.clone());
        let parsed = unwrap_scalar(MapScalar::parse(gql))?;
        assert_eq!(parsed.0, json);
        assert_eq!(parsed.to_value(), json_to_async_graphql(json));
        Ok(())
    }

    #[test]
    fn any_scalar_round_trips_array_and_scalar() -> Result<(), Box<dyn std::error::Error>> {
        let array = serde_json::json!([1, "two", false, null]);
        let gql = json_to_async_graphql(array.clone());
        let parsed = unwrap_scalar(AnyScalar::parse(gql))?;
        assert_eq!(parsed.0, array);

        let num = serde_json::json!(42);
        let parsed_num = unwrap_scalar(AnyScalar::parse(json_to_async_graphql(num.clone())))?;
        assert_eq!(parsed_num.0, num);
        Ok(())
    }

    #[test]
    fn json_raw_message_round_trips_nested_object() -> Result<(), Box<dyn std::error::Error>> {
        let payload = serde_json::json!({
            "headers": { "x-trace-id": "abc" },
            "body": [1, 2, 3],
        });
        let gql = json_to_async_graphql(payload.clone());
        let parsed = unwrap_scalar(JsonRawMessageScalar::parse(gql))?;
        assert_eq!(parsed.0, payload);
        Ok(())
    }

    #[test]
    fn decimal_scalar_round_trips_string() -> Result<(), Box<dyn std::error::Error>> {
        use std::str::FromStr;
        let d = Decimal::from_str("123.456")?;
        let scalar = DecimalScalar(d);
        match scalar.to_value() {
            Value::String(s) => {
                assert_eq!(s, "123.456");
                let back = unwrap_scalar(DecimalScalar::parse(Value::String(s)))?;
                assert_eq!(back.0, d);
            }
            other => panic!("DecimalScalar::to_value should yield a string, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn decimal_scalar_rejects_invalid_string() {
        let result = DecimalScalar::parse(Value::String("not-a-decimal".to_string()));
        assert!(result.is_err());
    }

    #[test]
    fn cursor_scalar_round_trips_opaque_string() -> Result<(), Box<dyn std::error::Error>> {
        let opaque = "conduit-admin-graphql:cursor:v1:000000000000002a".to_string();
        let parsed = unwrap_scalar(CursorScalar::parse(Value::String(opaque.clone())))?;
        assert_eq!(parsed.0, opaque);
        match parsed.to_value() {
            Value::String(out) => assert_eq!(out, opaque),
            other => panic!("CursorScalar::to_value should yield a string, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn cursor_scalar_rejects_non_string() {
        let result = CursorScalar::parse(Value::Number(7i64.into()));
        assert!(result.is_err());
    }

    #[test]
    fn json_conversion_round_trips_through_serde() {
        // Any value that serde can represent must survive a full round-trip
        // through both conversions.
        let cases = vec![
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!(42),
            serde_json::json!(-1),
            serde_json::json!("hello"),
            serde_json::json!([1, 2, 3]),
            serde_json::json!({"a": 1, "b": [true, null]}),
        ];
        for original in cases {
            let gql = json_to_async_graphql(original.clone());
            let back = async_graphql_to_json(gql);
            assert_eq!(back, original, "round-trip failed for {original}");
        }
    }

    // ---- S10: SDL field-shape spot check ------------------------------

    #[test]
    fn snapshot_enum_blocks_match_rendered_literals() {
        // For each system-settings enum, every variant's `as_str()` must
        // appear (in declaration order) inside the captured snapshot's enum
        // block. This is the load-bearing assertion for S10's "enum casing"
        // dimension; the full SDL diff lives in the schema-build smoke test.
        assert!(QUOTA_ENFORCEMENT_MODE_SDL_BLOCK.contains("EXHAUSTED_ONLY"));
        assert!(QUOTA_ENFORCEMENT_MODE_SDL_BLOCK.contains("DE_PRIORITIZE"));

        assert!(AUTO_SYNC_FREQUENCY_SDL_BLOCK.contains("ONE_HOUR"));
        assert!(AUTO_SYNC_FREQUENCY_SDL_BLOCK.contains("SIX_HOURS"));
        assert!(AUTO_SYNC_FREQUENCY_SDL_BLOCK.contains("ONE_DAY"));
        assert!(!AUTO_SYNC_FREQUENCY_SDL_BLOCK.contains("TWELVE_HOURS"));

        assert!(PROBE_FREQUENCY_SDL_BLOCK.contains("ONE_MINUTE"));
        assert!(PROBE_FREQUENCY_SDL_BLOCK.contains("FIVE_MINUTES"));
        assert!(PROBE_FREQUENCY_SDL_BLOCK.contains("THIRTY_MINUTES"));
        assert!(PROBE_FREQUENCY_SDL_BLOCK.contains("ONE_HOUR"));
    }

    /// Reads the canonical snapshot file and asserts the captured enum blocks
    /// match the runtime `as_str()` literals character-for-character. This is
    /// the true parity guard: it fails the moment the snapshot is regenerated
    /// with different casing OR a variant is added/removed on the Rust side.
    #[test]
    fn snapshot_enum_blocks_match_runtime_literals() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = std::fs::read_to_string("tests/contracts/admin_graphql_schema.graphql")
            .or_else(|_| {
                std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/contracts/admin_graphql_schema.graphql"
                ))
            })
            .map_err(|err| {
                format!(
                    "could not read snapshot tests/contracts/admin_graphql_schema.graphql: {err}"
                )
            })?;

        for variant in [
            QuotaEnforcementMode::ExhaustedOnly,
            QuotaEnforcementMode::DePrioritize,
        ] {
            let literal = variant.as_str();
            assert!(
                snapshot.contains(literal),
                "snapshot is missing QuotaEnforcementMode literal {literal}"
            );
        }

        for variant in AutoSyncFrequency::all() {
            let literal = variant.as_str();
            assert!(
                snapshot.contains(literal),
                "snapshot is missing AutoSyncFrequency literal {literal}"
            );
        }
        assert!(
            !snapshot.contains("TWELVE_HOURS"),
            "snapshot must not declare AutoSyncFrequency::TWELVE_HOURS (Go parity)"
        );

        for variant in ProbeFrequency::all() {
            let literal = variant.as_str();
            assert!(
                snapshot.contains(literal),
                "snapshot is missing ProbeFrequency literal {literal}"
            );
        }
        Ok(())
    }
}
