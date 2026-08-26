use std::{
    collections::BTreeMap,
    str::FromStr,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use conduit_core::objects::pricing::{
    ModelPrice, ModelPriceItem, PRICING_MODE_FLAT_FEE, PRICING_MODE_TIERED,
    PRICING_MODE_USAGE_PER_UNIT, PRICING_MODE_VOLUME, Pricing, price_item_code,
    prompt_write_cache_variant_code,
};
use conduit_db::RequestContext;
use conduit_llm::Usage;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model_price_service::PriceUnit;

/// Million-token divisor used by the Go `unitsInMillionTokens` helper. All
/// per-unit / tiered / volume subtotals are computed as
/// `price_per_unit * (quantity / million_tokens_divisor())`. `Decimal::new`
/// is not `const`-evaluable, so this is a function rather than a `const`.
fn million_tokens_divisor() -> Decimal {
    Decimal::new(1_000_000, 0)
}

pub type UsageServiceResult<T> = Result<T, UsageServiceError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UsageServiceError {
    #[error("invalid decimal for {field}: {value}")]
    InvalidDecimal { field: &'static str, value: String },
    #[error("usage persistence lock poisoned")]
    LockPoisoned,
    /// Calendar bucket could not be constructed for the requested date in the
    /// given `FixedOffset` (e.g. NaiveDate overflow from AddDate on a far-future
    /// timestamp). Mirrors the unreachable-but-defensive Go panics inside
    /// `xtime.GetCalendarPeriods` (Go would `panic` on a bad `time.Date`; we
    /// surface it as a typed error per workspace lints).
    #[error("invalid calendar window for the given offset")]
    InvalidCalendarWindow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub id: String,
    pub project_id: String,
    pub model: String,
    pub channel: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub prompt_unit_price: String,
    pub completion_unit_price: String,
    pub prompt_cost: String,
    pub completion_cost: String,
    pub total_cost: String,
}

impl UsageRecord {
    pub fn new(
        id: impl Into<String>,
        project_id: impl Into<String>,
        model: impl Into<String>,
        channel: impl Into<String>,
        prompt_tokens: u64,
        completion_tokens: u64,
        prompt_unit_price: impl Into<String>,
        completion_unit_price: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            project_id: project_id.into(),
            model: model.into(),
            channel: channel.into(),
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            prompt_unit_price: prompt_unit_price.into(),
            completion_unit_price: completion_unit_price.into(),
            prompt_cost: "0".to_string(),
            completion_cost: "0".to_string(),
            total_cost: "0".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostItem {
    pub project_id: String,
    pub model: String,
    pub channel: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub prompt_cost: String,
    pub completion_cost: String,
    pub total_cost: String,
}

impl CostItem {
    fn empty(project_id: String, model: String, channel: String) -> Self {
        Self {
            project_id,
            model,
            channel,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            prompt_cost: "0".to_string(),
            completion_cost: "0".to_string(),
            total_cost: "0".to_string(),
        }
    }
}

// ============================================================================
// RUST-P10-002 S07 — comprehensive UsageLog row mirroring the Go Ent schema
// `internal/ent/schema/usage_log.go` + `biz/usage_log.go::CreateUsageLogParams`.
//
// The pre-existing `UsageRecord` above is a *legacy* simplified shape used by
// the cost-aggregation tests (S04/S05/S06/S08, Turing/Anscombe). It does NOT
// cover the full Go field set required by S07. This `UsageLog` struct is the
// faithful 1:1 port of the Go UsageLog row — every field the Go
// `CreateUsageLog` mutator writes is represented here with the matching JSON
// camelCase tag and the Go optionality/zero-value semantics.
//
// Field parity (Go schema field → Rust field):
//   request_id                       (int,    Immutable, required) → request_id: i64
//   api_key_id                       (int,    Optional, Immutable) → api_key_id: Option<i64>
//   project_id                       (int,    Immutable, default 1)→ project_id: i64
//   channel_id                       (int,    Optional, Immutable) → channel_id: Option<i64>
//   model_id                         (string, Immutable)           → model_id: String
//   prompt_tokens                    (int64,  default 0)           → prompt_tokens: i64
//   completion_tokens                (int64,  default 0)           → completion_tokens: i64
//   total_tokens                     (int64,  default 0)           → total_tokens: i64
//   prompt_audio_tokens              (int64,  Optional, default 0) → prompt_audio_tokens: i64
//   prompt_cached_tokens             (int64,  Optional, default 0) → prompt_cached_tokens: i64
//   prompt_write_cached_tokens       (int64,  Optional, default 0) → prompt_write_cached_tokens: i64
//   prompt_write_cached_tokens_5m    (int64,  Optional, default 0) → prompt_write_cached_tokens_5m: i64
//   prompt_write_cached_tokens_1h    (int64,  Optional, default 0) → prompt_write_cached_tokens_1h: i64
//   completion_audio_tokens          (int64,  Optional, default 0) → completion_audio_tokens: i64
//   completion_reasoning_tokens      (int64,  Optional, default 0) → completion_reasoning_tokens: i64
//   completion_accepted_prediction_tokens (int64, Optional, dflt 0)→ completion_accepted_prediction_tokens: i64
//   completion_rejected_prediction_tokens (int64, Optional, dflt 0)→ completion_rejected_prediction_tokens: i64
//   source                           (enum,   default "api")       → source: UsageLogSource
//   format                           (string, default "openai/chat_completions") → format: String
//   total_cost                       (float,  Nillable, Optional)  → total_cost: Option<Decimal>
//   cost_items                       (JSON []objects.CostItem)     → cost_items: Vec<ComputedCostItem>
//   cost_price_reference_id          (string, Optional)            → cost_price_reference_id: Option<String>
//
// Notes on type choices:
//   * Go `int`/`int64` ids/counts → `i64` (workspace parity rule).
//   * Go `field.Float("total_cost")` is persisted as a float, but the Rust
//     port keeps the full-precision `Decimal` (S09: no f64 for final money).
//     `Option<>` mirrors Go's `Nillable().Optional()`.
//   * Go `cost_items` is `[]objects.CostItem` JSON — Rust `ComputedCostItem`
//     is the 1:1 serde port of `objects.CostItem` (itemCode /
//     promptWriteCacheVariantCode / quantity / subtotal / tierBreakdown).
// ============================================================================

/// Source of a usage-log row. Mirrors Go
/// `internal/ent/schema/usage_log.go::field.Enum("source").Values("api",
/// "playground", "test")`. The Go default is `"api"`; the Rust port mirrors
/// that via `UsageLogSource::default()`.
///
/// Serde renders the enum as the lowercase Go enum string (`"api"` /
/// `"playground"` / `"test"`), matching the PostgreSQL representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UsageLogSource {
    #[default]
    Api,
    Playground,
    Test,
}

impl UsageLogSource {
    /// Mirrors Go's `usagelog.Source(request.Source)` string cast — the enum
    /// is persisted and compared as its lowercase string form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Playground => "playground",
            Self::Test => "test",
        }
    }
}

/// Default request format — mirrors Go schema
/// `field.String("format").Default("openai/chat_completions")`.
pub const DEFAULT_USAGE_FORMAT: &str = "openai/chat_completions";

/// Comprehensive UsageLog row — 1:1 port of the Go Ent `UsageLog` schema.
/// See the module-level S07 comment above for the field-by-field mapping.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLog {
    /// Related request ID (Go: `request_id`, Immutable, required).
    pub request_id: i64,
    /// Project ID (Go: `project_id`, Immutable, default 1).
    #[serde(default = "default_project_id")]
    pub project_id: i64,
    /// API key ID (Go: `api_key_id`, Optional, Immutable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_id: Option<i64>,
    /// Channel ID used for the request (Go: `channel_id`, Optional,
    /// Immutable — optional for deleted channel, the field is not null).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<i64>,
    /// Model identifier used for the request (Go: `model_id`, Immutable).
    pub model_id: String,

    // --- Core usage metrics from llm.Usage ---
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,

    // --- Prompt tokens details from llm.PromptTokensDetails ---
    #[serde(default)]
    pub prompt_audio_tokens: i64,
    #[serde(default)]
    pub prompt_cached_tokens: i64,
    #[serde(default)]
    pub prompt_write_cached_tokens: i64,
    #[serde(default)]
    pub prompt_write_cached_tokens_5m: i64,
    #[serde(default)]
    pub prompt_write_cached_tokens_1h: i64,

    // --- Completion tokens details from llm.CompletionTokensDetails ---
    #[serde(default)]
    pub completion_audio_tokens: i64,
    #[serde(default)]
    pub completion_reasoning_tokens: i64,
    #[serde(default)]
    pub completion_accepted_prediction_tokens: i64,
    #[serde(default)]
    pub completion_rejected_prediction_tokens: i64,

    // --- Additional metadata ---
    /// Source of the request (Go enum `api`/`playground`/`test`, default `api`).
    #[serde(default)]
    pub source: UsageLogSource,
    /// Request format used (Go default `"openai/chat_completions"`).
    #[serde(default = "default_format")]
    pub format: String,

    // --- Cost fields ---
    /// Total cost (Go `field.Float` Nillable/Optional; Rust keeps full
    /// precision via `Decimal`, optional to mirror Go nil). Serialized as a
    /// JSON **number** (S10) to match Go's `field.Float` → `float64` wire
    /// form — `cost_items[].subtotal`, by contrast, stays a JSON string to
    /// match Go's `decimal.Decimal` form. See [`total_cost_as_float`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "total_cost_as_float"
    )]
    pub total_cost: Option<Decimal>,
    /// Detailed cost breakdown items in JSON (Go `[]objects.CostItem`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cost_items: Vec<ComputedCostItem>,
    /// Reference ID to the channel model price version used for cost
    /// calculation (Go: `cost_price_reference_id`, Optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_price_reference_id: Option<String>,
}

/// Default project id — mirrors Go `field.Int("project_id").Default(1)`.
fn default_project_id() -> i64 {
    1
}

/// Default format — mirrors Go
/// `field.String("format").Default("openai/chat_completions")`.
fn default_format() -> String {
    DEFAULT_USAGE_FORMAT.to_string()
}

// ============================================================================
// RUST-P10-002 S10 — Decimal → JSON-float compatibility for `total_cost`.
//
// Go's `UsageLog` schema stores `total_cost` as `field.Float("total_cost")`
// and `biz/usage_log.go::computeUsageCost` converts the internal
// `decimal.Decimal` result of `ComputeUsageCost` via `total.InexactFloat64()`
// before persisting. The Go GraphQL/JSON surface therefore serializes
// `totalCost` as a JSON **number** (e.g. `0.42`), NOT a string.
//
// `objects.CostItem.Subtotal`, by contrast, is `decimal.Decimal` and serializes
// (via `shopspring/decimal`) as a JSON **string** (`"0.42"`). The Rust port's
// default `rust_decimal` `serde` feature already matches that string form, so
// `ComputedCostItem` / `CostItemDetail` / `TierCostDetail` are unchanged.
//
// This module gives `UsageLog.total_cost` a one-off serde adapter so the
// `Option<Decimal>` field round-trips as a JSON number when present — keeping
// the full-precision `Decimal` in-memory (S09: no f64 for the actual money
// math) while producing the Go-compatible wire form. Deserialization accepts
// BOTH number and string so the Rust side can ingest legacy snapshots either
// way.
//
// `to_f64` mirrors Go's `Decimal.InexactFloat64()` (lossy); it is used ONLY
// at the serialization boundary, never for arithmetic.
// ============================================================================

/// Convert a `Decimal` to `f64` mirroring Go's `shopspring/decimal`
/// `InexactFloat64()`. Used solely at the JSON-serialization boundary to
/// match Go's `field.Float` wire form — all money math stays in `Decimal`.
fn decimal_to_f64(d: Decimal) -> f64 {
    use std::str::FromStr;
    f64::from_str(&d.to_string()).unwrap_or(0.0)
}

/// Serialize `Option<Decimal>` as a JSON **number** when `Some`, mirroring
/// Go's `field.Float("total_cost")` → `float64` wire form. `None` serializes
/// as `null` (Go `Nillable`).
pub mod total_cost_as_float {
    use std::str::FromStr;

    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::decimal_to_f64;

    pub fn serialize<S>(value: &Option<Decimal>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(d) => Serialize::serialize(&decimal_to_f64(*d), serializer),
            None => serializer.serialize_none(),
        }
    }

    /// Deserialize accepting BOTH a JSON number (Go `float64` form) and a JSON
    /// string (legacy Rust snapshot form). Returns `None` for JSON `null`.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Decimal>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;
        match v {
            serde_json::Value::Null => Ok(None),
            serde_json::Value::Number(n) => {
                let s = n.to_string();
                Decimal::from_str(&s)
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            }
            serde_json::Value::String(s) => Decimal::from_str(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            other => Err(serde::de::Error::custom(format!(
                "expected null, number, or string for total_cost, got {}",
                other
            ))),
        }
    }
}

impl UsageLog {
    /// Build a new `UsageLog` from the `CreateUsageLogParams`-equivalent
    /// inputs. Mirrors Go `biz/usage_log.go::(*UsageLogService).CreateUsageLog`
    /// field-population logic (lines 103-154): the core token counts come
    /// from `llm::Usage`, the prompt/completion detail fields are populated
    /// only when the corresponding `Usage` detail struct is present (Go:
    /// `if params.Usage.PromptTokensDetails != nil { ... }`), and the
    /// cost/reference fields default to empty/None.
    ///
    /// `request_id`, `project_id`, `channel_id` (when supplied), `model_id`,
    /// `source`, `format`, and `api_key_id` are the caller's responsibility
    /// — they are NOT derivable from `llm::Usage`. This keeps the helper
    /// pure and free of context/DB lookups (the Go `contexts.GetAPIKey`
    /// fallback is the HTTP layer's job, mirroring the Go separation).
    #[allow(clippy::too_many_arguments)]
    pub fn from_usage(
        request_id: i64,
        project_id: i64,
        channel_id: Option<i64>,
        model_id: impl Into<String>,
        source: UsageLogSource,
        format: impl Into<String>,
        api_key_id: Option<i64>,
        usage: &Usage,
    ) -> Self {
        Self {
            request_id,
            project_id,
            api_key_id,
            channel_id,
            model_id: model_id.into(),
            prompt_tokens: usage.prompt_tokens as i64,
            completion_tokens: usage.completion_tokens as i64,
            total_tokens: usage.total_tokens as i64,
            prompt_audio_tokens: usage.prompt_details.audio_tokens as i64,
            prompt_cached_tokens: usage.prompt_details.cached_tokens as i64,
            prompt_write_cached_tokens: usage.prompt_details.write_cached_tokens as i64,
            prompt_write_cached_tokens_5m: usage.prompt_details.write_cached_tokens_5m as i64,
            prompt_write_cached_tokens_1h: usage.prompt_details.write_cached_tokens_1h as i64,
            completion_audio_tokens: usage.completion_details.audio_tokens as i64,
            completion_reasoning_tokens: usage.completion_details.reasoning_tokens as i64,
            completion_accepted_prediction_tokens: usage
                .completion_details
                .accepted_prediction_tokens
                as i64,
            completion_rejected_prediction_tokens: usage
                .completion_details
                .rejected_prediction_tokens
                as i64,
            source,
            format: format.into(),
            total_cost: None,
            cost_items: Vec::new(),
            cost_price_reference_id: None,
        }
    }

    /// Attach the computed cost breakdown. Mirrors the Go tail of
    /// `CreateUsageLog`:
    /// ```text
    /// mut = mut.SetNillableTotalCost(totalCost).SetCostItems(costItems)
    /// if priceReferenceID != "" { mut = mut.SetCostPriceReferenceID(priceReferenceID) }
    /// ```
    /// `total_cost` is stored as `Some(...)` only when the price was found
    /// (Go: `totalCost *float64` non-nil); `cost_price_reference_id` is
    /// stored only when non-empty (Go skips `SetCostPriceReferenceID` for `""`).
    pub fn with_cost(
        mut self,
        total_cost: Decimal,
        cost_items: Vec<ComputedCostItem>,
        price_reference_id: String,
    ) -> Self {
        self.total_cost = Some(total_cost);
        self.cost_items = cost_items;
        if !price_reference_id.is_empty() {
            self.cost_price_reference_id = Some(price_reference_id);
        } else {
            self.cost_price_reference_id = None;
        }
        self
    }

    /// (S10) Return `total_cost` as `f64`, mirroring Go's
    /// `(*UsageLog).TotalCost` (`*float64`) — i.e. the post-`InexactFloat64`
    /// wire/storage value. `None` when no cost was computed (Go nil pointer).
    /// The in-memory `Decimal` keeps full precision; this is the lossy view
    /// only for downstream consumers that expect the Go float64 form.
    pub fn total_cost_as_f64(&self) -> Option<f64> {
        self.total_cost.map(decimal_to_f64)
    }

    /// (S10) Sum the `subtotal` of every entry in `cost_items` into a single
    /// `Decimal` total — the pure equivalent of Go's
    /// `ComputeUsageCost(...).Total` (the second return value). This is the
    /// value the caller would then pass to [`UsageLog::with_cost`] (after
    /// converting via [`decimal_to_f64`] / [`Self::total_cost_as_f64`] at the
    /// persistence boundary, exactly as Go does in `computeUsageCost`).
    pub fn sum_cost_items(&self) -> Decimal {
        self.cost_items
            .iter()
            .map(|item| item.detail.subtotal)
            .sum()
    }
}

// ============================================================================
// Dashboard timezone-aware calendar buckets — RUST-P10-002 S13.
//
// Pure port of Go `internal/pkg/xtime.GetCalendarPeriods(loc *time.Location)`
// (the helper the dashboard resolvers in `gql/dashboard_helpers.go::parseTimeWindow`
// call to bound a usage-log aggregation by "day" / "week" / "month"). Go computes
// the calendar boundaries in `loc`'s wall clock and returns them as UTC instants;
// we do the same with `chrono::FixedOffset`.
//
// `FixedOffset` is intentional: the workspace has no `chrono-tz` dependency
// (consistent with the quota timezone work in P10-003 S07, see
// `quota_service.rs::QuotaPeriod::window_in_offset`). A system-configured
// timezone always resolves to a fixed offset at the query timestamp, which is
// exactly the case `FixedOffset` represents; DST transitions would need
// `chrono-tz` and are out of scope (the Go `time.LoadLocation` fallback for an
// unknown/empty timezone is UTC, i.e. `FixedOffset::east_opt(0)`).
// ============================================================================

/// Half-open calendar period `[start, end)` in **UTC**. Mirrors Go
/// `xtime.Period` — both `start` and `end` are absolute instants even though
/// they were computed in a local wall clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarPeriod {
    /// Inclusive period start (UTC instant).
    pub start: DateTime<Utc>,
    /// Exclusive period end (UTC instant).
    pub end: DateTime<Utc>,
}

impl CalendarPeriod {
    /// Half-open `[start, end)` containment test. Mirrors Go's implicit
    /// `t.Before(End) && !t.Before(Start)` checks in dashboard filtering.
    pub fn contains(&self, t: DateTime<Utc>) -> bool {
        t >= self.start && t < self.end
    }

    /// Duration of the period (`end - start`).
    pub fn duration(&self) -> Duration {
        self.end.signed_duration_since(self.start)
    }
}

/// Calendar-aligned time periods used by dashboard aggregations. Pure port of
/// Go `xtime.CalendarPeriods`.
///
/// Field semantics mirror the Go doc comments exactly:
/// - `today`: `[00:00:00 today, 00:00:00 tomorrow)`
/// - `this_week`: `[Monday 00:00:00 this week, Monday 00:00:00 next week)`
///   (ISO 8601 Monday-based week — Go remaps Sunday from `0` to `7`).
/// - `last_week`: `[Monday 00:00:00 last week, Monday 00:00:00 this week)`
/// - `this_month`: `[1st day 00:00:00 this month, 1st day of next month 00:00:00)`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarPeriods {
    pub today: CalendarPeriod,
    pub this_week: CalendarPeriod,
    pub last_week: CalendarPeriod,
    pub this_month: CalendarPeriod,
}

/// (S13) Compute calendar-aligned dashboard periods for `now` interpreted in
/// `offset`'s wall clock. Pure port of Go
/// `xtime.GetCalendarPeriods(loc *time.Location)`.
///
/// All four periods are returned as UTC instants (matching Go, which calls
/// `.UTC()` on every boundary before returning). `offset` only affects which
/// **calendar day / week / month** `now` falls into — the absolute instants are
/// the same no matter how `now` is displayed.
///
/// # Errors
/// Returns [`UsageServiceError::InvalidCalendarWindow`] only if a calendar
/// boundary cannot be constructed (e.g. far-future overflow). In practice the
/// inputs are always well-formed.
pub fn get_calendar_periods(
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> UsageServiceResult<CalendarPeriods> {
    // Convert the wall-clock instant to the configured offset — this is the
    // value Go names `nowLocal := utcNowFunc().In(loc)`. All subsequent
    // `time.Date(...)` calls in Go happen in `loc`, which is exactly
    // `offset.from_local_datetime(...)` here.
    let now_local = now.with_timezone(&offset);

    // --- Today / tomorrow local-midnight ---
    let today_start_local = local_midnight(now_local, offset)?;
    let today_end_local = today_start_local + Duration::days(1);

    // --- This week / last week (Monday-based) ---
    // Go: `weekday := int(nowLocal.Weekday()); if weekday == 0 { weekday = 7 }`
    // chrono's `num_days_from_monday()` is already Monday=0..Sunday=6, which
    // matches the post-remap Go value (Go's Monday=1..Saturday=6, Sunday→7
    // becomes `weekday - 1 = 6`). So `num_days_from_monday()` is the direct
    // equivalent of Go's `weekday - 1` expression.
    let days_since_monday = now_local.weekday().num_days_from_monday() as i64;
    let this_week_start_local = today_start_local - Duration::days(days_since_monday);
    let this_week_end_local = this_week_start_local + Duration::days(7);
    let last_week_start_local = this_week_start_local - Duration::days(7);
    let last_week_end_local = this_week_start_local;

    // --- This month / next month ---
    // Go: `time.Date(nowLocal.Year(), nowLocal.Month(), 1, 0,0,0,0, loc)` then
    // `.AddDate(0, 1, 0)`. We mirror the month-arithmetic via `shift_months_local`
    // (same helper the quota layer uses) so a Shanghai 2024-01-01 boundary
    // advances to 2024-02-01 in the *local* calendar, not a UTC-derived date.
    let this_month_start_local = local_month_start(now_local.year(), now_local.month(), offset)?;
    let this_month_end_local = shift_months_local(now_local.year(), now_local.month(), 1, offset)?;

    Ok(CalendarPeriods {
        today: CalendarPeriod {
            start: today_start_local.with_timezone(&Utc),
            end: today_end_local.with_timezone(&Utc),
        },
        this_week: CalendarPeriod {
            start: this_week_start_local.with_timezone(&Utc),
            end: this_week_end_local.with_timezone(&Utc),
        },
        last_week: CalendarPeriod {
            start: last_week_start_local.with_timezone(&Utc),
            end: last_week_end_local.with_timezone(&Utc),
        },
        this_month: CalendarPeriod {
            start: this_month_start_local.with_timezone(&Utc),
            end: this_month_end_local.with_timezone(&Utc),
        },
    })
}

/// (S13) Mirrors Go `xtime.FormatUTCOffset(offsetSeconds int) string`. Renders
/// the canonical `+HH:MM` / `-HH:MM` form used by the legacy Go dashboard
/// contract. PostgreSQL runtime queries use native timezone expressions.
///
/// Pure function over a `FixedOffset` so callers don't need a separate
/// `offset_seconds` int — the Go API only ever receives an offset it derived
/// from the same `*time.Location`.
pub fn format_utc_offset(offset: FixedOffset) -> String {
    let total_secs = offset.local_minus_utc();
    let sign = if total_secs < 0 { '-' } else { '+' };
    let abs = total_secs.unsigned_abs() as i64;
    let hours = abs / 3600;
    let minutes = (abs % 3600) / 60;
    format!("{sign}{hours:02}:{minutes:02}")
}

/// Local-midnight `DateTime` in `offset` for the calendar date of `now_local`
/// (Go: `time.Date(nowLocal.Year(), nowLocal.Month(), nowLocal.Day(),
/// 0,0,0,0, loc)`). Same shape as `quota_service.rs::local_midnight` but lives
/// here so dashboard aggregation has no cross-service dependency.
fn local_midnight(
    now_local: DateTime<FixedOffset>,
    offset: FixedOffset,
) -> UsageServiceResult<DateTime<FixedOffset>> {
    let date = NaiveDate::from_ymd_opt(now_local.year(), now_local.month(), now_local.day())
        .ok_or(UsageServiceError::InvalidCalendarWindow)?;
    let ndt = date
        .and_hms_opt(0, 0, 0)
        .ok_or(UsageServiceError::InvalidCalendarWindow)?;
    offset
        .from_local_datetime(&ndt)
        .single()
        .ok_or(UsageServiceError::InvalidCalendarWindow)
}

/// First-of-month local-midnight `DateTime` in `offset` (Go:
/// `time.Date(year, month, 1, 0,0,0,0, loc)`).
fn local_month_start(
    year: i32,
    month: u32,
    offset: FixedOffset,
) -> UsageServiceResult<DateTime<FixedOffset>> {
    let date =
        NaiveDate::from_ymd_opt(year, month, 1).ok_or(UsageServiceError::InvalidCalendarWindow)?;
    let ndt = date
        .and_hms_opt(0, 0, 0)
        .ok_or(UsageServiceError::InvalidCalendarWindow)?;
    offset
        .from_local_datetime(&ndt)
        .single()
        .ok_or(UsageServiceError::InvalidCalendarWindow)
}

/// Shift `(year, month)` by `delta_months` calendar months and return the
/// first-of-month local-midnight `DateTime` in `offset`. Mirrors Go's
/// `t.AddDate(0, delta, 0)`. Identical algorithm to
/// `quota_service.rs::shift_months_local`.
fn shift_months_local(
    year: i32,
    month: u32,
    delta_months: i32,
    offset: FixedOffset,
) -> UsageServiceResult<DateTime<FixedOffset>> {
    let month_index = year
        .checked_mul(12)
        .and_then(|m| m.checked_add((month as i32) - 1))
        .and_then(|m| m.checked_add(delta_months))
        .ok_or(UsageServiceError::InvalidCalendarWindow)?;
    let new_year = month_index.div_euclid(12);
    let new_month = u32::try_from(month_index.rem_euclid(12) + 1)
        .map_err(|_| UsageServiceError::InvalidCalendarWindow)?;
    local_month_start(new_year, new_month, offset)
}

/// Resolve the dashboard "day / week / month / allTime" filter to an optional
/// `since` UTC instant, exactly mirroring Go
/// `gql/dashboard_helpers.go::(*queryResolver).parseTimeWindow`.
///
/// Returns `Ok(None)` for `allTime` (and any unrecognized / empty value — Go
/// disables filtering in those cases); `Ok(Some(since))` for `day` / `week` /
/// `month`. The caller is expected to AND this with whatever other predicates
/// the dashboard query carries.
pub fn parse_time_window(
    now: DateTime<Utc>,
    offset: FixedOffset,
    time_window: Option<&str>,
) -> UsageServiceResult<Option<DateTime<Utc>>> {
    let Some(tw) = time_window else {
        return Ok(None);
    };
    if tw.is_empty() || tw == "allTime" {
        return Ok(None);
    }
    let periods = get_calendar_periods(now, offset)?;
    let since = match tw {
        "day" => periods.today.start,
        "week" => periods.this_week.start,
        "month" => periods.this_month.start,
        // Unknown value — Go sets applyFilter=false; we surface that as None.
        _ => return Ok(None),
    };
    Ok(Some(since))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageCostBucket {
    Prompt,
    Completion,
    Cache,
    Audio,
    Reasoning,
}

impl UsageCostBucket {
    fn price_field(self) -> &'static str {
        match self {
            Self::Prompt => "prompt_price",
            Self::Completion => "completion_price",
            Self::Cache => "cache_price",
            Self::Audio => "audio_price",
            Self::Reasoning => "reasoning_price",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCostBreakdownTokens {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub cache_tokens: u64,
    #[serde(default)]
    pub audio_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

impl UsageCostBreakdownTokens {
    pub fn new(
        prompt_tokens: u64,
        completion_tokens: u64,
        cache_tokens: u64,
        audio_tokens: u64,
        reasoning_tokens: u64,
    ) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            cache_tokens,
            audio_tokens,
            reasoning_tokens,
        }
    }

    pub fn from_usage(usage: &Usage) -> Self {
        let cache_tokens = usage
            .prompt_details
            .cached_tokens
            .saturating_add(usage.prompt_details.write_cached_tokens)
            .saturating_add(usage.prompt_details.write_cached_tokens_5m)
            .saturating_add(usage.prompt_details.write_cached_tokens_1h);
        let audio_tokens = usage
            .prompt_details
            .audio_tokens
            .saturating_add(usage.completion_details.audio_tokens);
        let reasoning_tokens = usage
            .prompt_details
            .reasoning_tokens
            .saturating_add(usage.completion_details.reasoning_tokens);

        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cache_tokens,
            audio_tokens,
            reasoning_tokens,
        }
    }

    fn total_tokens(self) -> u64 {
        self.prompt_tokens
            .saturating_add(self.completion_tokens)
            .saturating_add(self.cache_tokens)
            .saturating_add(self.audio_tokens)
            .saturating_add(self.reasoning_tokens)
    }

    fn tokens_for(self, bucket: UsageCostBucket) -> u64 {
        match bucket {
            UsageCostBucket::Prompt => self.prompt_tokens,
            UsageCostBucket::Completion => self.completion_tokens,
            UsageCostBucket::Cache => self.cache_tokens,
            UsageCostBucket::Audio => self.audio_tokens,
            UsageCostBucket::Reasoning => self.reasoning_tokens,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCostBreakdownRates {
    pub provider: String,
    pub model: String,
    pub prompt_price: String,
    pub completion_price: String,
    pub cache_price: String,
    pub audio_price: String,
    pub reasoning_price: String,
    pub unit: PriceUnit,
    pub currency: String,
}

impl UsageCostBreakdownRates {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        prompt_price: impl Into<String>,
        completion_price: impl Into<String>,
        cache_price: impl Into<String>,
        audio_price: impl Into<String>,
        reasoning_price: impl Into<String>,
        unit: PriceUnit,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            prompt_price: prompt_price.into(),
            completion_price: completion_price.into(),
            cache_price: cache_price.into(),
            audio_price: audio_price.into(),
            reasoning_price: reasoning_price.into(),
            unit,
            currency: currency.into(),
        }
    }

    fn price_for(&self, bucket: UsageCostBucket) -> &str {
        match bucket {
            UsageCostBucket::Prompt => &self.prompt_price,
            UsageCostBucket::Completion => &self.completion_price,
            UsageCostBucket::Cache => &self.cache_price,
            UsageCostBucket::Audio => &self.audio_price,
            UsageCostBucket::Reasoning => &self.reasoning_price,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCostBreakdownItem {
    pub bucket: UsageCostBucket,
    pub tokens: u64,
    pub unit_price: String,
    pub cost: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCostBreakdown {
    pub provider: String,
    pub model: String,
    pub total_tokens: u64,
    pub total_cost: String,
    pub currency: String,
    pub items: Vec<UsageCostBreakdownItem>,
}

#[async_trait]
pub trait UsageLogRepo: Send + Sync {
    async fn insert_usage(
        &self,
        ctx: &RequestContext,
        usage: UsageRecord,
    ) -> UsageServiceResult<UsageRecord>;

    async fn aggregate_by_project_model_channel(
        &self,
        ctx: &RequestContext,
    ) -> UsageServiceResult<Vec<CostItem>>;
}

pub struct UsageLogService {
    repo: Arc<dyn UsageLogRepo>,
}

impl UsageLogService {
    pub fn new(repo: Arc<dyn UsageLogRepo>) -> Self {
        Self { repo }
    }

    pub async fn insert_usage(
        &self,
        ctx: &RequestContext,
        mut usage: UsageRecord,
    ) -> UsageServiceResult<UsageRecord> {
        apply_costs(&mut usage)?;
        self.repo.insert_usage(ctx, usage).await
    }

    pub async fn aggregate_by_project_model_channel(
        &self,
        ctx: &RequestContext,
    ) -> UsageServiceResult<Vec<CostItem>> {
        self.repo.aggregate_by_project_model_channel(ctx).await
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeUsageLogRepo {
    inner: Arc<Mutex<Vec<UsageRecord>>>,
}

impl FakeUsageLogRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_count(&self) -> UsageServiceResult<usize> {
        Ok(self.lock()?.len())
    }

    fn lock(&self) -> UsageServiceResult<std::sync::MutexGuard<'_, Vec<UsageRecord>>> {
        self.inner
            .lock()
            .map_err(|_| UsageServiceError::LockPoisoned)
    }
}

#[async_trait]
impl UsageLogRepo for FakeUsageLogRepo {
    async fn insert_usage(
        &self,
        _ctx: &RequestContext,
        usage: UsageRecord,
    ) -> UsageServiceResult<UsageRecord> {
        self.lock()?.push(usage.clone());
        Ok(usage)
    }

    async fn aggregate_by_project_model_channel(
        &self,
        _ctx: &RequestContext,
    ) -> UsageServiceResult<Vec<CostItem>> {
        let records = self.lock()?.clone();
        let mut aggregate = BTreeMap::new();

        for record in records {
            let key = (
                record.project_id.clone(),
                record.model.clone(),
                record.channel.clone(),
            );
            let item = aggregate.entry(key).or_insert_with(|| {
                CostItem::empty(
                    record.project_id.clone(),
                    record.model.clone(),
                    record.channel.clone(),
                )
            });

            item.prompt_tokens += record.prompt_tokens;
            item.completion_tokens += record.completion_tokens;
            item.total_tokens += record.total_tokens;
            item.prompt_cost =
                sum_decimal_strings("prompt_cost", &item.prompt_cost, &record.prompt_cost)?;
            item.completion_cost = sum_decimal_strings(
                "completion_cost",
                &item.completion_cost,
                &record.completion_cost,
            )?;
            item.total_cost =
                sum_decimal_strings("total_cost", &item.total_cost, &record.total_cost)?;
        }

        Ok(aggregate.into_values().collect())
    }
}

fn apply_costs(usage: &mut UsageRecord) -> UsageServiceResult<()> {
    let prompt_unit_price = parse_decimal("prompt_unit_price", &usage.prompt_unit_price)?;
    let completion_unit_price =
        parse_decimal("completion_unit_price", &usage.completion_unit_price)?;

    // Prices are stored as string decimals per token; this keeps calculation
    // free of binary floating-point rounding before real billing storage exists.
    let prompt_cost = prompt_unit_price * Decimal::from(usage.prompt_tokens);
    let completion_cost = completion_unit_price * Decimal::from(usage.completion_tokens);
    let total_cost = prompt_cost + completion_cost;

    usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
    usage.prompt_cost = decimal_to_string(prompt_cost);
    usage.completion_cost = decimal_to_string(completion_cost);
    usage.total_cost = decimal_to_string(total_cost);
    Ok(())
}

pub fn build_usage_cost_breakdown(
    tokens: UsageCostBreakdownTokens,
    rates: &UsageCostBreakdownRates,
) -> UsageServiceResult<UsageCostBreakdown> {
    let denominator = price_unit_denominator(rates.unit);
    let mut total_cost = Decimal::ZERO;
    let mut items = Vec::with_capacity(5);

    for bucket in [
        UsageCostBucket::Prompt,
        UsageCostBucket::Completion,
        UsageCostBucket::Cache,
        UsageCostBucket::Audio,
        UsageCostBucket::Reasoning,
    ] {
        let token_count = tokens.tokens_for(bucket);
        let unit_price = rates.price_for(bucket);
        let cost = parse_decimal(bucket.price_field(), unit_price)? * Decimal::from(token_count)
            / denominator;
        total_cost += cost;

        items.push(UsageCostBreakdownItem {
            bucket,
            tokens: token_count,
            unit_price: unit_price.to_string(),
            cost: decimal_to_string(cost),
        });
    }

    Ok(UsageCostBreakdown {
        provider: rates.provider.clone(),
        model: rates.model.clone(),
        total_tokens: tokens.total_tokens(),
        total_cost: decimal_to_string(total_cost),
        currency: rates.currency.clone(),
        items,
    })
}

fn price_unit_denominator(unit: PriceUnit) -> Decimal {
    match unit {
        PriceUnit::PerToken => Decimal::ONE,
        PriceUnit::PerThousandTokens => Decimal::from(1_000_u64),
        PriceUnit::PerMillionTokens => Decimal::from(1_000_000_u64),
    }
}

fn sum_decimal_strings(field: &'static str, left: &str, right: &str) -> UsageServiceResult<String> {
    Ok(decimal_to_string(
        parse_decimal(field, left)? + parse_decimal(field, right)?,
    ))
}

fn parse_decimal(field: &'static str, value: &str) -> UsageServiceResult<Decimal> {
    Decimal::from_str(value).map_err(|_| UsageServiceError::InvalidDecimal {
        field,
        value: value.to_string(),
    })
}

fn decimal_to_string(value: Decimal) -> String {
    value.normalize().to_string()
}

// ============================================================================
// Cost calculation — pure logic ported from `conduit/internal/server/biz/cost_calc.go`
// and `conduit/internal/objects/price.go`. S04/S05/S06/S08 of RUST-P10-002.
//
// Money math stays in `rust_decimal::Decimal` end-to-end; the Go code uses
// `shopspring/decimal`. The million-token divisor mirrors Go's
// `unitsInMillionTokens(units int64) decimal.Decimal = decimal.New(units) / 1e6`.
// ============================================================================

/// Per-item cost subtotal plus an optional tier breakdown. Mirrors the Go
/// `objects.CostItem` shape produced by `computeItemSubtotal`.
///
/// Note: Go also stamps `item.ItemCode` / `item.PromptWriteCacheVariantCode`
/// onto the item *after* `computeItemSubtotal` returns (see `ComputeUsageCost`);
/// those fields are attached by the caller of [`compute_item_subtotal`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CostItemDetail {
    /// Quantity of billable units (tokens) used for this item.
    #[serde(default)]
    pub quantity: i64,
    /// Subtotal for this item. Always populated on return.
    #[serde(default)]
    pub subtotal: Decimal,
    /// Tier breakdown for tiered / volume modes; empty for flat-fee / per-unit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tier_breakdown: Vec<TierCostDetail>,
}

/// One tier's contribution to a tiered subtotal. Mirrors Go `objects.TierCost`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TierCostDetail {
    /// Upper bound of this tier (`None` = open-ended final tier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to: Option<i64>,
    /// Number of units billed in this tier.
    #[serde(default)]
    pub units: i64,
    /// Subtotal contributed by this tier.
    #[serde(default)]
    pub subtotal: Decimal,
}

/// Tag applied to a computed [`CostItemDetail`] to identify which price item /
/// write-cache variant it belongs to. Mirrors the Go post-processing inside
/// `ComputeUsageCost`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComputedCostItem {
    /// Mirrors Go `CostItem.ItemCode`.
    #[serde(default)]
    pub item_code: String,
    /// Mirrors Go `CostItem.PromptWriteCacheVariantCode`. `None` for non-variant items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_write_cache_variant_code: Option<String>,
    /// The per-item quantity/subtotal/tier-breakdown detail.
    #[serde(default, flatten)]
    pub detail: CostItemDetail,
}

/// Result of [`compute_usage_cost`]: the per-item breakdown plus the grand total.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ComputeUsageCostResult {
    /// One entry per processed price item (write-cache variants produce their
    /// own entries — see S06).
    pub items: Vec<ComputedCostItem>,
    /// Sum of every item's subtotal. Mirrors Go `ComputeUsageCost`'s returned
    /// `total decimal.Decimal`.
    pub total: Decimal,
}

/// (S04) Compute one price item's subtotal from a quantity and a [`Pricing`]
/// config, dispatching across the four Go pricing modes.
///
/// Pure port of Go `computeItemSubtotal(quantity int64, pricing objects.Pricing)`.
/// Returns `Default::default()` (zero subtotal) when the pricing branch is
/// missing its required data (e.g. `flat_fee` with `flat_fee == None`), exactly
/// matching Go's `return item, decimal.Zero` fallbacks.
pub fn compute_item_subtotal(quantity: i64, pricing: &Pricing) -> CostItemDetail {
    match pricing.mode.as_str() {
        PRICING_MODE_FLAT_FEE => match pricing.flat_fee {
            Some(fee) => CostItemDetail {
                // A flat fee is charged once for the successful request. Its
                // source item may be a token code for compatibility with the
                // original Conduit API schema, but displaying a zero-token
                // quantity here is misleading: the billable unit is one
                // request, independent of token usage.
                quantity: 1,
                subtotal: fee,
                tier_breakdown: Vec::new(),
            },
            None => CostItemDetail::default(),
        },
        PRICING_MODE_USAGE_PER_UNIT => match pricing.usage_per_unit {
            Some(per_unit) => {
                let sub = per_unit * units_in_million_tokens(quantity);
                CostItemDetail {
                    quantity,
                    subtotal: sub,
                    tier_breakdown: Vec::new(),
                }
            }
            None => CostItemDetail::default(),
        },
        PRICING_MODE_TIERED => match &pricing.usage_tiered {
            Some(tiered) => compute_tiered_subtotal(quantity, &tiered.tiers),
            None => CostItemDetail::default(),
        },
        PRICING_MODE_VOLUME => match &pricing.usage_tiered {
            Some(tiered) => compute_volume_subtotal(quantity, &tiered.tiers),
            None => CostItemDetail::default(),
        },
        // Unknown mode → Go's `default` arm returns zero.
        _ => CostItemDetail::default(),
    }
}

/// Walks tiers summing each segment's contribution. Mirrors Go's
/// `PricingModeTiered` branch.
fn compute_tiered_subtotal(
    quantity: i64,
    tiers: &[conduit_core::objects::pricing::PriceTier],
) -> CostItemDetail {
    let mut total = Decimal::ZERO;
    let mut tier_breakdown: Vec<TierCostDetail> = Vec::new();
    let mut prev_up_to: i64 = 0;

    for tier in tiers {
        let tier_units = match tier.up_to {
            Some(up_to) => {
                if quantity <= up_to {
                    (quantity - prev_up_to).max(0)
                } else {
                    (up_to - prev_up_to).max(0)
                }
            }
            None => (quantity - prev_up_to).max(0),
        };

        if tier_units <= 0 {
            // Mirror Go's break / continue semantics inside the tier loop.
            if matches!(tier.up_to, Some(up_to) if quantity <= up_to) {
                break;
            }
            prev_up_to = get_up_to_or_zero(tier.up_to);
            continue;
        }

        let sub = tier.price_per_unit * units_in_million_tokens(tier_units);
        total += sub;
        tier_breakdown.push(TierCostDetail {
            up_to: tier.up_to,
            units: tier_units,
            subtotal: sub,
        });
        prev_up_to = get_up_to_or_zero(tier.up_to);

        if matches!(tier.up_to, Some(up_to) if quantity <= up_to) {
            break;
        }
    }

    CostItemDetail {
        quantity,
        subtotal: total,
        tier_breakdown,
    }
}

/// Finds the first tier matching the total quantity and bills ALL tokens at that
/// tier's price. Mirrors Go's `PricingModeVolume` branch.
fn compute_volume_subtotal(
    quantity: i64,
    tiers: &[conduit_core::objects::pricing::PriceTier],
) -> CostItemDetail {
    let matched = tiers.iter().find(|tier| match tier.up_to {
        Some(up_to) => quantity <= up_to,
        None => true, // open-ended tier always matches.
    });

    match matched {
        Some(tier) => {
            let sub = tier.price_per_unit * units_in_million_tokens(quantity);
            CostItemDetail {
                quantity,
                subtotal: sub,
                tier_breakdown: vec![TierCostDetail {
                    up_to: tier.up_to,
                    units: quantity,
                    subtotal: sub,
                }],
            }
        }
        None => CostItemDetail::default(),
    }
}

/// Mirrors Go `getUpToOrZero(v *int64) int64`.
fn get_up_to_or_zero(v: Option<i64>) -> i64 {
    v.unwrap_or(0)
}

/// Mirrors Go `unitsInMillionTokens`: `quantity <= 0 → 0`, else
/// `Decimal::from(quantity) / 1_000_000`. The division is exact (no rounding)
/// because `rust_decimal` preserves arbitrary precision.
fn units_in_million_tokens(units: i64) -> Decimal {
    if units <= 0 {
        return Decimal::ZERO;
    }
    Decimal::from(units) / million_tokens_divisor()
}

/// (S05) Return the billable prompt-token count for a [`Usage`], i.e. the
/// prompt tokens with cached + write-cached variants subtracted, clamped to
/// `>= 0`.
///
/// Mirrors the `PriceItemCodeUsage` quantity logic in Go `ComputeUsageCost`
/// (lines 155-163 of `cost_calc.go`):
/// ```text
/// quantity = usage.PromptTokens
/// quantity -= PromptTokensDetails.CachedTokens
/// quantity -= PromptTokensDetails.WriteCachedTokens
/// if quantity < 0 { quantity = 0 }
/// ```
///
/// Note: Go subtracts only `CachedTokens` and `WriteCachedTokens` here — the
/// `WriteCached5MinTokens` / `WriteCached1HourTokens` variants are NOT
/// subtracted at this step because Go enters a separate `continue`d branch
/// for them (handled by [`compute_usage_cost`]). This Rust helper mirrors
/// that exact behavior to preserve parity.
pub fn billable_prompt_tokens(usage: &Usage) -> i64 {
    let prompt = usage.prompt_tokens as i64;
    let cached = usage.prompt_details.cached_tokens as i64;
    let write_cached = usage.prompt_details.write_cached_tokens as i64;
    (prompt - cached - write_cached).max(0)
}

/// (S06) Resolve the [`Pricing`] to apply for a given prompt-write-cache
/// variant code on a price item. If a matching variant is configured, return
/// its pricing; otherwise fall back to the item's base pricing.
///
/// Pure port of Go `ModelPriceItem.FindPromptWriteCacheVariantPricing(variantCode) Pricing`.
pub fn select_write_cached_price<'a>(variant_code: &str, item: &'a ModelPriceItem) -> &'a Pricing {
    // `PromptWriteCacheVariantCode` is a `String` alias; the Go contract only
    // compares by value, so a `&str` parameter keeps the call sites ergonomic.
    for variant in &item.prompt_write_cache_variants {
        if variant.variant_code == variant_code {
            return &variant.pricing;
        }
    }
    &item.pricing
}

/// (S08) Pure aggregation of usage-log numeric fields. Sums
/// `prompt_tokens` / `completion_tokens` / `total_tokens` (saturating) and
/// `prompt_cost` / `completion_cost` / `total_cost` (as `Decimal`). Mirrors
/// the field-by-field accumulation that Go performs when rolling up
/// `CostItem`s / `UsageLog` rows (see `aggregate_by_project_model_channel`).
///
/// `T: UsageLogRow` decouples this from any concrete row type — both the
/// service-layer `UsageRecord` and the DB `CostItem` can feed it.
pub fn aggregate_usage<T>(rows: &[T]) -> UsageServiceResult<UsageTotals>
where
    T: UsageLogRow,
{
    let mut totals = UsageTotals::default();
    for row in rows {
        totals.prompt_tokens = totals.prompt_tokens.saturating_add(row.prompt_tokens());
        totals.completion_tokens = totals
            .completion_tokens
            .saturating_add(row.completion_tokens());
        totals.total_tokens = totals.total_tokens.saturating_add(row.total_tokens());
        totals.cached_tokens = totals.cached_tokens.saturating_add(row.cached_tokens());
        totals.write_cached_tokens = totals
            .write_cached_tokens
            .saturating_add(row.write_cached_tokens());
        totals.prompt_cost += parse_decimal("prompt_cost", row.prompt_cost())?;
        totals.completion_cost += parse_decimal("completion_cost", row.completion_cost())?;
        totals.total_cost += parse_decimal("total_cost", row.total_cost())?;
    }
    Ok(totals)
}

/// Trait abstracting any row that can be aggregated by [`aggregate_usage`].
/// Implemented for the service-layer [`UsageRecord`].
pub trait UsageLogRow {
    fn prompt_tokens(&self) -> u64;
    fn completion_tokens(&self) -> u64;
    fn total_tokens(&self) -> u64;
    fn cached_tokens(&self) -> u64 {
        0
    }
    fn write_cached_tokens(&self) -> u64 {
        0
    }
    fn prompt_cost(&self) -> &str;
    fn completion_cost(&self) -> &str;
    fn total_cost(&self) -> &str;
}

impl UsageLogRow for UsageRecord {
    fn prompt_tokens(&self) -> u64 {
        self.prompt_tokens
    }
    fn completion_tokens(&self) -> u64 {
        self.completion_tokens
    }
    fn total_tokens(&self) -> u64 {
        self.total_tokens
    }
    fn prompt_cost(&self) -> &str {
        &self.prompt_cost
    }
    fn completion_cost(&self) -> &str {
        &self.completion_cost
    }
    fn total_cost(&self) -> &str {
        &self.total_cost
    }
}

/// Sum of a batch of usage rows. Returned by [`aggregate_usage`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTotals {
    /// Saturated sum of `prompt_tokens`.
    pub prompt_tokens: u64,
    /// Saturated sum of `completion_tokens`.
    pub completion_tokens: u64,
    /// Saturated sum of `total_tokens`.
    pub total_tokens: u64,
    /// Saturated sum of cached prompt tokens (zero if the row type lacks them).
    pub cached_tokens: u64,
    /// Saturated sum of write-cached prompt tokens (zero if the row type lacks them).
    pub write_cached_tokens: u64,
    /// Decimal sum of `prompt_cost` strings.
    pub prompt_cost: Decimal,
    /// Decimal sum of `completion_cost` strings.
    pub completion_cost: Decimal,
    /// Decimal sum of `total_cost` strings.
    pub total_cost: Decimal,
}

// ============================================================================
// RUST-P10-002 S11 — price lookup → usage cost binding, with no-error fallback.
//
// Mirrors Go `biz/usage_log.go::(*UsageLogService).computeUsageCost` lines
// 29-70. The Go function returns `(items []objects.CostItem, totalCost
// *float64, priceReferenceID string)` and NEVER returns an error — every
// "price not found" / "channel disabled" / "usage nil" path returns the
// zero tuple `(nil, nil, "")`. The caller (`CreateUsageLog`) then sets the
// row fields via `SetNillableTotalCost(nil)` + `SetCostItems(nil)` and only
// conditionally writes `cost_price_reference_id` (Go `if priceReferenceID !=
// ""`), so the request continues unblocked with an empty/zero-cost usage
// log row. This is the S11 "must not error / must not block" contract.
//
// Rust port: the pure helper [`compute_usage_cost_with_reference`] takes an
// optional [`ResolvedModelPrice`] (the equivalent of Go's
// `ch.cachedModelPrices[modelID]` lookup) and an optional `&Usage`, and
// returns a [`UsageCostComputation`] that already matches Go's three
// fallback shapes. No `Result` — the lookup-miss IS the contract, not an
// error. The HTTP/service layer's responsibility is to feed it the lookup
// result; this function decides what to persist.
// ============================================================================

/// Resolved model price + the reference id of the price-list version it came
/// from. Mirrors the Go `ch.cachedModelPrices[modelID]` cache entry shape
/// (`*Channel` holds `cachedModelPrices map[string]*cachedModelPrice` where
/// `cachedModelPrice{ Price objects.ModelPrice; ReferenceID string }`). The
/// Rust port keeps the same field names so the parity is direct.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModelPrice<'a> {
    /// The model price list (Go: `objects.ModelPrice`).
    pub price: &'a conduit_core::objects::pricing::ModelPrice,
    /// Reference id of the channel model price version (Go:
    /// `cachedModelPrice.ReferenceID`). Empty string when the source price
    /// list has no version tracking — Go then skips writing it.
    pub reference_id: &'a str,
}

/// Pure result of [`compute_usage_cost_with_reference`]. Mirrors Go's
/// `(items []objects.CostItem, totalCost *float64, priceReferenceID string)`
/// triple, mapped to the Rust cost types. `None`/empty fields reproduce the
/// Go fallback shape exactly.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageCostComputation {
    /// Computed cost-item breakdown. Empty when no price was resolved (Go
    /// returns `nil` items) — `Vec`'s empty form is the Rust equivalent.
    pub items: Vec<ComputedCostItem>,
    /// Full-precision decimal total. `None` when no price was resolved (Go
    /// returns `totalCost *float64 = nil`); `Some(total)` when the price hit.
    /// Note: Go immediately converts this to `float64` via `InexactFloat64`
    /// at the persistence boundary — see [`UsageLog::total_cost_as_f64`].
    pub total: Option<Decimal>,
    /// Price reference id. Empty string when no price was resolved (Go
    /// returns `""`). Non-empty only when a price was resolved AND the price
    /// list carried a reference id.
    pub reference_id: String,
}

impl UsageCostComputation {
    /// The Go "no-op fallback" shape — `(nil, nil, "")`. Returned for every
    /// price-miss / usage-nil / channel-disabled path. The caller persists a
    /// usage-log row with zero-cost fields, never errors.
    pub fn no_cost() -> Self {
        Self::default()
    }

    /// True when this computation carries no cost (Go's `totalCost == nil &&
    /// len(items) == 0 && priceReferenceID == ""` shape).
    pub fn is_no_cost(&self) -> bool {
        self.total.is_none() && self.items.is_empty() && self.reference_id.is_empty()
    }
}

/// (S11) Pure port of Go `(*UsageLogService).computeUsageCost`. Given the
/// optional resolved model price (the lookup result) and the optional LLM
/// usage, produce the cost computation that should be persisted on the
/// usage-log row.
///
/// **Never returns an error** — the lookup-miss IS the contract, mirroring
/// Go's three no-error fallback paths:
///   1. `usage.is_none()`             → Go line 30-32 → `no_cost()`.
///   2. `resolved_price.is_none()`    → Go line 35-42 + 69 → `no_cost()`
///      (Go logs a `Warn` for the channel-disabled case; the Rust caller
///      owns logging — this pure helper just returns the zero shape).
///   3. price hit                      → Go line 52-67 → cost computed via
///      [`compute_usage_cost_full`], `Some(total)`, the items, and the
///      resolved `reference_id`.
///
/// `apply_to(log)` then folds the result onto a [`UsageLog`] row via the
/// already-implemented [`UsageLog::with_cost`], reproducing Go's
/// `SetNillableTotalCost` + `SetCostItems` + conditional
/// `SetCostPriceReferenceID` tail. Use [`Self::apply_to`] for the full
/// Go `CreateUsageLog` end-to-end shape, or read the fields directly.
pub fn compute_usage_cost_with_reference<'a>(
    usage: Option<&Usage>,
    resolved_price: Option<ResolvedModelPrice<'a>>,
) -> UsageCostComputation {
    // Path 1: no usage data — Go returns `(nil, nil, "")`.
    let Some(usage) = usage else {
        return UsageCostComputation::no_cost();
    };
    // Path 2: price not resolved (channel disabled OR model not in price cache)
    // — Go returns `(nil, nil, "")` WITHOUT erroring.
    let Some(resolved) = resolved_price else {
        return UsageCostComputation::no_cost();
    };
    // Path 3: price hit — Go runs `ComputeUsageCost(usage, modelPrice.Price)`
    // and returns `(items, lo.ToPtr(totalCost), modelPrice.ReferenceID)`.
    let result = compute_usage_cost_full(Some(usage), resolved.price);
    UsageCostComputation {
        items: result.items,
        total: Some(result.total),
        reference_id: resolved.reference_id.to_string(),
    }
}

impl UsageCostComputation {
    /// (S11) Fold this computation onto a [`UsageLog`] row, mirroring the
    /// Go tail of `CreateUsageLog`:
    /// ```text
    /// mut = mut.SetNillableTotalCost(totalCost).SetCostItems(costItems)
    /// if priceReferenceID != "" { mut = mut.SetCostPriceReferenceID(priceReferenceID) }
    /// ```
    /// On a no-cost fallback, this leaves `total_cost` / `cost_items` /
    /// `cost_price_reference_id` at their Go zero state (`None` / empty /
    /// `None`) — the request continues unblocked.
    pub fn apply_to(self, mut log: UsageLog) -> UsageLog {
        match self.total {
            None => {
                // Go fallback: SetNillableTotalCost(nil) + SetCostItems(nil);
                // SetCostPriceReferenceID skipped for "".
                log.total_cost = None;
                log.cost_items = Vec::new();
                log.cost_price_reference_id = None;
            }
            Some(total) => {
                // Go hit path: reuses the existing `with_cost` helper (S07),
                // which already implements the `if reference_id != ""` guard.
                log = log.with_cost(total, self.items, self.reference_id);
            }
        }
        log
    }
}

// ============================================================================
// RUST-P10-002 S14 — architectural constraint: UsageLogService consumes ONLY
// structured `llm::Usage`, never raw provider bodies.
//
// Go reference (`internal/server/biz/usage_log.go`):
//   * `CreateUsageLogParams.Usage` is typed `*llm.Usage` — a structured Go
//     struct, NOT a raw JSON body. Every field on the UsageLog row is read
//     off `params.Usage.PromptTokens` / `.PromptTokensDetails.CachedTokens`
//     / `.CompletionTokensDetails.AudioTokens` etc.
//   * The provider-response → `*llm.Usage` conversion lives in the
//     transformer/pipeline layer (`llm/pipeline/`, `llm/transformer/*`),
//     NOT in `biz/usage_log.go`. The Go `UsageLogService` has no
//     `json.Unmarshal` of any provider body anywhere in its surface.
//
// This Rust module MUST honor the same constraint: the service layer
// receives already-parsed `conduit_llm::Usage` values and is forbidden from
// parsing provider JSON bodies to extract token counts. The helpers below
// (`CreateUsageLogParams` + `create_usage_log_from_structured_usage`)
// reproduce the Go `CreateUsageLog` flow at the pure layer and exist partly
// to *document* the structured-input contract: their signatures take
// `&Usage`, never `&[u8]` / `serde_json::Value` / `&str` bodies. Any future
// body-parsing path must live in a transformer, not here.
// ============================================================================

/// Structured parameters for building a usage-log row. Pure Rust port of Go
/// `biz/usage_log.go::CreateUsageLogParams`. The `.usage` field is
/// `&llm::Usage` — already structured by the transformer/pipeline layer;
/// this type intentionally offers NO field for a raw provider body (S14).
#[derive(Debug, Clone)]
pub struct CreateUsageLogParams<'a> {
    pub request_id: i64,
    pub project_id: i64,
    pub channel_id: Option<i64>,
    /// The channel actual model id, NOT the request model id (mirrors Go
    /// `ActualModelID`).
    pub actual_model_id: &'a str,
    /// Already-parsed LLM usage. S14: this MUST be the structured form —
    /// provider JSON body parsing is the transformer layer's responsibility.
    pub usage: &'a Usage,
    pub source: UsageLogSource,
    pub format: &'a str,
    pub api_key_id: Option<i64>,
    /// Optional resolved model price (the lookup result the caller already
    /// performed). When `None`, the no-cost fallback applies (S11) — the
    /// request continues unblocked.
    pub resolved_price: Option<ResolvedModelPrice<'a>>,
}

impl<'a> CreateUsageLogParams<'a> {
    /// Convenience constructor mirroring Go's positional struct literal.
    /// `resolved_price` defaults to `None` (no-cost fallback) — the caller
    /// typically chains `.with_resolved_price(...)` after the lookup.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request_id: i64,
        project_id: i64,
        channel_id: Option<i64>,
        actual_model_id: &'a str,
        usage: &'a Usage,
        source: UsageLogSource,
        format: &'a str,
        api_key_id: Option<i64>,
    ) -> Self {
        Self {
            request_id,
            project_id,
            channel_id,
            actual_model_id,
            usage,
            source,
            format,
            api_key_id,
            resolved_price: None,
        }
    }

    /// Attach the resolved model price after the caller's lookup. Mirrors
    /// Go's flow where `computeUsageCost` is called *inside* `CreateUsageLog`
    /// after the channel/price cache lookup — here we split it so the price
    /// resolution stays pluggable (cache, DB, test double).
    pub fn with_resolved_price(mut self, resolved: ResolvedModelPrice<'a>) -> Self {
        self.resolved_price = Some(resolved);
        self
    }
}

/// (S14) Build a fully-populated [`UsageLog`] row from STRUCTURED inputs.
/// Mirrors Go `biz/usage_log.go::(*UsageLogService).CreateUsageLog` at the
/// pure layer: populate identity/token fields from `params.usage` (already
/// structured by the transformer), compute cost via
/// [`compute_usage_cost_with_reference`] (S11), and fold the result onto the
/// row via [`UsageCostComputation::apply_to`] (S07/S10/S11 tail).
///
/// **S14 contract**: this function takes `&Usage` and `Option<&str>` model
/// ids — it has NO parameter for a raw provider JSON body. Body → `Usage`
/// parsing is the transformer/pipeline layer's job; this function refuses to
/// participate in it. The signature is the executable contract.
///
/// Returns the populated row (caller then persists via the repo). The Go
/// equivalent persists inside the function; the Rust split keeps the pure
/// construction testable without a DB.
pub fn create_usage_log_from_structured_usage(params: CreateUsageLogParams<'_>) -> UsageLog {
    // 1. Build the row skeleton from structured `Usage`. No body parsing
    //    happens here — `from_usage` reads only typed `Usage` fields.
    let log = UsageLog::from_usage(
        params.request_id,
        params.project_id,
        params.channel_id,
        params.actual_model_id,
        params.source,
        params.format,
        params.api_key_id,
        params.usage,
    );
    // 2. Compute the cost from the (optional) resolved price + the SAME
    //    structured `Usage`. S11 fallback: price miss → no-cost row, no
    //    error.
    let computation =
        compute_usage_cost_with_reference(Some(params.usage), params.resolved_price.clone());
    // 3. Fold the computation onto the row (S07/S10/S11 tail — mirrors Go's
    //    `SetNillableTotalCost` / `SetCostItems` / conditional
    //    `SetCostPriceReferenceID`).
    computation.apply_to(log)
}

/// Entry point mirroring Go `ComputeUsageCost(usage *llm.Usage, price objects.ModelPrice)`.
///
/// Walks each price item, computes the quantity per Go's item-code switch
/// (S05 clamp for `PriceItemCodeUsage`, S06 variant handling for
/// `PriceItemCodeWriteCachedTokens`), dispatches to [`compute_item_subtotal`]
/// (S04 four-mode logic), and accumulates the grand total. Returns
/// `Default::default()` when `usage` is `None` (Go returns `nil, nil, ""`).
pub fn compute_usage_cost_full(
    usage: Option<&Usage>,
    price: &ModelPrice,
) -> ComputeUsageCostResult {
    let Some(usage) = usage else {
        return ComputeUsageCostResult::default();
    };

    let mut items: Vec<ComputedCostItem> = Vec::new();
    let mut total = Decimal::ZERO;

    for item in &price.items {
        match item.item_code.as_str() {
            price_item_code::USAGE => {
                let quantity = billable_prompt_tokens(usage);
                push_item(&mut items, &mut total, item, &item.pricing, quantity, None);
            }
            price_item_code::COMPLETION => {
                let quantity = usage.completion_tokens as i64;
                push_item(&mut items, &mut total, item, &item.pricing, quantity, None);
            }
            price_item_code::PROMPT_CACHED_TOKEN => {
                let quantity = usage.prompt_details.cached_tokens as i64;
                push_item(&mut items, &mut total, item, &item.pricing, quantity, None);
            }
            price_item_code::WRITE_CACHED_TOKENS => {
                let five_min = usage.prompt_details.write_cached_tokens_5m;
                let one_hour = usage.prompt_details.write_cached_tokens_1h;
                if five_min > 0 || one_hour > 0 {
                    if five_min > 0 {
                        let pricing = select_write_cached_price(
                            prompt_write_cache_variant_code::FIVE_MIN,
                            item,
                        );
                        push_item(
                            &mut items,
                            &mut total,
                            item,
                            pricing,
                            five_min as i64,
                            Some(prompt_write_cache_variant_code::FIVE_MIN.to_string()),
                        );
                    }
                    if one_hour > 0 {
                        let pricing = select_write_cached_price(
                            prompt_write_cache_variant_code::ONE_HOUR,
                            item,
                        );
                        push_item(
                            &mut items,
                            &mut total,
                            item,
                            pricing,
                            one_hour as i64,
                            Some(prompt_write_cache_variant_code::ONE_HOUR.to_string()),
                        );
                    }
                    // Go `continue`s, skipping the shared-pricing fallback.
                    continue;
                }
                let quantity = usage.prompt_details.write_cached_tokens as i64;
                push_item(&mut items, &mut total, item, &item.pricing, quantity, None);
            }
            // Unknown item code → Go's `default` arm sets quantity = 0.
            _ => push_item(&mut items, &mut total, item, &item.pricing, 0, None),
        }
    }

    ComputeUsageCostResult { items, total }
}

/// Helper that computes a single item's subtotal and stamps it onto the running
/// result list. Mirrors the tail of Go's `ComputeUsageCost` loop body
/// (`item.ItemCode = ...; items = append(...); total = total.Add(sub)`).
fn push_item(
    items: &mut Vec<ComputedCostItem>,
    total: &mut Decimal,
    _item: &ModelPriceItem,
    pricing: &Pricing,
    quantity: i64,
    variant_code: Option<String>,
) {
    let detail = compute_item_subtotal(quantity, pricing);
    *total += detail.subtotal;
    items.push(ComputedCostItem {
        item_code: _item.item_code.clone(),
        prompt_write_cache_variant_code: variant_code,
        detail,
    });
}

#[cfg(test)]
mod tests {
    use conduit_db::{PolicyContext, Principal, RequestContext};
    use conduit_llm::TokenDetails;

    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    #[test]
    fn flat_fee_cost_detail_represents_one_successful_request() {
        let pricing = Pricing {
            mode: PRICING_MODE_FLAT_FEE.to_string(),
            flat_fee: Some(Decimal::new(5, 2)),
            ..Pricing::default()
        };

        let detail = compute_item_subtotal(0, &pricing);

        assert_eq!(detail.quantity, 1);
        assert_eq!(detail.subtotal, Decimal::new(5, 2));
    }

    #[tokio::test]
    async fn insert_usage_calculates_decimal_safe_costs() -> UsageServiceResult<()> {
        let repo = Arc::new(FakeUsageLogRepo::new());
        let service = UsageLogService::new(repo.clone());
        let ctx = ctx();

        let saved = service
            .insert_usage(
                &ctx,
                UsageRecord::new(
                    "usage-1",
                    "project-a",
                    "gpt-test",
                    "openai",
                    12,
                    8,
                    "0.000001",
                    "0.000002",
                ),
            )
            .await?;

        assert_eq!(repo.record_count()?, 1);
        assert_eq!(saved.total_tokens, 20);
        assert_eq!(saved.prompt_cost, "0.000012");
        assert_eq!(saved.completion_cost, "0.000016");
        assert_eq!(saved.total_cost, "0.000028");
        Ok(())
    }

    #[tokio::test]
    async fn aggregate_groups_by_project_model_channel_and_sums_costs() -> UsageServiceResult<()> {
        let repo = Arc::new(FakeUsageLogRepo::new());
        let service = UsageLogService::new(repo);
        let ctx = ctx();

        service
            .insert_usage(
                &ctx,
                UsageRecord::new(
                    "usage-1",
                    "project-a",
                    "gpt-test",
                    "openai",
                    10,
                    5,
                    "0.001",
                    "0.002",
                ),
            )
            .await?;
        service
            .insert_usage(
                &ctx,
                UsageRecord::new(
                    "usage-2",
                    "project-a",
                    "gpt-test",
                    "openai",
                    7,
                    3,
                    "0.001",
                    "0.002",
                ),
            )
            .await?;
        service
            .insert_usage(
                &ctx,
                UsageRecord::new(
                    "usage-3",
                    "project-a",
                    "gpt-other",
                    "openai",
                    1,
                    2,
                    "0.003",
                    "0.004",
                ),
            )
            .await?;

        let aggregate = service.aggregate_by_project_model_channel(&ctx).await?;

        assert_eq!(aggregate.len(), 2);
        assert_eq!(
            aggregate[0],
            CostItem {
                project_id: "project-a".to_string(),
                model: "gpt-other".to_string(),
                channel: "openai".to_string(),
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
                prompt_cost: "0.003".to_string(),
                completion_cost: "0.008".to_string(),
                total_cost: "0.011".to_string(),
            }
        );
        assert_eq!(
            aggregate[1],
            CostItem {
                project_id: "project-a".to_string(),
                model: "gpt-test".to_string(),
                channel: "openai".to_string(),
                prompt_tokens: 17,
                completion_tokens: 8,
                total_tokens: 25,
                prompt_cost: "0.017".to_string(),
                completion_cost: "0.016".to_string(),
                total_cost: "0.033".to_string(),
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn invalid_decimal_rate_is_rejected() {
        let repo = Arc::new(FakeUsageLogRepo::new());
        let service = UsageLogService::new(repo);
        let ctx = ctx();

        let err = service
            .insert_usage(
                &ctx,
                UsageRecord::new(
                    "usage-1",
                    "project-a",
                    "gpt-test",
                    "openai",
                    1,
                    1,
                    "not-decimal",
                    "0.002",
                ),
            )
            .await;

        assert!(matches!(
            err,
            Err(UsageServiceError::InvalidDecimal {
                field: "prompt_unit_price",
                value,
            }) if value == "not-decimal"
        ));
    }

    #[test]
    fn cost_breakdown_builds_items_for_each_token_bucket() -> UsageServiceResult<()> {
        let rates = UsageCostBreakdownRates::new(
            "openai",
            "gpt-test",
            "1",
            "2",
            "0.25",
            "4",
            "8",
            PriceUnit::PerThousandTokens,
            "USD",
        );
        let tokens = UsageCostBreakdownTokens::new(1_000, 500, 200, 25, 10);

        let breakdown = build_usage_cost_breakdown(tokens, &rates)?;

        assert_eq!(
            breakdown,
            UsageCostBreakdown {
                provider: "openai".to_string(),
                model: "gpt-test".to_string(),
                total_tokens: 1_735,
                total_cost: "2.23".to_string(),
                currency: "USD".to_string(),
                items: vec![
                    UsageCostBreakdownItem {
                        bucket: UsageCostBucket::Prompt,
                        tokens: 1_000,
                        unit_price: "1".to_string(),
                        cost: "1".to_string(),
                    },
                    UsageCostBreakdownItem {
                        bucket: UsageCostBucket::Completion,
                        tokens: 500,
                        unit_price: "2".to_string(),
                        cost: "1".to_string(),
                    },
                    UsageCostBreakdownItem {
                        bucket: UsageCostBucket::Cache,
                        tokens: 200,
                        unit_price: "0.25".to_string(),
                        cost: "0.05".to_string(),
                    },
                    UsageCostBreakdownItem {
                        bucket: UsageCostBucket::Audio,
                        tokens: 25,
                        unit_price: "4".to_string(),
                        cost: "0.1".to_string(),
                    },
                    UsageCostBreakdownItem {
                        bucket: UsageCostBucket::Reasoning,
                        tokens: 10,
                        unit_price: "8".to_string(),
                        cost: "0.08".to_string(),
                    },
                ],
            }
        );
        Ok(())
    }

    #[test]
    fn cost_breakdown_tokens_can_be_derived_from_llm_usage_details() {
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 40,
            prompt_details: TokenDetails {
                cached_tokens: 10,
                write_cached_tokens: 2,
                write_cached_tokens_5m: 3,
                write_cached_tokens_1h: 4,
                audio_tokens: 5,
                reasoning_tokens: 6,
                ..TokenDetails::default()
            },
            completion_details: TokenDetails {
                audio_tokens: 7,
                reasoning_tokens: 8,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        assert_eq!(
            UsageCostBreakdownTokens::from_usage(&usage),
            UsageCostBreakdownTokens::new(100, 40, 19, 12, 14)
        );
    }

    #[test]
    fn cost_breakdown_rejects_invalid_bucket_price() {
        let rates = UsageCostBreakdownRates::new(
            "openai",
            "gpt-test",
            "1",
            "2",
            "bad-cache-price",
            "4",
            "8",
            PriceUnit::PerThousandTokens,
            "USD",
        );
        let tokens = UsageCostBreakdownTokens::new(1, 1, 1, 1, 1);

        let err = build_usage_cost_breakdown(tokens, &rates);

        assert!(matches!(
            err,
            Err(UsageServiceError::InvalidDecimal {
                field: "cache_price",
                value,
            }) if value == "bad-cache-price"
        ));
    }

    // =========================================================================
    // RUST-P10-002 S13 — dashboard timezone-aware calendar bucket tests.
    // Mirror the Go golden cases in `internal/pkg/xtime/time_test.go`:
    //   - `TestGetCalendarPeriods` (UTC, Wed / Mon / Sun / month-start /
    //     month-end-on-leap-year);
    //   - `TestGetCalendarPeriodsWithLocation` (Shanghai + New York same UTC
    //     instant, different local date);
    //   - `TestFormatUTCOffset` (+0, +8, +5:30, -5, -3:30).
    // The Go tests use `time.LoadLocation`; here we substitute the equivalent
    // `FixedOffset` (the workspace has no `chrono-tz` dependency). For the
    // golden-instant snapshots this is exact: the Go `Asia/Shanghai` zone at
    // every snapshot instant in `time_test.go` has a fixed +08:00 offset (no DST
    // there), and `America/New_York` is at EST (-05:00) for the 2024-01-17 case.
    // =========================================================================

    fn utc_dt(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        let nd = chrono::NaiveDate::from_ymd_opt(y, m, d)
            .and_then(|d| d.and_hms_opt(h, mi, s))
            .unwrap_or_else(|| panic!("invalid date {y}-{m}-{d}T{h}:{mi}:{s}"));
        DateTime::<Utc>::from_naive_utc_and_offset(nd, Utc)
    }

    fn offset(secs: i32) -> FixedOffset {
        FixedOffset::east_opt(secs).unwrap_or_else(|| panic!("invalid fixed offset seconds {secs}"))
    }

    #[test]
    fn calendar_periods_utc_midweek_wednesday() -> UsageServiceResult<()> {
        // Go: "Wednesday in UTC" — mockNow 2024-01-17 14:30:00 UTC.
        let now = utc_dt(2024, 1, 17, 14, 30, 0);
        let got = get_calendar_periods(now, offset(0))?;

        assert_eq!(got.today.start, utc_dt(2024, 1, 17, 0, 0, 0));
        assert_eq!(got.today.end, utc_dt(2024, 1, 18, 0, 0, 0));
        // Monday 2024-01-15 .. 2024-01-22.
        assert_eq!(got.this_week.start, utc_dt(2024, 1, 15, 0, 0, 0));
        assert_eq!(got.this_week.end, utc_dt(2024, 1, 22, 0, 0, 0));
        // Last week Monday 2024-01-08 .. 2024-01-15.
        assert_eq!(got.last_week.start, utc_dt(2024, 1, 8, 0, 0, 0));
        assert_eq!(got.last_week.end, utc_dt(2024, 1, 15, 0, 0, 0));
        // This month Jan 1 .. Feb 1.
        assert_eq!(got.this_month.start, utc_dt(2024, 1, 1, 0, 0, 0));
        assert_eq!(got.this_month.end, utc_dt(2024, 2, 1, 0, 0, 0));
        Ok(())
    }

    #[test]
    fn calendar_periods_utc_monday_is_week_start() -> UsageServiceResult<()> {
        // Go: "Monday (start of week) in UTC" — 2024-01-15 10:00:00.
        let now = utc_dt(2024, 1, 15, 10, 0, 0);
        let got = get_calendar_periods(now, offset(0))?;

        assert_eq!(got.today.start, utc_dt(2024, 1, 15, 0, 0, 0));
        assert_eq!(got.today.end, utc_dt(2024, 1, 16, 0, 0, 0));
        assert_eq!(got.this_week.start, utc_dt(2024, 1, 15, 0, 0, 0));
        assert_eq!(got.this_week.end, utc_dt(2024, 1, 22, 0, 0, 0));
        assert_eq!(got.last_week.start, utc_dt(2024, 1, 8, 0, 0, 0));
        assert_eq!(got.last_week.end, utc_dt(2024, 1, 15, 0, 0, 0));
        assert_eq!(got.this_month.start, utc_dt(2024, 1, 1, 0, 0, 0));
        assert_eq!(got.this_month.end, utc_dt(2024, 2, 1, 0, 0, 0));
        Ok(())
    }

    #[test]
    fn calendar_periods_utc_sunday_is_end_of_week() -> UsageServiceResult<()> {
        // Go: "Sunday (end of week) in UTC" — 2024-01-21 23:59:59.
        let now = utc_dt(2024, 1, 21, 23, 59, 59);
        let got = get_calendar_periods(now, offset(0))?;

        assert_eq!(got.today.start, utc_dt(2024, 1, 21, 0, 0, 0));
        assert_eq!(got.today.end, utc_dt(2024, 1, 22, 0, 0, 0));
        // Still inside this week (Mon 01-15 .. Mon 01-22).
        assert_eq!(got.this_week.start, utc_dt(2024, 1, 15, 0, 0, 0));
        assert_eq!(got.this_week.end, utc_dt(2024, 1, 22, 0, 0, 0));
        assert_eq!(got.last_week.start, utc_dt(2024, 1, 8, 0, 0, 0));
        assert_eq!(got.last_week.end, utc_dt(2024, 1, 15, 0, 0, 0));
        assert_eq!(got.this_month.start, utc_dt(2024, 1, 1, 0, 0, 0));
        assert_eq!(got.this_month.end, utc_dt(2024, 2, 1, 0, 0, 0));
        Ok(())
    }

    #[test]
    fn calendar_periods_utc_month_boundaries() -> UsageServiceResult<()> {
        // Go: "First day of month" — 2024-03-01.
        let now_mar1 = utc_dt(2024, 3, 1, 8, 0, 0);
        let got = get_calendar_periods(now_mar1, offset(0))?;
        assert_eq!(got.today.start, utc_dt(2024, 3, 1, 0, 0, 0));
        assert_eq!(got.today.end, utc_dt(2024, 3, 2, 0, 0, 0));
        // Monday 2024-02-26 .. 2024-03-04.
        assert_eq!(got.this_week.start, utc_dt(2024, 2, 26, 0, 0, 0));
        assert_eq!(got.this_week.end, utc_dt(2024, 3, 4, 0, 0, 0));
        assert_eq!(got.this_month.start, utc_dt(2024, 3, 1, 0, 0, 0));
        assert_eq!(got.this_month.end, utc_dt(2024, 4, 1, 0, 0, 0));

        // Go: "Last day of month" — 2024-02-29 (leap year).
        let now_feb29 = utc_dt(2024, 2, 29, 20, 0, 0);
        let got = get_calendar_periods(now_feb29, offset(0))?;
        assert_eq!(got.today.start, utc_dt(2024, 2, 29, 0, 0, 0));
        assert_eq!(got.today.end, utc_dt(2024, 3, 1, 0, 0, 0));
        assert_eq!(got.this_month.start, utc_dt(2024, 2, 1, 0, 0, 0));
        assert_eq!(got.this_month.end, utc_dt(2024, 3, 1, 0, 0, 0));
        Ok(())
    }

    #[test]
    fn calendar_periods_shanghai_vs_new_york_same_utc_instant() -> UsageServiceResult<()> {
        // Go: TestGetCalendarPeriodsWithLocation — 2024-01-17 14:30:00 UTC.
        // Shanghai (+08:00): local 2024-01-17 22:30 → same calendar day.
        // New York EST (-05:00): local 2024-01-17 09:30 → same calendar day.
        let now = utc_dt(2024, 1, 17, 14, 30, 0);

        let sh = get_calendar_periods(now, offset(8 * 3600))?;
        // Shanghai local-midnight 2024-01-17 = 2024-01-16 16:00:00 UTC.
        assert_eq!(sh.today.start, utc_dt(2024, 1, 16, 16, 0, 0));
        assert_eq!(sh.today.end, utc_dt(2024, 1, 17, 16, 0, 0));

        let ny = get_calendar_periods(now, offset(-5 * 3600))?;
        // New York local-midnight 2024-01-17 = 2024-01-17 05:00:00 UTC.
        assert_eq!(ny.today.start, utc_dt(2024, 1, 17, 5, 0, 0));
        assert_eq!(ny.today.end, utc_dt(2024, 1, 18, 5, 0, 0));
        Ok(())
    }

    #[test]
    fn calendar_periods_cross_day_boundary_in_offset() -> UsageServiceResult<()> {
        // Same UTC instant near midnight UTC must fall into DIFFERENT local
        // days for opposite-offset zones — this is the parity-critical case
        // the dashboard relies on.
        // 2024-01-17 23:30:00 UTC.
        let now = utc_dt(2024, 1, 17, 23, 30, 0);

        // Tokyo (+09:00): local 2024-01-18 08:30 → today boundary is 18th.
        let tokyo = get_calendar_periods(now, offset(9 * 3600))?;
        // Tokyo local-midnight 2024-01-18 = 2024-01-17 15:00:00 UTC.
        assert_eq!(tokyo.today.start, utc_dt(2024, 1, 17, 15, 0, 0));
        assert_eq!(tokyo.today.end, utc_dt(2024, 1, 18, 15, 0, 0));

        // Honolului (-10:00): local 2024-01-17 13:30 → today boundary is 17th.
        let hnl = get_calendar_periods(now, offset(-10 * 3600))?;
        // Honolulu local-midnight 2024-01-17 = 2024-01-17 10:00:00 UTC.
        assert_eq!(hnl.today.start, utc_dt(2024, 1, 17, 10, 0, 0));
        assert_eq!(hnl.today.end, utc_dt(2024, 1, 18, 10, 0, 0));
        // Now is strictly inside Honolulu's today window.
        assert!(hnl.today.contains(now));
        Ok(())
    }

    #[test]
    fn calendar_periods_half_open_interval() -> UsageServiceResult<()> {
        // Go: TestPeriodHalfOpenInterval — exactly midnight UTC.
        let now = utc_dt(2024, 1, 17, 0, 0, 0);
        let got = get_calendar_periods(now, offset(0))?;
        assert_eq!(got.today.start, now);
        // Start is inclusive, end is exclusive.
        assert!(got.today.contains(got.today.start));
        assert!(!got.today.contains(got.today.end));
        // Duration is exactly one day.
        assert_eq!(got.today.duration(), Duration::days(1));
        Ok(())
    }

    #[test]
    fn calendar_periods_cross_month_in_shanghai() -> UsageServiceResult<()> {
        // 2024-02-15 16:30:00 UTC = 2024-02-16 00:30 Shanghai — the local
        // calendar month is February; the next-month boundary is
        // Shanghai-local 2024-03-01 00:00 = 2024-02-29 16:00 UTC (2024 is leap).
        let now = utc_dt(2024, 2, 15, 16, 30, 0);
        let got = get_calendar_periods(now, offset(8 * 3600))?;
        assert_eq!(got.this_month.start, utc_dt(2024, 1, 31, 16, 0, 0));
        assert_eq!(got.this_month.end, utc_dt(2024, 2, 29, 16, 0, 0));
        Ok(())
    }

    #[test]
    fn format_utc_offset_matches_go_table() {
        // Go: TestFormatUTCOffset — table of fixed offsets → "+HH:MM".
        assert_eq!(format_utc_offset(offset(0)), "+00:00");
        assert_eq!(format_utc_offset(offset(8 * 3600)), "+08:00");
        assert_eq!(format_utc_offset(offset(5 * 3600 + 30 * 60)), "+05:30");
        assert_eq!(format_utc_offset(offset(-5 * 3600)), "-05:00");
        assert_eq!(format_utc_offset(offset(-3 * 3600 - 30 * 60)), "-03:30");
    }

    #[test]
    fn parse_time_window_filters_match_go_resolver() -> UsageServiceResult<()> {
        // Go: (*queryResolver).parseTimeWindow — same UTC instant, Shanghai offset.
        let now = utc_dt(2024, 1, 17, 14, 30, 0);
        let off = offset(8 * 3600);

        // allTime / None / empty / unknown → None.
        assert_eq!(parse_time_window(now, off, None)?, None);
        assert_eq!(parse_time_window(now, off, Some(""))?, None);
        assert_eq!(parse_time_window(now, off, Some("allTime"))?, None);
        assert_eq!(parse_time_window(now, off, Some("bogus"))?, None);

        // day/week/month → the corresponding Shanghai-local-midnight-as-UTC instant.
        let periods = get_calendar_periods(now, off)?;
        assert_eq!(
            parse_time_window(now, off, Some("day"))?,
            Some(periods.today.start)
        );
        assert_eq!(
            parse_time_window(now, off, Some("week"))?,
            Some(periods.this_week.start)
        );
        assert_eq!(
            parse_time_window(now, off, Some("month"))?,
            Some(periods.this_month.start)
        );
        Ok(())
    }

    // =========================================================================
    // RUST-P10-002 S07 — comprehensive UsageLog field coverage.
    // Mirror the Go Ent schema `internal/ent/schema/usage_log.go` +
    // `biz/usage_log.go::CreateUsageLog` field-population logic. The Go
    // `TestCreateUsageLog_*` cases live in `biz/usage_log_test.go` and assert
    // field-by-field population after `mut.Save(ctx)`; the tests below assert
    // the same field coverage at the pure-construction layer (the I/O is the
    // repo's job, mirroring the service/repository split used throughout the
    // Rust port).
    // =========================================================================

    // Helper building a populated `Usage` exercising every token-detail field.
    fn full_usage() -> Usage {
        Usage {
            prompt_tokens: 100,
            completion_tokens: 40,
            total_tokens: 140,
            prompt_details: TokenDetails {
                cached_tokens: 10,
                write_cached_tokens: 2,
                write_cached_tokens_5m: 3,
                write_cached_tokens_1h: 4,
                audio_tokens: 5,
                reasoning_tokens: 6,
                accepted_prediction_tokens: 7,
                rejected_prediction_tokens: 8,
                ..TokenDetails::default()
            },
            completion_details: TokenDetails {
                audio_tokens: 11,
                reasoning_tokens: 12,
                accepted_prediction_tokens: 13,
                rejected_prediction_tokens: 14,
                ..TokenDetails::default()
            },
            ..Usage::default()
        }
    }

    // S07 — `UsageLog::from_usage` populates every Go schema field from the
    // `llm::Usage` struct (mirrors Go `CreateUsageLog` lines 103-137). All
    // token counts and both prompt/completion detail groups are mapped, and
    // the cost fields are left at their Go zero state (None / empty) — Go
    // computes cost in a separate `computeUsageCost` step.
    #[test]
    fn s07_from_usage_populates_all_token_fields() {
        let usage = full_usage();
        let log = UsageLog::from_usage(
            42,
            7,
            Some(99),
            "gpt-test",
            UsageLogSource::Api,
            "openai/chat_completions",
            Some(5),
            &usage,
        );

        // Identity fields.
        assert_eq!(log.request_id, 42);
        assert_eq!(log.project_id, 7);
        assert_eq!(log.channel_id, Some(99));
        assert_eq!(log.api_key_id, Some(5));
        assert_eq!(log.model_id, "gpt-test");
        assert_eq!(log.source, UsageLogSource::Api);
        assert_eq!(log.format, "openai/chat_completions");

        // Core token metrics.
        assert_eq!(log.prompt_tokens, 100);
        assert_eq!(log.completion_tokens, 40);
        assert_eq!(log.total_tokens, 140);

        // Prompt tokens details.
        assert_eq!(log.prompt_audio_tokens, 5);
        assert_eq!(log.prompt_cached_tokens, 10);
        assert_eq!(log.prompt_write_cached_tokens, 2);
        assert_eq!(log.prompt_write_cached_tokens_5m, 3);
        assert_eq!(log.prompt_write_cached_tokens_1h, 4);

        // Completion tokens details.
        assert_eq!(log.completion_audio_tokens, 11);
        assert_eq!(log.completion_reasoning_tokens, 12);
        assert_eq!(log.completion_accepted_prediction_tokens, 13);
        assert_eq!(log.completion_rejected_prediction_tokens, 14);

        // Cost fields default to the Go zero state (cost is computed later).
        assert!(log.total_cost.is_none());
        assert!(log.cost_items.is_empty());
        assert!(log.cost_price_reference_id.is_none());
    }

    // S07 — `with_cost` mirrors Go `mut.SetNillableTotalCost(totalCost)
    // .SetCostItems(costItems); if priceReferenceID != "" {
    // mut.SetCostPriceReferenceID(priceReferenceID) }`. A non-empty
    // reference id is stored; an empty one leaves `cost_price_reference_id`
    // unset (Go skips the Set call for `""`).
    #[test]
    fn s07_with_cost_attaches_reference_id_when_non_empty() -> UsageServiceResult<()> {
        let usage = full_usage();
        let log = UsageLog::from_usage(
            1,
            1,
            None,
            "gpt-test",
            UsageLogSource::Playground,
            "anthropic/messages",
            None,
            &usage,
        )
        .with_cost(
            Decimal::new(42, 2), // 0.42
            vec![ComputedCostItem {
                item_code: "usage".to_string(),
                prompt_write_cache_variant_code: None,
                detail: CostItemDetail {
                    quantity: 100,
                    subtotal: Decimal::new(42, 2),
                    tier_breakdown: Vec::new(),
                },
            }],
            "price-ref-abc".to_string(),
        );

        assert_eq!(log.total_cost, Some(Decimal::new(42, 2)));
        assert_eq!(log.cost_items.len(), 1);
        assert_eq!(log.cost_items[0].item_code, "usage");
        assert_eq!(log.cost_items[0].detail.quantity, 100);
        assert_eq!(
            log.cost_price_reference_id.as_deref(),
            Some("price-ref-abc")
        );
        Ok(())
    }

    // S07 — Go `if priceReferenceID != "" { ... }` guard: an empty
    // reference id MUST NOT populate `cost_price_reference_id`.
    #[test]
    fn s07_with_cost_omits_reference_id_when_empty() {
        let usage = full_usage();
        let log = UsageLog::from_usage(
            1,
            1,
            None,
            "gpt-test",
            UsageLogSource::Test,
            "openai/chat_completions",
            None,
            &usage,
        )
        .with_cost(Decimal::ZERO, Vec::new(), String::new());

        assert_eq!(log.total_cost, Some(Decimal::ZERO));
        assert!(log.cost_items.is_empty());
        // Go: empty string → SetCostPriceReferenceID is NOT called.
        assert!(log.cost_price_reference_id.is_none());
    }

    // S07 — serde round-trip preserves every field with the Go camelCase JSON
    // shape. This pins the `rename_all = "camelCase"` mapping for the
    // acronym-bearing and hyphen-bearing fields the Go frontend reads
    // (`requestId`, `apiKeyId`, `projectId`, `channelId`, `modelId`,
    // `promptWriteCachedTokens5m`, `promptWriteCachedTokens1h`,
    // `completionAcceptedPredictionTokens`,
    // `completionRejectedPredictionTokens`, `totalCost`, `costItems`,
    // `costPriceReferenceId`). The Go GraphQL schema exposes these names.
    #[test]
    fn s07_usage_log_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        let usage = full_usage();
        let log = UsageLog::from_usage(
            42,
            7,
            Some(99),
            "gpt-test",
            UsageLogSource::Playground,
            "anthropic/messages",
            Some(5),
            &usage,
        )
        .with_cost(
            Decimal::new(42, 2),
            vec![ComputedCostItem {
                item_code: "usage".to_string(),
                prompt_write_cache_variant_code: Some("5m".to_string()),
                detail: CostItemDetail {
                    quantity: 100,
                    subtotal: Decimal::new(42, 2),
                    tier_breakdown: vec![TierCostDetail {
                        up_to: Some(100),
                        units: 100,
                        subtotal: Decimal::new(42, 2),
                    }],
                },
            }],
            "price-ref-abc".to_string(),
        );

        let json = serde_json::to_string(&log)?;
        let back: UsageLog = serde_json::from_str(&json)?;

        assert_eq!(log, back);

        // Spot-check the Go camelCase field names appear verbatim in the JSON
        // (defends against an accidental `rename_all` regression on this
        // struct).
        for needle in [
            "\"requestId\":42",
            "\"apiKeyId\":5",
            "\"projectId\":7",
            "\"channelId\":99",
            "\"modelId\":\"gpt-test\"",
            "\"promptTokens\":100",
            "\"completionTokens\":40",
            "\"totalTokens\":140",
            "\"promptAudioTokens\":5",
            "\"promptCachedTokens\":10",
            "\"promptWriteCachedTokens\":2",
            "\"promptWriteCachedTokens5m\":3",
            "\"promptWriteCachedTokens1h\":4",
            "\"completionAudioTokens\":11",
            "\"completionReasoningTokens\":12",
            "\"completionAcceptedPredictionTokens\":13",
            "\"completionRejectedPredictionTokens\":14",
            "\"source\":\"playground\"",
            "\"format\":\"anthropic/messages\"",
            // S10: `totalCost` serializes as a JSON **number** to match Go's
            // `field.Float` → `float64` wire form (NOT a quoted string).
            "\"totalCost\":0.42",
            "\"costItems\":[{",
            "\"itemCode\":\"usage\"",
            "\"promptWriteCacheVariantCode\":\"5m\"",
            "\"tierBreakdown\":[{",
            "\"costPriceReferenceId\":\"price-ref-abc\"",
        ] {
            assert!(
                json.contains(needle),
                "JSON missing expected camelCase field: {needle}\nfull json: {json}"
            );
        }
        Ok(())
    }

    // S07 — Go schema defaults: `project_id` defaults to 1, `format` defaults
    // to `"openai/chat_completions"`, `source` defaults to `"api"`. The Rust
    // serde defaults (`#[serde(default = ...)]` + `UsageLogSource::default()`)
    // must reproduce these when the JSON omits the fields entirely (mirrors
    // how Go Ent back-fills `Default(...)` values on insert).
    #[test]
    fn s07_serde_defaults_match_go_schema_defaults() -> Result<(), Box<dyn std::error::Error>> {
        // Minimal JSON with only the required (non-defaulted) fields.
        let json = r#"{
            "requestId": 1,
            "modelId": "gpt-test",
            "promptTokens": 10,
            "completionTokens": 5,
            "totalTokens": 15
        }"#;
        let log: UsageLog = serde_json::from_str(json)?;

        // Go `field.Int("project_id").Default(1)`.
        assert_eq!(log.project_id, 1);
        // Go `field.String("format").Default("openai/chat_completions")`.
        assert_eq!(log.format, "openai/chat_completions");
        // Go `field.Enum("source").Default("api")`.
        assert_eq!(log.source, UsageLogSource::Api);
        // Go Optional/zero fields.
        assert!(log.api_key_id.is_none());
        assert!(log.channel_id.is_none());
        assert_eq!(log.prompt_cached_tokens, 0);
        assert_eq!(log.completion_reasoning_tokens, 0);
        assert!(log.total_cost.is_none());
        assert!(log.cost_items.is_empty());
        assert!(log.cost_price_reference_id.is_none());

        // The default enum serializes back to the lowercase Go form `"api"`.
        assert_eq!(UsageLogSource::default().as_str(), "api");
        let s = serde_json::to_string(&UsageLogSource::default())?;
        assert_eq!(s, "\"api\"");
        Ok(())
    }

    // =========================================================================
    // RUST-P10-002 S10 — `total_cost` JSON form must match Go's `field.Float`
    // → `float64` wire form (a JSON **number**, not a string). The internal
    // cost math stays in full-precision `Decimal` (S09); only the
    // serialization boundary converts to f64, mirroring Go's
    // `total.InexactFloat64()` in `biz/usage_log.go::computeUsageCost`.
    //
    // Go reference behavior (verified against
    // `internal/ent/schema/usage_log.go::field.Float("total_cost")` +
    // `biz/usage_log.go` lines 55, 149):
    //   * `cost_calc.go::ComputeUsageCost` returns `decimal.Decimal` total.
    //   * `usage_log.go` calls `total.InexactFloat64()` → stores as float64.
    //   * Ent serializes `field.Float` as JSON number.
    // By contrast, `objects.CostItem.Subtotal` (`decimal.Decimal`) serializes
    // as a JSON **string** — so `cost_items[].subtotal` keeps the default
    // `rust_decimal` string form (no adapter). This asymmetry IS the Go
    // contract; the tests below pin it.
    // =========================================================================

    // S10 — `total_cost` (Some) serializes as a JSON **number**, not a string.
    // Mirrors Go `field.Float` → `0.42` (no quotes). This is the headline
    // S10 compatibility requirement.
    #[test]
    fn s10_total_cost_serializes_as_json_number_not_string()
    -> Result<(), Box<dyn std::error::Error>> {
        let usage = full_usage();
        let log = UsageLog::from_usage(
            1,
            1,
            None,
            "gpt-test",
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
            &usage,
        )
        .with_cost(Decimal::new(42, 2), Vec::new(), "ref".to_string());

        let json = serde_json::to_string(&log)?;
        // JSON number form: no quotes around 0.42.
        assert!(
            json.contains("\"totalCost\":0.42"),
            "totalCost must be a JSON number (no quotes); got: {json}"
        );
        // And it must NOT be the quoted string form.
        assert!(
            !json.contains("\"totalCost\":\"0.42\""),
            "totalCost must NOT be a quoted string; got: {json}"
        );

        // Re-parse and confirm the JSON value is a number, not a string.
        let v: serde_json::Value = serde_json::from_str(&json)?;
        match v.get("totalCost") {
            Some(serde_json::Value::Number(n)) => {
                assert_eq!(n.as_f64(), Some(0.42));
            }
            other => panic!("expected JSON number for totalCost, got {other:?}"),
        }
        Ok(())
    }

    // S10 — `total_cost` round-trips through JSON preserving precision. The
    // f64 wire form is lossy in general, but for typical cost magnitudes
    // (≤ trillions with ≤ 10 decimal places) it round-trips exactly; the
    // Go side has the same limitation because it also stores float64.
    #[test]
    fn s10_total_cost_round_trips_through_json() -> Result<(), Box<dyn std::error::Error>> {
        // Mirror the Go golden cost from `TestUsageLogService_CreateUsageLog_WithCachedTokens`:
        // (700/1e6)*0.03 + (300/1e6)*0.015 + (500/1e6)*0.06 = 0.0000555.
        let d = Decimal::new(555, 7); // 0.0000555
        let usage = full_usage();
        let log = UsageLog::from_usage(
            1,
            1,
            Some(2),
            "gpt-4",
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
            &usage,
        )
        .with_cost(d, Vec::new(), "ref".to_string());

        let json = serde_json::to_string(&log)?;
        let back: UsageLog = serde_json::from_str(&json)?;

        // The in-memory Decimal survives the round-trip.
        assert_eq!(back.total_cost, Some(d));
        // And the f64 view matches the Go `InexactFloat64` expected value
        // (`require.InDelta(t, 0.0000555, *created.TotalCost, 0.0000001)`).
        let f = back.total_cost_as_f64().unwrap_or(0.0);
        assert!(
            (f - 0.0000555).abs() < 0.0000001,
            "f64 form {f} must match Go InexactFloat64 within 1e-7"
        );
        Ok(())
    }

    // S10 — deserialization accepts BOTH the Go number form AND the legacy
    // Rust string form, so old snapshots continue to load. Go's float64 form
    // is the primary contract; the string form is a backwards-compat fallback.
    #[test]
    fn s10_total_cost_deserializes_from_number_or_string() -> Result<(), Box<dyn std::error::Error>>
    {
        // Number form (Go JSON snapshot).
        let num_json = r#"{"requestId":1,"modelId":"m","promptTokens":1,"completionTokens":1,"totalTokens":2,"totalCost":0.42}"#;
        let from_num: UsageLog = serde_json::from_str(num_json)?;
        assert_eq!(from_num.total_cost, Some(Decimal::new(42, 2)));

        // String form (legacy Rust snapshot, pre-S10).
        let str_json = r#"{"requestId":1,"modelId":"m","promptTokens":1,"completionTokens":1,"totalTokens":2,"totalCost":"0.42"}"#;
        let from_str: UsageLog = serde_json::from_str(str_json)?;
        assert_eq!(from_str.total_cost, Some(Decimal::new(42, 2)));

        // null → None.
        let null_json = r#"{"requestId":1,"modelId":"m","promptTokens":1,"completionTokens":1,"totalTokens":2,"totalCost":null}"#;
        let from_null: UsageLog = serde_json::from_str(null_json)?;
        assert!(from_null.total_cost.is_none());

        // Omitted → None (serde default).
        let omitted_json = r#"{"requestId":1,"modelId":"m","promptTokens":1,"completionTokens":1,"totalTokens":2}"#;
        let from_omitted: UsageLog = serde_json::from_str(omitted_json)?;
        assert!(from_omitted.total_cost.is_none());
        Ok(())
    }

    // S10 — `cost_items[].subtotal` (Go `decimal.Decimal`) stays a JSON
    // **string**, even though `total_cost` is a number. This asymmetry IS the
    // Go contract: `field.Float("total_cost")` vs `objects.CostItem.Subtotal
    // decimal.Decimal`. The Rust port must reproduce it exactly so a single
    // JSON snapshot containing both fields parses the same way on both sides.
    #[test]
    fn s10_cost_item_subtotal_stays_string_while_total_cost_is_number()
    -> Result<(), Box<dyn std::error::Error>> {
        let usage = full_usage();
        let log = UsageLog::from_usage(
            1,
            1,
            None,
            "gpt-4",
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
            &usage,
        )
        .with_cost(
            Decimal::new(42, 2),
            vec![ComputedCostItem {
                item_code: "usage".to_string(),
                prompt_write_cache_variant_code: None,
                detail: CostItemDetail {
                    quantity: 100,
                    subtotal: Decimal::new(12, 2), // 0.12
                    tier_breakdown: vec![TierCostDetail {
                        up_to: Some(100),
                        units: 100,
                        subtotal: Decimal::new(12, 2),
                    }],
                },
            }],
            "ref".to_string(),
        );

        let json = serde_json::to_string(&log)?;
        // totalCost → number (S10).
        assert!(json.contains("\"totalCost\":0.42"));
        // cost_items[].subtotal → string (Go decimal.Decimal form).
        assert!(
            json.contains("\"subtotal\":\"0.12\""),
            "cost_items[].subtotal must stay a JSON string (Go decimal.Decimal form); got: {json}"
        );
        // tier_breakdown[].subtotal → string too.
        assert!(json.contains("\"tierBreakdown\":[{\""));
        // Round-trip preserves the asymmetry.
        let back: UsageLog = serde_json::from_str(&json)?;
        assert_eq!(back.total_cost, Some(Decimal::new(42, 2)));
        assert_eq!(back.cost_items[0].detail.subtotal, Decimal::new(12, 2));
        Ok(())
    }

    // S10 — `sum_cost_items` reproduces Go's `ComputeUsageCost(...).Total`
    // (the pure decimal accumulation), and `total_cost_as_f64` reproduces Go's
    // `total.InexactFloat64()`. Together they mirror the two-step Go flow in
    // `biz/usage_log.go::computeUsageCost` lines 53-55:
    //   `items, total := ComputeUsageCost(usage, modelPrice.Price)`
    //   `totalCost := total.InexactFloat64()`.
    // The Go golden value here mirrors `TestUsageLogService_CreateUsageLog_WithPriceReferenceID`:
    //   (1000/1e6)*0.03 + (500/1e6)*0.06 = 0.00006.
    #[test]
    fn s10_sum_cost_items_and_f64_view_match_go_compute_flow()
    -> Result<(), Box<dyn std::error::Error>> {
        // Build two cost items mirroring the Go test's price calculation.
        let items = vec![
            ComputedCostItem {
                item_code: "usage".to_string(),
                prompt_write_cache_variant_code: None,
                detail: CostItemDetail {
                    quantity: 1000,
                    // (1000 / 1e6) * 0.03 = 0.00003
                    subtotal: Decimal::new(3, 5),
                    tier_breakdown: Vec::new(),
                },
            },
            ComputedCostItem {
                item_code: "completion".to_string(),
                prompt_write_cache_variant_code: None,
                detail: CostItemDetail {
                    quantity: 500,
                    // (500 / 1e6) * 0.06 = 0.00003
                    subtotal: Decimal::new(3, 5),
                    tier_breakdown: Vec::new(),
                },
            },
        ];

        let usage = full_usage();
        let total = items.iter().map(|i| i.detail.subtotal).sum::<Decimal>();
        let log = UsageLog::from_usage(
            1,
            1,
            Some(1),
            "gpt-4",
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
            &usage,
        )
        .with_cost(total, items, "test-ref-123".to_string());

        // Pure decimal sum (matches Go `ComputeUsageCost` total return).
        assert_eq!(log.sum_cost_items(), Decimal::new(6, 5)); // 0.00006
        // f64 view matches Go's `InexactFloat64` and the
        // `require.InDelta(t, 0.00006, *created.TotalCost, 0.0000001)` golden.
        let f = log.total_cost_as_f64().unwrap_or(0.0);
        assert!(
            (f - 0.00006).abs() < 0.0000001,
            "f64 view {f} must match Go InexactFloat64 within 1e-7"
        );
        // Reference id propagation (S11 hookup).
        assert_eq!(log.cost_price_reference_id.as_deref(), Some("test-ref-123"));
        Ok(())
    }

    // =========================================================================
    // RUST-P10-002 S11 — price reference_id binding + no-error fallback.
    // Mirror Go `biz/usage_log.go::(*UsageLogService).computeUsageCost`
    // (lines 29-70), which NEVER errors: every "price not found" /
    // "channel disabled" / "usage nil" path returns the zero triple
    // `(items=nil, totalCost=nil, priceReferenceID="")` and the request
    // continues with a zero-cost usage-log row.
    // =========================================================================

    use conduit_core::objects::pricing::{
        ModelPrice, ModelPriceItem, PRICING_MODE_USAGE_PER_UNIT, PriceTier, Pricing,
        PromptWriteCacheVariant, TieredPricing,
    };

    /// Build a `ModelPrice` mirroring Go `TestUsageLogService_CreateUsageLog_WithPriceReferenceID`:
    /// prompt $0.03 / 1M, completion $0.06 / 1M. Both items use
    /// `usage_per_unit`, so `compute_item_subtotal` divides quantity by 1e6
    /// before multiplying the per-unit price (Go `unitsInMillionTokens`).
    fn price_with_reference() -> ModelPrice {
        ModelPrice {
            items: vec![
                ModelPriceItem {
                    item_code: price_item_code::USAGE.to_string(),
                    pricing: Pricing {
                        mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                        usage_per_unit: Some(Decimal::new(3, 2)), // 0.03
                        ..Default::default()
                    },
                    prompt_write_cache_variants: Vec::new(),
                },
                ModelPriceItem {
                    item_code: price_item_code::COMPLETION.to_string(),
                    pricing: Pricing {
                        mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                        usage_per_unit: Some(Decimal::new(6, 2)), // 0.06
                        ..Default::default()
                    },
                    prompt_write_cache_variants: Vec::new(),
                },
            ],
        }
    }

    // S11 — price hit: items + total + reference_id are populated. Mirrors Go
    // `computeUsageCost` lines 52-67 (`modelPrice, ok :=
    // ch.cachedModelPrices[modelID]` cache-hit branch). Golden value matches
    // `TestUsageLogService_CreateUsageLog_WithPriceReferenceID`:
    //   (1000/1e6)*0.03 + (500/1e6)*0.06 = 0.00006.
    #[test]
    fn s11_price_hit_populates_items_total_and_reference_id() {
        let price = price_with_reference();
        let resolved = ResolvedModelPrice {
            price: &price,
            reference_id: "test-ref-123",
        };
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            ..Usage::default()
        };

        let got = compute_usage_cost_with_reference(Some(&usage), Some(resolved));

        assert!(!got.is_no_cost(), "price hit must not be the no-cost shape");
        assert_eq!(got.items.len(), 2);
        // item codes preserved in declaration order.
        assert_eq!(got.items[0].item_code, price_item_code::USAGE);
        assert_eq!(got.items[1].item_code, price_item_code::COMPLETION);
        // Decimal total (full precision, S09).
        assert_eq!(got.total, Some(Decimal::new(6, 5))); // 0.00006
        // Reference id propagated verbatim.
        assert_eq!(got.reference_id, "test-ref-123");
    }

    // S11 — price miss (model not in price cache): returns the no-cost
    // fallback `(items=[], total=None, reference_id="")`. Mirrors Go line 69
    // (`return nil, nil, ""`) — the request MUST continue unblocked with a
    // zero-cost row.
    #[test]
    fn s11_price_miss_returns_no_cost_without_error() {
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            ..Usage::default()
        };

        let got = compute_usage_cost_with_reference(Some(&usage), None);

        // The function returns a value (not a Result::Err) — Go parity is
        // that price miss is a value, not a failure.
        assert!(got.is_no_cost());
        assert!(got.items.is_empty());
        assert!(got.total.is_none());
        assert!(got.reference_id.is_empty());
    }

    // S11 — `usage.is_none()` short-circuits BEFORE the price lookup: even if
    // a price IS resolved, a nil usage yields the no-cost fallback. Mirrors
    // Go lines 30-32 (`if usage == nil { return nil, nil, "" }`).
    #[test]
    fn s11_nil_usage_short_circuits_to_no_cost_even_with_price() {
        let price = price_with_reference();
        let resolved = ResolvedModelPrice {
            price: &price,
            reference_id: "test-ref-123",
        };

        // No usage data — the function MUST not touch `resolved.price`.
        let got = compute_usage_cost_with_reference(None, Some(resolved));

        assert!(got.is_no_cost());
        assert!(got.items.is_empty());
        assert!(got.total.is_none());
        assert!(got.reference_id.is_empty());
    }

    // S11 — `apply_to` reproduces Go's `CreateUsageLog` tail: hit path
    // populates the row (and the `if priceReferenceID != ""` guard keeps the
    // ref id), miss path leaves the row at the Go zero state. This is the
    // S11 end-to-end "request never blocks" guarantee.
    #[test]
    fn s11_apply_to_log_reproduces_go_createusagelog_tail() -> Result<(), Box<dyn std::error::Error>>
    {
        let price = price_with_reference();
        let usage = full_usage();

        // --- Hit path: total/items/ref_id populated. ---
        let hit_log = UsageLog::from_usage(
            1,
            1,
            Some(1),
            "gpt-4",
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
            &usage,
        );
        let hit_computation = compute_usage_cost_with_reference(
            Some(&usage),
            Some(ResolvedModelPrice {
                price: &price,
                reference_id: "ref-hit",
            }),
        );
        let hit_log = hit_computation.apply_to(hit_log);
        assert!(hit_log.total_cost.is_some());
        assert_eq!(hit_log.cost_items.len(), 2);
        // Go `if priceReferenceID != ""` guard keeps a non-empty ref id.
        assert_eq!(hit_log.cost_price_reference_id.as_deref(), Some("ref-hit"));

        // --- Miss path: row stays at Go zero state (None/empty/None). ---
        let miss_log = UsageLog::from_usage(
            2,
            1,
            Some(1),
            "gpt-4",
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
            &usage,
        );
        let miss_computation = compute_usage_cost_with_reference(Some(&usage), None); // price miss
        let miss_log = miss_computation.apply_to(miss_log);
        // Go: SetNillableTotalCost(nil) + SetCostItems(nil) + skipped ref id.
        assert!(miss_log.total_cost.is_none());
        assert!(miss_log.cost_items.is_empty());
        assert!(miss_log.cost_price_reference_id.is_none());
        Ok(())
    }

    // S11 — empty `reference_id` on a price hit must NOT populate
    // `cost_price_reference_id` (Go `if priceReferenceID != ""` guard, line
    // 152-154). The cost itself is still computed and stored.
    #[test]
    fn s11_empty_reference_id_is_skipped_but_cost_still_computed() {
        let price = price_with_reference();
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            ..Usage::default()
        };

        // Price hit but reference_id is "" (price list has no version tracking).
        let got = compute_usage_cost_with_reference(
            Some(&usage),
            Some(ResolvedModelPrice {
                price: &price,
                reference_id: "",
            }),
        );

        // Cost IS computed (hit path), so `is_no_cost()` is false.
        assert!(!got.is_no_cost());
        assert_eq!(got.total, Some(Decimal::new(6, 5)));
        assert_eq!(got.items.len(), 2);
        // But the reference id carried through is empty.
        assert_eq!(got.reference_id, "");

        // And `apply_to` skips writing `cost_price_reference_id` for "".
        let log = UsageLog::from_usage(
            1,
            1,
            None,
            "gpt-4",
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
            &usage,
        );
        let log = got.apply_to(log);
        // Cost populated...
        assert!(log.total_cost.is_some());
        assert_eq!(log.cost_items.len(), 2);
        // ...but ref id is None (Go `if priceReferenceID != ""` guard).
        assert!(log.cost_price_reference_id.is_none());
    }

    // S11 — cost-item accumulation across the hit-path items reproduces Go
    // `ComputeUsageCost(...).Total` (the running `total = total.Add(sub)`
    // inside the loop). Mirrors the golden in
    // `TestUsageLogService_CreateUsageLog_WithCachedTokens`: 3 items
    // (input/cached/completion) sum to 0.0000555.
    #[test]
    fn s11_hit_path_item_accumulation_matches_go_total_add_loop() {
        // Build a price with three items: usage $0.03/M, prompt_cached $0.015/M,
        // completion $0.06/M — mirrors the Go cached-tokens golden test.
        let price = ModelPrice {
            items: vec![
                ModelPriceItem {
                    item_code: price_item_code::USAGE.to_string(),
                    pricing: Pricing {
                        mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                        usage_per_unit: Some(Decimal::new(3, 2)),
                        ..Default::default()
                    },
                    prompt_write_cache_variants: Vec::new(),
                },
                ModelPriceItem {
                    item_code: price_item_code::PROMPT_CACHED_TOKEN.to_string(),
                    pricing: Pricing {
                        mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                        usage_per_unit: Some(Decimal::new(15, 3)), // 0.015
                        ..Default::default()
                    },
                    prompt_write_cache_variants: Vec::new(),
                },
                ModelPriceItem {
                    item_code: price_item_code::COMPLETION.to_string(),
                    pricing: Pricing {
                        mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                        usage_per_unit: Some(Decimal::new(6, 2)),
                        ..Default::default()
                    },
                    prompt_write_cache_variants: Vec::new(),
                },
            ],
        };
        // Go test usage: prompt=1000 (300 cached) → billable input=700;
        // cached=300; completion=500.
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            prompt_details: TokenDetails {
                cached_tokens: 300,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let got = compute_usage_cost_with_reference(
            Some(&usage),
            Some(ResolvedModelPrice {
                price: &price,
                reference_id: "test-ref-cached",
            }),
        );

        // Three items, one per price-list entry.
        assert_eq!(got.items.len(), 3);
        // Go golden total = 0.000021 + 0.0000045 + 0.00003 = 0.0000555.
        // Decimal preserves full precision (no float rounding).
        assert_eq!(got.total, Some(Decimal::new(555, 7))); // 0.0000555
        // And `sum_cost_items` (the manual accumulation) agrees — the
        // `total.Add(sub)` loop in Go is equivalent to summing item subtotals.
        let summed: Decimal = got.items.iter().map(|i| i.detail.subtotal).sum();
        assert_eq!(summed, Decimal::new(555, 7));
        assert_eq!(got.reference_id, "test-ref-cached");
    }

    // =========================================================================
    // RUST-P10-002 S14 — architectural constraint: UsageLogService consumes
    // ONLY structured `llm::Usage`, never raw provider bodies. Mirror Go
    // `biz/usage_log.go::CreateUsageLogParams.Usage *llm.Usage` — the service
    // layer receives already-parsed usage; body → Usage parsing is the
    // transformer/pipeline layer's job.
    //
    // These tests pin the contract via the signature of
    // `create_usage_log_from_structured_usage` / `CreateUsageLogParams` /
    // `UsageLog::from_usage`: they all take `&Usage` and have NO parameter
    // for a raw provider body. The tests assert the resulting row shape and
    // (via compile-time-checked types) that body parsing cannot sneak in.
    // =========================================================================

    // S14 — `create_usage_log_from_structured_usage` builds a full row from
    // STRUCTURED `Usage` + resolved price, mirroring Go `CreateUsageLog`.
    // No body parsing occurs anywhere in the path. The signature itself is
    // the executable contract: the only `Usage`-shaped input is `&Usage`.
    #[test]
    fn s14_create_from_structured_usage_populates_full_row_on_price_hit() {
        let price = price_with_reference();
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            prompt_details: TokenDetails {
                cached_tokens: 100,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let params = CreateUsageLogParams::new(
            42,
            7,
            Some(99),
            "gpt-4",
            &usage,
            UsageLogSource::Api,
            "openai/chat_completions",
            Some(5),
        )
        .with_resolved_price(ResolvedModelPrice {
            price: &price,
            reference_id: "test-ref-123",
        });

        let log = create_usage_log_from_structured_usage(params);

        // Identity fields propagated from the structured params.
        assert_eq!(log.request_id, 42);
        assert_eq!(log.project_id, 7);
        assert_eq!(log.channel_id, Some(99));
        assert_eq!(log.api_key_id, Some(5));
        assert_eq!(log.model_id, "gpt-4");
        assert_eq!(log.source, UsageLogSource::Api);
        assert_eq!(log.format, "openai/chat_completions");

        // Token fields populated from the STRUCTURED Usage (no body parsing).
        assert_eq!(log.prompt_tokens, 1000);
        assert_eq!(log.completion_tokens, 500);
        assert_eq!(log.total_tokens, 1500);
        assert_eq!(log.prompt_cached_tokens, 100);

        // Cost computed via S11 hit path (price resolved).
        // billable prompt = 1000 - 100 cached = 900 → (900/1e6)*0.03 = 0.000027
        // completion       = 500                → (500/1e6)*0.06 = 0.00003
        // total = 0.000057
        assert_eq!(log.total_cost, Some(Decimal::new(57, 6)));
        assert_eq!(log.cost_items.len(), 2);
        assert_eq!(log.cost_price_reference_id.as_deref(), Some("test-ref-123"));
    }

    // S14 — price miss path: `create_usage_log_from_structured_usage` still
    // returns a valid row (no error), with cost fields at the Go zero state.
    // The structured-Usage token fields are still populated — only the cost
    // computation falls back. This mirrors Go's `CreateUsageLog` continuing
    // unblocked when `computeUsageCost` returns `(nil, nil, "")`.
    #[test]
    fn s14_create_from_structured_usage_falls_back_to_no_cost_when_price_misses() {
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            ..Usage::default()
        };
        // No resolved price — S11 fallback path.
        let params = CreateUsageLogParams::new(
            1,
            1,
            None,
            "gpt-4",
            &usage,
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
        );

        let log = create_usage_log_from_structured_usage(params);

        // Structured token fields ARE populated (from `&Usage`, no body).
        assert_eq!(log.prompt_tokens, 1000);
        assert_eq!(log.completion_tokens, 500);
        assert_eq!(log.total_tokens, 1500);
        // Cost fields are at the Go no-cost zero state (S11).
        assert!(log.total_cost.is_none());
        assert!(log.cost_items.is_empty());
        assert!(log.cost_price_reference_id.is_none());
    }

    // S14 — nil-usage guard: Go `CreateUsageLog` returns early when
    // `params.Usage == nil` (line 97-99: `return nil, nil`). The Rust
    // equivalent cannot express `None` because `CreateUsageLogParams.usage`
    // is `&Usage` (non-optional) — the type system ITSELF enforces S14: a
    // caller cannot construct the params without a real `Usage` value, which
    // means the Go `nil` guard is subsumed by Rust's ownership of a live
    // `Usage`. This test documents that: building `Usage::default()` (the
    // Rust equivalent of "zero usage") still yields a valid row; the caller
    // who would have passed `nil` in Go simply never calls this function.
    #[test]
    fn s14_zero_usage_produces_zero_token_row_without_nil_guard() {
        let usage = Usage::default(); // all-zero structured Usage
        let params = CreateUsageLogParams::new(
            1,
            1,
            None,
            "gpt-4",
            &usage,
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
        );
        let log = create_usage_log_from_structured_usage(params);

        // Zero structured usage → zero token fields. No nil guard needed.
        assert_eq!(log.prompt_tokens, 0);
        assert_eq!(log.completion_tokens, 0);
        assert_eq!(log.total_tokens, 0);
    }

    // S14 — the type system enforces "no body parsing": `UsageLog::from_usage`
    // and `CreateUsageLogParams::new` signatures accept ONLY `&Usage`. There
    // is no `from_json` / `from_body` / `from_slice` constructor on `UsageLog`
    // that takes a provider body. This test grep-checks the public API at
    // compile time by asserting the documented constructors exist and have
    // structured-only signatures; any future body-parsing constructor would
    // have to be added explicitly and would show up in a parity audit.
    //
    // The test exercises every constructor that builds a `UsageLog` from
    // external input and confirms none of them accepts a `&[u8]` / `Value` /
    // `&str` body in the usage position.
    #[test]
    fn s14_usage_log_constructors_take_only_structured_usage() {
        let usage = full_usage();

        // `UsageLog::from_usage` — takes `&Usage` in the usage slot.
        let log1 = UsageLog::from_usage(
            1,
            1,
            None,
            "gpt-test",
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
            &usage,
        );
        assert_eq!(log1.prompt_tokens, usage.prompt_tokens as i64);

        // `CreateUsageLogParams::new` — takes `&Usage` in the usage slot.
        let params = CreateUsageLogParams::new(
            1,
            1,
            None,
            "gpt-test",
            &usage,
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
        );
        let log2 = create_usage_log_from_structured_usage(params);
        assert_eq!(log2.prompt_tokens, usage.prompt_tokens as i64);

        // The serde `Deserialize` impl on `UsageLog` exists for JSON
        // *storage/retrieval* of an already-built row (DB round-trip), NOT
        // for ingesting a provider response body. This is documented on the
        // struct; the test confirms a row built from structured Usage
        // round-trips losslessly through the storage JSON form.
        let json = serde_json::to_string(&log2)
            .map_err(|e| format!("serialize: {e}"))
            .unwrap_or_else(|_| String::new());
        let back: UsageLog = serde_json::from_str(&json)
            .map_err(|e| format!("deserialize: {e}"))
            .unwrap_or_else(|_| {
                UsageLog::from_usage(
                    0,
                    0,
                    None,
                    "",
                    UsageLogSource::Api,
                    "",
                    None,
                    &Usage::default(),
                )
            });
        assert_eq!(log2, back);
    }

    // S14 — the structured-input contract is preserved across the cost
    // computation pipeline too: `compute_usage_cost_with_reference` takes
    // `Option<&Usage>` (structured), never a body. End-to-end: structured
    // Usage → cost computation → row, with no body parsing anywhere. This
    // mirrors how Go's `*llm.Usage` flows from the transformer through
    // `CreateUsageLog` into `computeUsageCost` without ever becoming a body
    // again.
    #[test]
    fn s14_structured_usage_flows_through_cost_computation_without_body_parsing()
    -> Result<(), Box<dyn std::error::Error>> {
        let price = price_with_reference();
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            ..Usage::default()
        };

        // Stage 1: structured Usage → cost computation (pure).
        let resolved = ResolvedModelPrice {
            price: &price,
            reference_id: "ref-1",
        };
        let computation = compute_usage_cost_with_reference(Some(&usage), Some(resolved));
        assert_eq!(computation.total, Some(Decimal::new(6, 5))); // 0.00006

        // Stage 2: structured Usage → row skeleton (pure, no body parsing).
        let log = UsageLog::from_usage(
            1,
            1,
            Some(1),
            "gpt-4",
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
            &usage,
        );

        // Stage 3: fold cost onto row — the row's `Usage`-derived fields are
        // unchanged; only cost fields are attached.
        let log = computation.apply_to(log);
        assert_eq!(log.prompt_tokens, 1000); // still from structured Usage
        assert_eq!(log.completion_tokens, 500);
        assert_eq!(log.total_cost, Some(Decimal::new(6, 5))); // from the computation
        assert_eq!(log.cost_items.len(), 2);

        // The pipeline never sees a provider body — `&Usage` is the only
        // usage-shaped input at every stage.
        Ok(())
    }

    // =========================================================================
    // RUST-P10-002 A01 — cost_calc.go Go tests port.
    //
    // Each test below mirrors one golden case in
    // `conduit/internal/server/biz/cost_calc_test.go` (the pure
    // `ComputeUsageCost` helper, NOT the DB-backed `usage_cost_test.go`).
    // The Rust entry point [`compute_usage_cost_full`] is the 1:1 port of Go
    // `ComputeUsageCost(usage *llm.Usage, price objects.ModelPrice)`.
    //
    // Coverage map (Go test → Rust test):
    //   * `TestComputeUsageCost_WithCachedTokens` (line 13)
    //     → `a01_with_cached_tokens_excludes_them_from_input_quantity`
    //   * `TestComputeUsageCost_WithoutCachedTokens` (line 94)
    //     → `a01_without_cached_tokens_bills_full_prompt`
    //   * `TestComputeUsageCost_WithZeroCachedTokens` (line 148)
    //     → `a01_with_zero_cached_tokens_bills_full_prompt`
    //   * `TestComputeUsageCost_WithWriteCachedTokens` (line 196)
    //     → `a01_with_write_cached_tokens_uses_shared_pricing`
    //   * `TestComputeUsageCost_WithBothCachedAndWriteCachedTokens` (line 275)
    //     → `a01_with_both_cached_and_write_cached_excludes_both_from_input`
    //   * `TestComputeUsageCost_AllTokensCached` (line 370)
    //     → `a01_all_tokens_cached_clamps_input_to_zero_subtotal`
    //
    // Decimal goldens use `Decimal::new(value, scale)` (workspace forbids
    // `dec!` macro and `unwrap`/`expect`). The million-token divisor mirrors
    // Go's `unitsInMillionTokens`.
    // =========================================================================

    /// Build a per-unit pricing item with `item_code` and `price_per_million`.
    /// Mirrors Go's `mustDecimalPtr("0.03")` test helper.
    fn per_unit_item(item_code: &str, price_per_million: Decimal) -> ModelPriceItem {
        ModelPriceItem {
            item_code: item_code.to_string(),
            pricing: Pricing {
                mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                usage_per_unit: Some(price_per_million),
                ..Default::default()
            },
            prompt_write_cache_variants: Vec::new(),
        }
    }

    /// Find the first computed cost item matching `code`, panicking if absent.
    /// Mirrors the Go test pattern of scanning `items` for the matching
    /// `ItemCode` via `require.NotNil(t, ...)`. Uses `panic!` (not `.expect`)
    /// to honor the workspace's `clippy::expect_used = "deny"` lint.
    fn require_item<'a>(
        items: &'a [ComputedCostItem],
        code: &str,
        label: &str,
    ) -> &'a ComputedCostItem {
        match items.iter().find(|i| i.item_code == code) {
            Some(item) => item,
            None => panic!("missing cost item for {label} ({code})"),
        }
    }

    // Go `TestComputeUsageCost_WithCachedTokens` (cost_calc_test.go:13-92).
    // Usage: prompt=1000 (incl. 300 cached), completion=500.
    // Price: usage $0.03/M, completion $0.06/M, prompt_cached $0.015/M.
    // Expected: input qty=700, cached qty=300, completion qty=500;
    //   total = 0.000021 + 0.0000045 + 0.00003 = 0.0000555.
    #[test]
    fn a01_with_cached_tokens_excludes_them_from_input_quantity() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(3, 2)), // 0.03
                per_unit_item(price_item_code::COMPLETION, Decimal::new(6, 2)), // 0.06
                per_unit_item(price_item_code::PROMPT_CACHED_TOKEN, Decimal::new(15, 3)), // 0.015
            ],
        };
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            prompt_details: TokenDetails {
                cached_tokens: 300,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        assert_eq!(result.items.len(), 3, "Go test requires Len(items)==3");
        let input = require_item(&result.items, price_item_code::USAGE, "usage");
        let cached = require_item(
            &result.items,
            price_item_code::PROMPT_CACHED_TOKEN,
            "cached",
        );
        let completion = require_item(&result.items, price_item_code::COMPLETION, "completion");

        // Per-item quantities mirror the Go golden assertions (lines 82-91).
        assert_eq!(input.detail.quantity, 700, "input excludes 300 cached");
        assert_eq!(cached.detail.quantity, 300, "cached qty is 300");
        assert_eq!(completion.detail.quantity, 500, "completion qty is 500");

        // Per-item subtotals (Go `require.InDelta` goldens).
        assert_eq!(input.detail.subtotal, Decimal::new(21, 6)); // 0.000021
        assert_eq!(cached.detail.subtotal, Decimal::new(45, 7)); // 0.0000045
        assert_eq!(completion.detail.subtotal, Decimal::new(3, 5)); // 0.00003

        // Grand total (full-precision decimal, no f64 rounding).
        assert_eq!(result.total, Decimal::new(555, 7)); // 0.0000555
    }

    // Go `TestComputeUsageCost_WithoutCachedTokens` (cost_calc_test.go:94-146).
    // No PromptTokensDetails at all → all prompt tokens billable.
    // Expected total = (1000/1e6)*0.03 + (500/1e6)*0.06 = 0.00006.
    #[test]
    fn a01_without_cached_tokens_bills_full_prompt() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(3, 2)),
                per_unit_item(price_item_code::COMPLETION, Decimal::new(6, 2)),
            ],
        };
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        assert_eq!(result.items.len(), 2);
        let input = require_item(&result.items, price_item_code::USAGE, "usage");
        assert_eq!(input.detail.quantity, 1000);
        assert_eq!(input.detail.subtotal, Decimal::new(3, 5)); // 0.00003
        assert_eq!(result.total, Decimal::new(6, 5)); // 0.00006
    }

    // Go `TestComputeUsageCost_WithZeroCachedTokens` (cost_calc_test.go:148-194).
    // PromptTokensDetails present but CachedTokens=0 → full prompt billable.
    // Mirrors Go's "explicit zero is the same as no detail" contract.
    #[test]
    fn a01_with_zero_cached_tokens_bills_full_prompt() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(3, 2)),
                per_unit_item(price_item_code::COMPLETION, Decimal::new(6, 2)),
            ],
        };
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            prompt_details: TokenDetails {
                cached_tokens: 0,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        let input = require_item(&result.items, price_item_code::USAGE, "usage");
        assert_eq!(input.detail.quantity, 1000);
        assert_eq!(result.total, Decimal::new(6, 5)); // 0.00006
    }

    // Go `TestComputeUsageCost_WithWriteCachedTokens` (cost_calc_test.go:196-273).
    // PromptTokensDetails.WriteCachedTokens=200 (no 5m/1h variants) → shared
    // pricing on the WRITE_CACHED_TOKENS item, input excludes the 200.
    // Expected = (800/1e6)*0.03 + (200/1e6)*0.0375 + (500/1e6)*0.06 = 0.0000615.
    #[test]
    fn a01_with_write_cached_tokens_uses_shared_pricing() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(3, 2)), // 0.03
                per_unit_item(price_item_code::COMPLETION, Decimal::new(6, 2)), // 0.06
                per_unit_item(price_item_code::WRITE_CACHED_TOKENS, Decimal::new(375, 4)), // 0.0375
            ],
        };
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            prompt_details: TokenDetails {
                write_cached_tokens: 200,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        assert_eq!(result.items.len(), 3);
        let input = require_item(&result.items, price_item_code::USAGE, "usage");
        let write_cached = require_item(
            &result.items,
            price_item_code::WRITE_CACHED_TOKENS,
            "write-cached",
        );
        let completion = require_item(&result.items, price_item_code::COMPLETION, "completion");

        // Go golden per-item quantities / subtotals (lines 263-272).
        assert_eq!(input.detail.quantity, 800);
        assert_eq!(input.detail.subtotal, Decimal::new(24, 6)); // 0.000024
        assert_eq!(write_cached.detail.quantity, 200);
        assert_eq!(write_cached.detail.subtotal, Decimal::new(75, 7)); // 0.0000075
        assert_eq!(completion.detail.quantity, 500);
        assert_eq!(completion.detail.subtotal, Decimal::new(3, 5)); // 0.00003

        assert_eq!(result.total, Decimal::new(615, 7)); // 0.0000615
    }

    // Go `TestComputeUsageCost_WithBothCachedAndWriteCachedTokens`
    // (cost_calc_test.go:275-368). Both read-cached (300) and write-cached
    // (200) subtract from input quantity, and each gets its own cost item.
    // Expected = (500/1e6)*0.03 + (300/1e6)*0.015 + (200/1e6)*0.0375
    //            + (500/1e6)*0.06 = 0.000057.
    #[test]
    fn a01_with_both_cached_and_write_cached_excludes_both_from_input() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(3, 2)),
                per_unit_item(price_item_code::COMPLETION, Decimal::new(6, 2)),
                per_unit_item(price_item_code::PROMPT_CACHED_TOKEN, Decimal::new(15, 3)),
                per_unit_item(price_item_code::WRITE_CACHED_TOKENS, Decimal::new(375, 4)),
            ],
        };
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            prompt_details: TokenDetails {
                cached_tokens: 300,
                write_cached_tokens: 200,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        assert_eq!(result.items.len(), 4);
        let input = require_item(&result.items, price_item_code::USAGE, "usage");
        let cached = require_item(
            &result.items,
            price_item_code::PROMPT_CACHED_TOKEN,
            "cached",
        );
        let write_cached = require_item(
            &result.items,
            price_item_code::WRITE_CACHED_TOKENS,
            "write-cached",
        );
        let completion = require_item(&result.items, price_item_code::COMPLETION, "completion");

        // Go per-item golden assertions (lines 354-367).
        assert_eq!(input.detail.quantity, 500); // 1000 - 300 - 200
        assert_eq!(input.detail.subtotal, Decimal::new(15, 6)); // 0.000015
        assert_eq!(cached.detail.quantity, 300);
        assert_eq!(cached.detail.subtotal, Decimal::new(45, 7)); // 0.0000045
        assert_eq!(write_cached.detail.quantity, 200);
        assert_eq!(write_cached.detail.subtotal, Decimal::new(75, 7)); // 0.0000075
        assert_eq!(completion.detail.quantity, 500);
        assert_eq!(completion.detail.subtotal, Decimal::new(3, 5)); // 0.00003

        assert_eq!(result.total, Decimal::new(57, 6)); // 0.000057
    }

    // Go `TestComputeUsageCost_AllTokensCached` (cost_calc_test.go:370-429).
    // Edge case: every prompt token came from cache → input quantity clamps
    // to 0 and the input subtotal is exactly zero (cache miss → zero cost).
    // Expected total = 0 + (1000/1e6)*0.015 + (500/1e6)*0.06 = 0.000045.
    #[test]
    fn a01_all_tokens_cached_clamps_input_to_zero_subtotal() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(3, 2)),
                per_unit_item(price_item_code::COMPLETION, Decimal::new(6, 2)),
                per_unit_item(price_item_code::PROMPT_CACHED_TOKEN, Decimal::new(15, 3)),
            ],
        };
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            prompt_details: TokenDetails {
                cached_tokens: 1000, // every prompt token is cached
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        let input = require_item(
            &result.items,
            price_item_code::USAGE,
            "usage (quantity=0 is NOT filtered out)",
        );
        assert_eq!(input.detail.quantity, 0);
        // Go's `require.True(t, inputItem.Subtotal.IsZero())`.
        assert_eq!(input.detail.subtotal, Decimal::ZERO);
        assert_eq!(result.total, Decimal::new(45, 6)); // 0.000045
    }

    // =========================================================================
    // RUST-P10-002 A02 — usage_cost_test.go pure-cost goldens.
    //
    // Each test below mirrors one golden case in
    // `conduit/internal/server/biz/usage_cost_test.go` (the DB-backed Go test
    // file). The Go tests stand up an Ent test client, but the *contract*
    // they pin is the pure cost math in `ComputeUsageCost`. We exercise the
    // same pure path via [`compute_usage_cost_full`].
    //
    // Coverage map (Go test → Rust test):
    //   * `TestUsageCost_TieredPrompt` (line 120)
    //     → `a02_tiered_prompt_splits_quantity_across_tiers`
    //   * `TestUsageCost_VolumePrompt` (line 196)
    //     → `a02_volume_prompt_bills_all_at_matched_tier`
    //   * `TestUsageCost_VolumePromptFirstTier` (line 273)
    //     → `a02_volume_prompt_first_tier_matches_first_upper_bound`
    //   * `TestUsageCost_CacheVariant5Min` (line 399)
    //     → `a02_cache_variant_5min_uses_variant_pricing`
    //   * `TestUsageCost_CacheVariant1Hour` (line 490)
    //     → `a02_cache_variant_1hour_uses_variant_pricing`
    //   * `TestUsageCost_CacheVariantBoth5MinAnd1Hour` (line 581)
    //     → `a02_cache_variant_both_5min_and_1hour_split_items`
    //   * `TestUsageCost_CacheVariantFallbackToShared` (line 679)
    //     → `a02_cache_variant_falls_back_to_shared_pricing`
    //
    // Plus PostgreSQL consistency tests for the pure aggregation helper
    // (`aggregate_usage`) — see `a02_postgres_aggregate_usage_*`.
    // =========================================================================

    /// Build a tiered/volume pricing item. Mirrors Go's
    /// `objects.Pricing{Mode: PricingModeTiered|Volume, UsageTiered: ...}`.
    fn tiered_item(
        item_code: &str,
        mode: &str,
        tiers: Vec<(Option<i64>, Decimal)>,
    ) -> ModelPriceItem {
        ModelPriceItem {
            item_code: item_code.to_string(),
            pricing: Pricing {
                mode: mode.to_string(),
                usage_tiered: Some(TieredPricing {
                    tiers: tiers
                        .into_iter()
                        .map(|(up_to, price_per_unit)| PriceTier {
                            up_to,
                            price_per_unit,
                        })
                        .collect(),
                }),
                ..Default::default()
            },
            prompt_write_cache_variants: Vec::new(),
        }
    }

    // Go `TestUsageCost_TieredPrompt` (usage_cost_test.go:120-194).
    // Tiers: [<=1000 @ $0.01], [open-ended @ $0.02]; prompt=1500.
    // Expected: tier1 = (1000/1e6)*0.01 = 0.00001;
    //           tier2 = (500/1e6)*0.02 = 0.00001; total = 0.00002.
    // Also asserts the cost item carries a 2-row tier_breakdown.
    #[test]
    fn a02_tiered_prompt_splits_quantity_across_tiers() {
        let price = ModelPrice {
            items: vec![tiered_item(
                price_item_code::USAGE,
                PRICING_MODE_TIERED,
                vec![
                    (Some(1000), Decimal::new(1, 2)), // 0.01
                    (None, Decimal::new(2, 2)),       // 0.02
                ],
            )],
        };
        let usage = Usage {
            prompt_tokens: 1500,
            completion_tokens: 0,
            total_tokens: 1500,
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        assert_eq!(result.items.len(), 1, "Go test requires Len(items)==1");
        let item = &result.items[0];
        assert_eq!(item.detail.tier_breakdown.len(), 2);
        assert_eq!(item.detail.tier_breakdown[0].units, 1000);
        assert_eq!(item.detail.tier_breakdown[1].units, 500);
        assert_eq!(result.total, Decimal::new(2, 5)); // 0.00002
    }

    // Go `TestUsageCost_VolumePrompt` (usage_cost_test.go:196-271).
    // Volume mode picks the tier that matches the total quantity — here
    // 1500 > 1000 so tier 2 (open-ended, $0.02) wins. ALL 1500 tokens bill
    // at $0.02 → total = (1500/1e6)*0.02 = 0.00003. Tier breakdown has 1 row.
    #[test]
    fn a02_volume_prompt_bills_all_at_matched_tier() {
        let price = ModelPrice {
            items: vec![tiered_item(
                price_item_code::USAGE,
                PRICING_MODE_VOLUME,
                vec![(Some(1000), Decimal::new(1, 2)), (None, Decimal::new(2, 2))],
            )],
        };
        let usage = Usage {
            prompt_tokens: 1500,
            completion_tokens: 0,
            total_tokens: 1500,
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        assert_eq!(result.items.len(), 1);
        let item = &result.items[0];
        assert_eq!(item.detail.tier_breakdown.len(), 1);
        assert_eq!(item.detail.tier_breakdown[0].units, 1500);
        assert_eq!(result.total, Decimal::new(3, 5)); // 0.00003
    }

    // Go `TestUsageCost_VolumePromptFirstTier` (usage_cost_test.go:273-348).
    // Volume mode, prompt=800 → tier 1 (<=1000 @ $0.01) matches. ALL 800
    // tokens bill at $0.01 → total = (800/1e6)*0.01 = 0.000008.
    #[test]
    fn a02_volume_prompt_first_tier_matches_first_upper_bound() {
        let price = ModelPrice {
            items: vec![tiered_item(
                price_item_code::USAGE,
                PRICING_MODE_VOLUME,
                vec![(Some(1000), Decimal::new(1, 2)), (None, Decimal::new(2, 2))],
            )],
        };
        let usage = Usage {
            prompt_tokens: 800,
            completion_tokens: 0,
            total_tokens: 800,
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        assert_eq!(result.items.len(), 1);
        let item = &result.items[0];
        assert_eq!(item.detail.tier_breakdown.len(), 1);
        assert_eq!(item.detail.tier_breakdown[0].units, 800);
        assert_eq!(result.total, Decimal::new(8, 6)); // 0.000008
    }

    /// Build a WRITE_CACHED_TOKENS item with one prompt-write-cache variant.
    /// Mirrors Go's `PromptWriteCacheVariants: []objects.PromptWriteCacheVariant{{...}}`.
    fn write_cache_item(shared_price: Decimal, variants: Vec<(&str, Decimal)>) -> ModelPriceItem {
        ModelPriceItem {
            item_code: price_item_code::WRITE_CACHED_TOKENS.to_string(),
            pricing: Pricing {
                mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                usage_per_unit: Some(shared_price),
                ..Default::default()
            },
            prompt_write_cache_variants: variants
                .into_iter()
                .map(|(code, price)| PromptWriteCacheVariant {
                    variant_code: code.to_string(),
                    pricing: Pricing {
                        mode: PRICING_MODE_USAGE_PER_UNIT.to_string(),
                        usage_per_unit: Some(price),
                        ..Default::default()
                    },
                })
                .collect(),
        }
    }

    // Go `TestUsageCost_CacheVariant5Min` (usage_cost_test.go:399-488).
    // Usage: prompt=100, write_cached_tokens=50, write_cached_5m=50.
    // Price: usage $0.01/M; write_cached shared $0.04/M; 5m variant $0.03/M.
    // Expected: input=(100-50)/1e6*0.01=0.0000005;
    //           5m=(50/1e6)*0.03=0.0000015; total=0.000002.
    #[test]
    fn a02_cache_variant_5min_uses_variant_pricing() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(1, 2)), // 0.01
                write_cache_item(
                    Decimal::new(4, 2), // shared 0.04
                    vec![(
                        prompt_write_cache_variant_code::FIVE_MIN,
                        Decimal::new(3, 2), // 0.03
                    )],
                ),
            ],
        };
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 0,
            total_tokens: 100,
            prompt_details: TokenDetails {
                write_cached_tokens: 50,
                write_cached_tokens_5m: 50,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        // Two items: input + one write-cached entry tagged five_min.
        assert_eq!(result.items.len(), 2);
        let write_item = require_item(
            &result.items,
            price_item_code::WRITE_CACHED_TOKENS,
            "write-cached",
        );
        assert_eq!(
            write_item.prompt_write_cache_variant_code.as_deref(),
            Some(prompt_write_cache_variant_code::FIVE_MIN)
        );
        assert_eq!(write_item.detail.quantity, 50);
        assert_eq!(write_item.detail.subtotal, Decimal::new(15, 7)); // 0.0000015
        assert_eq!(result.total, Decimal::new(2, 6)); // 0.000002
    }

    // Go `TestUsageCost_CacheVariant1Hour` (usage_cost_test.go:490-579).
    // Usage: prompt=100, write_cached_tokens=80, write_cached_1h=80.
    // Price: usage $0.01/M; shared $0.04/M; 1h variant $0.02/M.
    // Expected: input=(20/1e6)*0.01=0.0000002;
    //           1h=(80/1e6)*0.02=0.0000016; total=0.0000018.
    #[test]
    fn a02_cache_variant_1hour_uses_variant_pricing() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(1, 2)),
                write_cache_item(
                    Decimal::new(4, 2),
                    vec![(
                        prompt_write_cache_variant_code::ONE_HOUR,
                        Decimal::new(2, 2), // 0.02
                    )],
                ),
            ],
        };
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 0,
            total_tokens: 100,
            prompt_details: TokenDetails {
                write_cached_tokens: 80,
                write_cached_tokens_1h: 80,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        assert_eq!(result.items.len(), 2);
        let write_item = require_item(
            &result.items,
            price_item_code::WRITE_CACHED_TOKENS,
            "write-cached",
        );
        assert_eq!(
            write_item.prompt_write_cache_variant_code.as_deref(),
            Some(prompt_write_cache_variant_code::ONE_HOUR)
        );
        assert_eq!(write_item.detail.quantity, 80);
        assert_eq!(write_item.detail.subtotal, Decimal::new(16, 7)); // 0.0000016
        assert_eq!(result.total, Decimal::new(18, 7)); // 0.0000018
    }

    // Go `TestUsageCost_CacheVariantBoth5MinAnd1Hour` (usage_cost_test.go:581-677).
    // Usage: prompt=100, write_cached_tokens=100, 5m=40, 1h=60.
    // Price: usage $0.01/M; shared $0.06/M; 5m variant $0.05/M; 1h variant $0.03/M.
    // Expected: input=(0/1e6)*0.01=0; 5m=(40/1e6)*0.05=0.000002;
    //           1h=(60/1e6)*0.03=0.0000018; total=0.0000038.
    // Three cost items: usage + 5m write-cached + 1h write-cached.
    #[test]
    fn a02_cache_variant_both_5min_and_1hour_split_items() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(1, 2)),
                write_cache_item(
                    Decimal::new(6, 2), // shared 0.06
                    vec![
                        (
                            prompt_write_cache_variant_code::FIVE_MIN,
                            Decimal::new(5, 2), // 0.05
                        ),
                        (
                            prompt_write_cache_variant_code::ONE_HOUR,
                            Decimal::new(3, 2), // 0.03
                        ),
                    ],
                ),
            ],
        };
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 0,
            total_tokens: 100,
            prompt_details: TokenDetails {
                write_cached_tokens: 100,
                write_cached_tokens_5m: 40,
                write_cached_tokens_1h: 60,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        // Three items: usage (qty 0) + 5m + 1h.
        assert_eq!(result.items.len(), 3);
        let write_items: Vec<&ComputedCostItem> = result
            .items
            .iter()
            .filter(|i| i.item_code == price_item_code::WRITE_CACHED_TOKENS)
            .collect();
        assert_eq!(write_items.len(), 2, "two variant items, one per code");

        let five_min = match write_items.iter().copied().find(|i| {
            i.prompt_write_cache_variant_code.as_deref()
                == Some(prompt_write_cache_variant_code::FIVE_MIN)
        }) {
            Some(item) => item,
            None => panic!("five_min variant item must exist"),
        };
        let one_hour = match write_items.iter().copied().find(|i| {
            i.prompt_write_cache_variant_code.as_deref()
                == Some(prompt_write_cache_variant_code::ONE_HOUR)
        }) {
            Some(item) => item,
            None => panic!("one_hour variant item must exist"),
        };

        assert_eq!(five_min.detail.quantity, 40);
        assert_eq!(five_min.detail.subtotal, Decimal::new(2, 6)); // 0.000002
        assert_eq!(one_hour.detail.quantity, 60);
        assert_eq!(one_hour.detail.subtotal, Decimal::new(18, 7)); // 0.0000018
        assert_eq!(result.total, Decimal::new(38, 7)); // 0.0000038
    }

    // Go `TestUsageCost_CacheVariantFallbackToShared` (usage_cost_test.go:679-762).
    // Usage: prompt=100, write_cached_tokens=70 (no 5m/1h variants in details).
    // Price: usage $0.01/M; shared $0.04/M; NO configured variants on the item.
    // Expected: input=(30/1e6)*0.01=0.0000003;
    //           shared=(70/1e6)*0.04=0.0000028; total=0.0000031.
    #[test]
    fn a02_cache_variant_falls_back_to_shared_pricing() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(1, 2)),
                write_cache_item(Decimal::new(4, 2), Vec::new()),
            ],
        };
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 0,
            total_tokens: 100,
            prompt_details: TokenDetails {
                write_cached_tokens: 70,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };

        let result = compute_usage_cost_full(Some(&usage), &price);

        // Two items: input + shared write-cached entry (no variant code).
        assert_eq!(result.items.len(), 2);
        let write_item = require_item(
            &result.items,
            price_item_code::WRITE_CACHED_TOKENS,
            "write-cached",
        );
        assert!(
            write_item.prompt_write_cache_variant_code.is_none(),
            "fallback item must NOT carry a variant code"
        );
        assert_eq!(write_item.detail.quantity, 70);
        assert_eq!(write_item.detail.subtotal, Decimal::new(28, 7)); // 0.0000028
        assert_eq!(result.total, Decimal::new(31, 7)); // 0.0000031
    }

    // =========================================================================
    // RUST-P10-002 A02 — PostgreSQL aggregation consistency.
    //
    // The active Rust server runs usage-log aggregation through PostgreSQL.
    // The repository SQL lives in `conduit-db` (out of scope for this crate),
    // but the numeric semantics it implements live here:
    //   * token counts sum as saturated integers;
    //   * cost strings sum as full-precision `Decimal`s;
    //   * grouping is by (project_id, model, channel) tuple.
    //
    // `aggregate_usage` is the backend-neutral pure helper fed by PostgreSQL
    // results. The tests below pin that it is a pure function of input
    // order/grouping and has no driver-sensitive behavior.
    // =========================================================================

    // PostgreSQL invariant #1: same rows in any order yield the same totals.
    #[test]
    fn a02_postgres_aggregate_usage_is_order_invariant() -> UsageServiceResult<()> {
        let mut a = UsageRecord::new("u1", "p1", "gpt-4", "openai", 100, 50, "0.001", "0.002");
        let mut b = UsageRecord::new("u2", "p1", "gpt-4", "openai", 200, 100, "0.001", "0.002");
        let mut c = UsageRecord::new("u3", "p1", "gpt-4", "openai", 10, 5, "0.001", "0.002");

        // Apply costs so the prompt/completion/total cost strings are
        // populated by the same code path the service uses on insert.
        apply_costs(&mut a)?;
        apply_costs(&mut b)?;
        apply_costs(&mut c)?;

        let forward = aggregate_usage(&[a.clone(), b.clone(), c.clone()])?;
        let reverse = aggregate_usage(&[c.clone(), b.clone(), a.clone()])?;
        let shuffled = aggregate_usage(&[b.clone(), c.clone(), a.clone()])?;

        assert_eq!(forward, reverse, "order must not matter");
        assert_eq!(forward, shuffled, "any permutation matches");

        // Golden values the PostgreSQL aggregation query must also produce:
        // prompt_tokens = 100 + 200 + 10 = 310.
        assert_eq!(forward.prompt_tokens, 310);
        // completion_tokens = 50 + 100 + 5 = 155.
        assert_eq!(forward.completion_tokens, 155);
        // total_tokens = 465 (each row's total_tokens = prompt + completion).
        assert_eq!(forward.total_tokens, 465);
        // prompt_cost = 100*0.001 + 200*0.001 + 10*0.001 = 0.31.
        assert_eq!(forward.prompt_cost, Decimal::new(31, 2));
        // completion_cost = 50*0.002 + 100*0.002 + 5*0.002 = 0.31.
        assert_eq!(forward.completion_cost, Decimal::new(31, 2));
        // total_cost = 0.62 (each row's total_cost = prompt_cost + completion_cost).
        assert_eq!(forward.total_cost, Decimal::new(62, 2));
        Ok(())
    }

    // PostgreSQL invariant #2: an empty row set yields NULL from SUM, which
    // the repository coalesces to zero.
    #[test]
    fn a02_aggregate_usage_empty_rows_yield_zero_totals() -> UsageServiceResult<()> {
        let totals = aggregate_usage(&[] as &[UsageRecord])?;
        assert_eq!(totals.prompt_tokens, 0);
        assert_eq!(totals.completion_tokens, 0);
        assert_eq!(totals.total_tokens, 0);
        assert_eq!(totals.prompt_cost, Decimal::ZERO);
        assert_eq!(totals.completion_cost, Decimal::ZERO);
        assert_eq!(totals.total_cost, Decimal::ZERO);
        Ok(())
    }

    // PostgreSQL invariant #3: grouping by (project_id, model, channel)
    // produces per-group totals. This exercises the
    // `FakeUsageLogRepo::aggregate_by_project_model_channel` helper, which
    // implements the same grouping as the production SQL.
    #[tokio::test]
    async fn a02_aggregate_groups_match_postgres_group_by() -> UsageServiceResult<()> {
        let repo = Arc::new(FakeUsageLogRepo::new());
        // Use the UsageLogService wrapper so apply_costs runs on each insert
        // (mirrors the real Go flow where CreateUsageLog computes costs
        // before the row hits the PostgreSQL INSERT).
        let service = UsageLogService::new(repo.clone());
        let ctx = ctx();

        // Two groups: (p1, gpt-4, openai) and (p1, claude, anthropic).
        // Multiple inserts per group, with decimal-string unit prices the
        // service multiplies by token counts to populate cost strings.
        for (id, prompt, completion) in [("u1", 100_u64, 50_u64), ("u2", 200, 100), ("u3", 10, 5)] {
            service
                .insert_usage(
                    &ctx,
                    UsageRecord::new(
                        id, "p1", "gpt-4", "openai", prompt, completion, "0.001", "0.002",
                    ),
                )
                .await?;
        }
        for (id, prompt, completion) in [("u4", 1_000_u64, 500_u64), ("u5", 2_000, 1_000)] {
            service
                .insert_usage(
                    &ctx,
                    UsageRecord::new(
                        id,
                        "p1",
                        "claude",
                        "anthropic",
                        prompt,
                        completion,
                        "0.003",
                        "0.004",
                    ),
                )
                .await?;
        }

        let aggregate = repo.aggregate_by_project_model_channel(&ctx).await?;

        // Two groups, exactly — PostgreSQL's `GROUP BY project_id, model_id,
        // channel_id` over the same rows produces two output rows.
        assert_eq!(aggregate.len(), 2);

        // Find each group and verify the per-group sums match the pure
        // helper's totals — this is the cross-check that both code paths
        // (repository aggregation via FakeUsageLogRepo, and aggregate_usage)
        // agree on the same input.
        let gpt4 = match aggregate.iter().find(|c| c.model == "gpt-4") {
            Some(c) => c,
            None => panic!("gpt-4 group must exist"),
        };
        let claude = match aggregate.iter().find(|c| c.model == "claude") {
            Some(c) => c,
            None => panic!("claude group must exist"),
        };

        // gpt-4 group: 3 rows, prompt=310 completion=155 total=465.
        assert_eq!(gpt4.prompt_tokens, 310);
        assert_eq!(gpt4.completion_tokens, 155);
        assert_eq!(gpt4.total_tokens, 465);
        // prompt_cost = 310 * 0.001 = 0.31.
        assert_eq!(gpt4.prompt_cost, Decimal::new(31, 2).to_string());
        assert_eq!(gpt4.completion_cost, Decimal::new(31, 2).to_string());
        assert_eq!(gpt4.total_cost, Decimal::new(62, 2).to_string());

        // claude group: 2 rows, prompt=3000 completion=1500 total=4500.
        assert_eq!(claude.prompt_tokens, 3_000);
        assert_eq!(claude.completion_tokens, 1_500);
        assert_eq!(claude.total_tokens, 4_500);
        // prompt_cost = 3000 * 0.003 = 9.
        assert_eq!(claude.prompt_cost, "9");
        assert_eq!(claude.completion_cost, "6");
        assert_eq!(claude.total_cost, "15");
        Ok(())
    }

    // =========================================================================
    // RUST-P15-001 — usage_log_test.go pure-logic mirror (Mendel-the-12th).
    //
    // Go `internal/server/biz/usage_log_test.go` (361 lines / 3 top-level tests)
    // is fully DB-backed (Ent + channel service + model-price cache).
    // The pure-logic contract each test pins is the field-population + cost-
    // computation flow inside Go `(*UsageLogService).CreateUsageLog`. The Rust
    // equivalent at the pure layer is [`create_usage_log_from_structured_usage`],
    // which mirrors Go `CreateUsageLog` end-to-end (identity/token fields from
    // structured `Usage` + cost via `compute_usage_cost_with_reference` + fold
    // via `apply_to`). The 3 tests below mirror each Go test's EXACT golden
    // input (token counts, detail fields, price items, reference id) and assert
    // the same field-level invariants the Go tests assert — adapted for the
    // pure layer (no DB row is saved; we inspect the returned [`UsageLog`]).
    //
    // Coverage map (Go test → Rust test):
    //   * `TestUsageLogService_CreateUsageLog_PromptWriteCachedTokens` (L21-75)
    //     → `p15_prompt_write_cached_tokens_populated_and_no_cost_for_channel_zero`
    //   * `TestUsageLogService_CreateUsageLog_WithPriceReferenceID` (L77-188)
    //     → `p15_with_price_reference_id_costs_total_and_ref_on_log_row`
    //   * `TestUsageLogService_CreateUsageLog_WithCachedTokens` (L190-356)
    //     → `p15_with_cached_tokens_splits_three_cost_items_on_log_row`
    //
    // Decimal goldens use `Decimal::new(n, scale)` (workspace forbids `dec!`
    // macro and `unwrap`/`expect`). f64 comparisons use `total_cost_as_f64()`
    // mirroring Go's `*created.TotalCost` (`*float64` via `InexactFloat64`),
    // with `< 1e-7` tolerance matching Go's `require.InDelta(..., 0.0000001)`.
    // =========================================================================

    // Go `TestUsageLogService_CreateUsageLog_PromptWriteCachedTokens`
    // (usage_log_test.go:21-75). ChannelID=0 (no channel enabled → no cost),
    // Usage: prompt=10, completion=20, total=30, PromptTokensDetails{
    //   CachedTokens=2, WriteCachedTokens=3 }. Go asserts:
    //   `created.PromptCachedTokens == 2` and `created.PromptWriteCachedTokens == 3`.
    // The pure-layer equivalent: `from_usage` maps the two detail fields, and
    // with no resolved price (channel 0 not enabled), cost is None.
    #[test]
    fn p15_prompt_write_cached_tokens_populated_and_no_cost_for_channel_zero() {
        let usage = Usage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
            prompt_details: TokenDetails {
                cached_tokens: 2,
                write_cached_tokens: 3,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };
        // Go: ChannelID=0, no resolved price (channel 0 not in enabled cache).
        let params = CreateUsageLogParams::new(
            1,
            1,
            Some(0),
            "test-model",
            &usage,
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
        );
        let log = create_usage_log_from_structured_usage(params);

        // Go L73-74: `require.Equal(t, int64(2), created.PromptCachedTokens)`.
        assert_eq!(log.prompt_cached_tokens, 2);
        // Go L74: `require.Equal(t, int64(3), created.PromptWriteCachedTokens)`.
        assert_eq!(log.prompt_write_cached_tokens, 3);
        // Core tokens propagated from structured Usage.
        assert_eq!(log.prompt_tokens, 10);
        assert_eq!(log.completion_tokens, 20);
        assert_eq!(log.total_tokens, 30);
        assert_eq!(log.model_id, "test-model");
        // Channel 0 not enabled → Go `computeUsageCost` returns (nil,nil,"").
        assert!(log.total_cost.is_none());
        assert!(log.cost_items.is_empty());
        assert!(log.cost_price_reference_id.is_none());
    }

    // Go `TestUsageLogService_CreateUsageLog_WithPriceReferenceID`
    // (usage_log_test.go:77-188). Price: usage $0.03/M, completion $0.06/M,
    // reference_id="test-ref-123". Usage: prompt=1000, completion=500,
    // total=1500 (no PromptTokensDetails). Go asserts:
    //   `created.CostPriceReferenceID == "test-ref-123"`,
    //   `created.TotalCost` non-nil, `created.CostItems` non-empty,
    //   `require.InDelta(t, 0.00006, *created.TotalCost, 0.0000001)`.
    #[test]
    fn p15_with_price_reference_id_costs_total_and_ref_on_log_row() {
        let price = price_with_reference(); // usage $0.03/M, completion $0.06/M
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            ..Usage::default()
        };
        let params = CreateUsageLogParams::new(
            1,
            1,
            Some(1),
            "gpt-4",
            &usage,
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
        )
        .with_resolved_price(ResolvedModelPrice {
            price: &price,
            reference_id: "test-ref-123",
        });
        let log = create_usage_log_from_structured_usage(params);

        // Go L181: `require.Equal(t, "test-ref-123", created.CostPriceReferenceID)`.
        assert_eq!(log.cost_price_reference_id.as_deref(), Some("test-ref-123"));
        // Go L182-183: `require.NotNil(t, created.TotalCost)` + `NotEmpty(costItems)`.
        assert!(log.total_cost.is_some());
        assert!(!log.cost_items.is_empty());
        // Go L187: `require.InDelta(t, 0.00006, *created.TotalCost, 0.0000001)`.
        // (1000/1e6)*0.03 + (500/1e6)*0.06 = 0.00003 + 0.00003 = 0.00006.
        let f = log.total_cost_as_f64().unwrap_or(0.0);
        assert!(
            (f - 0.00006_f64).abs() < 0.0000001_f64,
            "total_cost f64 view {f} must match Go InexactFloat64 within 1e-7"
        );
        // Full-precision decimal total (Rust keeps Decimal; Go lost precision
        // via InexactFloat64 — this assert is stricter than Go's InDelta).
        assert_eq!(log.total_cost, Some(Decimal::new(6, 5))); // 0.00006
    }

    // Go `TestUsageLogService_CreateUsageLog_WithCachedTokens`
    // (usage_log_test.go:190-356). Price: usage $0.03/M, completion $0.06/M,
    // prompt_cached $0.015/M, reference_id="test-ref-cached". Usage:
    //   prompt=1000 (incl. 300 cached), completion=500, total=1500.
    // Go asserts: ref id set, TotalCost non-nil, CostItems len==3, and per-item:
    //   input qty=700  subtotal≈0.000021;  cached qty=300 subtotal≈0.0000045;
    //   completion qty=500 subtotal≈0.00003; total ≈ 0.0000555.
    #[test]
    fn p15_with_cached_tokens_splits_three_cost_items_on_log_row() {
        let price = ModelPrice {
            items: vec![
                per_unit_item(price_item_code::USAGE, Decimal::new(3, 2)), // 0.03
                per_unit_item(price_item_code::COMPLETION, Decimal::new(6, 2)), // 0.06
                per_unit_item(price_item_code::PROMPT_CACHED_TOKEN, Decimal::new(15, 3)), // 0.015
            ],
        };
        let usage = Usage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
            prompt_details: TokenDetails {
                cached_tokens: 300,
                ..TokenDetails::default()
            },
            ..Usage::default()
        };
        let params = CreateUsageLogParams::new(
            1,
            1,
            Some(1),
            "gpt-4",
            &usage,
            UsageLogSource::Api,
            "openai/chat_completions",
            None,
        )
        .with_resolved_price(ResolvedModelPrice {
            price: &price,
            reference_id: "test-ref-cached",
        });
        let log = create_usage_log_from_structured_usage(params);

        // Go L311: `require.Equal(t, "test-ref-cached", created.CostPriceReferenceID)`.
        assert_eq!(
            log.cost_price_reference_id.as_deref(),
            Some("test-ref-cached")
        );
        // Go L312-313: `NotNil(TotalCost)` + `NotEmpty(CostItems)`.
        assert!(log.total_cost.is_some());
        assert!(!log.cost_items.is_empty());
        // Go L325: `require.Len(t, created.CostItems, 3)`.
        assert_eq!(log.cost_items.len(), 3);

        // Go L330-339: find each cost item by ItemCode.
        let input = require_item(&log.cost_items, price_item_code::USAGE, "usage");
        let cached = require_item(
            &log.cost_items,
            price_item_code::PROMPT_CACHED_TOKEN,
            "cached",
        );
        let completion = require_item(&log.cost_items, price_item_code::COMPLETION, "completion");

        // Go L346: `require.Equal(t, int64(700), inputItem.Quantity)`.
        assert_eq!(
            input.detail.quantity, 700,
            "input qty 700 (1000-300 cached)"
        );
        // Go L347: `require.InDelta(t, 0.000021, inputItem.Subtotal.InexactFloat64(), ...)`.
        assert_eq!(input.detail.subtotal, Decimal::new(21, 6)); // 0.000021

        // Go L350: `require.Equal(t, int64(300), cachedItem.Quantity)`.
        assert_eq!(cached.detail.quantity, 300, "cached qty 300");
        // Go L351: `require.InDelta(t, 0.0000045, cachedItem.Subtotal.InexactFloat64(), ...)`.
        assert_eq!(cached.detail.subtotal, Decimal::new(45, 7)); // 0.0000045

        // Go L354: `require.Equal(t, int64(500), completionItem.Quantity)`.
        assert_eq!(completion.detail.quantity, 500, "completion qty 500");
        // Go L355: `require.InDelta(t, 0.00003, completionItem.Subtotal.InexactFloat64(), ...)`.
        assert_eq!(completion.detail.subtotal, Decimal::new(3, 5)); // 0.00003

        // Go L321-322: total ≈ 0.0000555 within 1e-7.
        // 0.000021 + 0.0000045 + 0.00003 = 0.0000555.
        let f = log.total_cost_as_f64().unwrap_or(0.0);
        assert!(
            (f - 0.0000555_f64).abs() < 0.0000001_f64,
            "total_cost f64 view {f} must match Go InexactFloat64 within 1e-7"
        );
        // Full-precision decimal total (stricter than Go's InDelta).
        assert_eq!(log.total_cost, Some(Decimal::new(555, 7))); // 0.0000555
    }
}
