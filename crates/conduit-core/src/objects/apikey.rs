//! API-key routing/quota domain objects ported 1:1 from
//! `conduit/internal/objects/apikey.go`.
//!
//! Covers the API-key profile surface: [`APIKeyProfiles`], [`APIKeyProfile`],
//! the [`ChannelTagsMatchMode`] string-newtype (with the
//! [`channel_tags_match_mode`] constants module), the quota sub-structs
//! ([`APIKeyQuota`], [`APIKeyQuotaPeriod`], [`APIKeyQuotaPastDuration`],
//! [`APIKeyQuotaCalendarDuration`]) and their period-type / duration-unit
//! string-newtypes. The free function [`match_channel_tags`] implements the
//! multi-tag combinator (`any` / `all` / `none`) and unblocks the placeholder
//! in [`crate::objects::project::ProjectProfile::match_channel_tags`].
//!
//! `ModelMapping` (referenced by [`APIKeyProfile::model_mappings`]) is defined
//! in `channel.go` and ported in [`crate::objects::channel_settings::ModelMapping`];
//! the field is typed accordingly (OBJ-00 / RUST-P3-005 audit).

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

/// Re-export so callers can refer to the typed mapping without reaching into
/// the `channel_settings` submodule.
pub use crate::objects::channel_settings::ModelMapping;
use serde::{Deserialize, Serialize};

/// Go string-newtype `ChannelTagsMatchMode` (`type ChannelTagsMatchMode string`).
///
/// Controls how the tags on an [`APIKeyProfile`] (or `ProjectProfile`) are
/// combined against a channel's tags. The empty string is a valid Go zero
/// value that means "use the default" (see [`Self::or_default`], which resolves
/// it to [`channel_tags_match_mode::ANY`]).
pub type ChannelTagsMatchMode = String;

/// Constants for [`ChannelTagsMatchMode`].
///
/// Mirrors the Go `const` block:
/// `ChannelTagsMatchModeAny = "any"`, `...All = "all"`, `...None = "none"`.
/// The empty string `""` is also a valid value (Go zero) meaning "default".
pub mod channel_tags_match_mode {
    /// `ChannelTagsMatchModeAny` — a channel matches if it carries **any** of
    /// the allowed tags (logical OR). Also the default mode.
    pub const ANY: &str = "any";
    /// `ChannelTagsMatchModeAll` — a channel matches only if it carries **all**
    /// of the allowed tags (logical AND).
    pub const ALL: &str = "all";
    /// `ChannelTagsMatchModeNone` — a channel matches only if it carries
    /// **none** of the allowed tags (logical NOR / not-any).
    pub const NONE: &str = "none";
}

/// Go string-newtype `APIKeyQuotaPeriodType`.
pub type APIKeyQuotaPeriodType = String;

/// Constants for [`APIKeyQuotaPeriodType`], mirroring the Go `const` block.
pub mod api_key_quota_period_type {
    pub const ALL_TIME: &str = "all_time";
    pub const PAST_DURATION: &str = "past_duration";
    pub const CALENDAR_DURATION: &str = "calendar_duration";
}

/// Go string-newtype `APIKeyQuotaPastDurationUnit`.
pub type APIKeyQuotaPastDurationUnit = String;

/// Constants for [`APIKeyQuotaPastDurationUnit`], mirroring the Go `const` block.
pub mod api_key_quota_past_duration_unit {
    pub const MINUTE: &str = "minute";
    pub const HOUR: &str = "hour";
    pub const DAY: &str = "day";
}

/// Go string-newtype `APIKeyQuotaCalendarDurationUnit`.
pub type APIKeyQuotaCalendarDurationUnit = String;

/// Constants for [`APIKeyQuotaCalendarDurationUnit`], mirroring the Go `const`
/// block.
pub mod api_key_quota_calendar_duration_unit {
    pub const DAY: &str = "day";
    pub const MONTH: &str = "month";
}

/// A collection of named API-key routing profiles with the active one selected.
///
/// Mirrors Go `APIKeyProfiles`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct APIKeyProfiles {
    /// Name of the currently active profile entry.
    #[serde(default)]
    pub active_profile: String,
    /// Available profiles. Go zero-fills (empty slice) when absent.
    #[serde(default)]
    pub profiles: Vec<APIKeyProfile>,
}

/// A single named routing profile for an API key.
///
/// Mirrors Go `APIKeyProfile`. Selector fields (`channel_ids`, `channel_tags`,
/// `model_ids`) and the optional `quota` / `load_balance_strategy` carry Go
/// `omitempty`; an empty profile matches every channel/model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct APIKeyProfile {
    #[serde(default)]
    pub name: String,
    /// Model alias mappings. Typed 1:1 to Go `[]ModelMapping`
    /// (defined in `channel.go`, ported in
    /// [`crate::objects::channel_settings::ModelMapping`]).
    #[serde(default)]
    pub model_mappings: Vec<ModelMapping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<APIKeyQuota>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_balance_strategy: Option<String>,
    /// Maximum whole-request concurrency for this key/profile. Retries share
    /// one slot. `None` or `0` means unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_requests: Option<u32>,

    /// Explicit allow-list of channel IDs. Renamed explicitly to preserve the
    /// internal-capital `IDs` (Go tag) instead of serde's `camelCase` ->
    /// `channelIds`.
    #[serde(default, rename = "channelIDs", skip_serializing_if = "Vec::is_empty")]
    pub channel_ids: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_tags_match_mode: Option<ChannelTagsMatchMode>,
    /// Explicit allow-list of model IDs. Renamed explicitly to preserve the
    /// internal-capital `IDs`.
    #[serde(default, rename = "modelIDs", skip_serializing_if = "Vec::is_empty")]
    pub model_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<DateTime<Utc>>,
}

impl APIKeyProfile {
    /// Returns `true` when this profile selects a channel carrying `tags`.
    ///
    /// Faithfully mirrors Go `(*APIKeyProfile).MatchChannelTags`:
    /// - a nil receiver or empty `channel_tags` matches everything; and
    /// - otherwise the decision is delegated to [`match_channel_tags`].
    pub fn match_channel_tags(&self, tags: &[String]) -> bool {
        if self.channel_tags.is_empty() {
            return true;
        }
        let mode: &str = self.channel_tags_match_mode.as_deref().unwrap_or_default();
        match_channel_tags(&self.channel_tags, mode, tags)
    }

    /// Returns a deep copy of this profile.
    ///
    /// Faithfully mirrors Go `(*APIKeyProfile).Clone` (nil-safe; returns
    /// `None` only via [`Default`] for that case is not needed here because the
    /// Rust receiver is `&self` rather than `Option<&Self>`). For the Rust API
    /// the nil-receiver branch of the Go method is expressed by callers via
    /// `Option<APIKeyProfile>::as_ref().map(|p| p.clone())`; here we simply
    /// clone the owned value. Because every field is owned (no `Arc`/raw
    /// pointer), Rust `Clone` already produces the same independent copy the Go
    /// hand-written `Clone` builds.
    pub fn clone_profile(&self) -> APIKeyProfile {
        self.clone()
    }
}

/// Returns `true` when `channel_tags` satisfies the constraint described by
/// `allowed_tags` + `mode`.
///
/// Faithfully mirrors Go `MatchChannelTags(allowedTags []string, matchMode
/// ChannelTagsMatchMode, channelTags []string) bool`:
///
/// - The mode is first normalized via [`or_default`] (empty / unknown ->
///   `"any"`).
/// - `"all"` (AND): every `allowed_tag` must be present in `channel_tags`.
/// - `"none"` (NOR / not-any): no tag in `channel_tags` may appear in
///   `allowed_tags`.
/// - `"any"` (OR, also the default): at least one tag in `channel_tags` must
///   appear in `allowed_tags`.
///
/// `allowed_tags` is intentionally *not* short-circuited when empty — the Go
/// function only short-circuits via the `(*APIKeyProfile).MatchChannelTags`
/// wrapper (empty `channel_tags` field), and the free function itself iterates
/// `channel_tags` regardless. To preserve Go parity this Rust port does the
/// same: callers that need the "empty allow-list" short-circuit should guard
/// upstream (as the wrapper does).
pub fn match_channel_tags(allowed_tags: &[String], mode: &str, channel_tags: &[String]) -> bool {
    match or_default(mode) {
        channel_tags_match_mode::ALL => {
            // AND: every allowed tag must be present in channel_tags.
            for allowed_tag in allowed_tags {
                if !channel_tags_contains(channel_tags, allowed_tag) {
                    return false;
                }
            }
            true
        }
        channel_tags_match_mode::NONE => {
            // NOR: no channel tag may appear in the allowed set.
            for tag in channel_tags {
                if allowed_tags_contains(allowed_tags, tag) {
                    return false;
                }
            }
            true
        }
        _ => {
            // OR (default / "any"): at least one channel tag must be allowed.
            for tag in channel_tags {
                if allowed_tags_contains(allowed_tags, tag) {
                    return true;
                }
            }
            false
        }
    }
}

/// Mirrors Go `ChannelTagsMatchMode.IsValid`: a mode is valid if it is empty or
/// one of `any` / `all` / `none`.
pub fn is_valid(mode: &str) -> bool {
    mode.is_empty()
        || mode == channel_tags_match_mode::ANY
        || mode == channel_tags_match_mode::ALL
        || mode == channel_tags_match_mode::NONE
}

/// Mirrors Go `ChannelTagsMatchMode.OrDefault`: `"all"` and `"none"` are
/// preserved; everything else (including `""` and any unknown value) falls back
/// to `"any"`.
pub fn or_default(mode: &str) -> &str {
    if mode == channel_tags_match_mode::ALL {
        return channel_tags_match_mode::ALL;
    }
    if mode == channel_tags_match_mode::NONE {
        return channel_tags_match_mode::NONE;
    }
    channel_tags_match_mode::ANY
}

fn allowed_tags_contains(allowed_tags: &[String], tag: &str) -> bool {
    allowed_tags.iter().any(|t| t == tag)
}

fn channel_tags_contains(channel_tags: &[String], tag: &str) -> bool {
    channel_tags.iter().any(|t| t == tag)
}

/// Per-period quota limits for an API-key profile.
///
/// Mirrors Go `APIKeyQuota`. `requests`, `total_tokens`, and `cost` are pointer
/// fields in Go with `omitempty`; `period` is always present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct APIKeyQuota {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Decimal>,
    #[serde(default)]
    pub period: APIKeyQuotaPeriod,
}

/// The quota-reset window description.
///
/// Mirrors Go `APIKeyQuotaPeriod`. `past_duration` and `calendar_duration` are
/// pointer fields in Go with `omitempty` and are mutually exclusive (selected
/// by `period_type`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct APIKeyQuotaPeriod {
    #[serde(default)]
    pub r#type: APIKeyQuotaPeriodType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub past_duration: Option<APIKeyQuotaPastDuration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_duration: Option<APIKeyQuotaCalendarDuration>,
}

impl APIKeyQuotaPeriod {
    /// Deep-clone this period. Mirrors Go `(*APIKeyQuotaPeriod).clone`.
    ///
    /// Rust `Clone` already yields an independent copy; this helper exists so
    /// the Go method's shape is explicit at the call site.
    pub fn clone_period(&self) -> APIKeyQuotaPeriod {
        self.clone()
    }
}

/// A rolling-window quota duration (e.g. "last 24 hours").
///
/// Mirrors Go `APIKeyQuotaPastDuration`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct APIKeyQuotaPastDuration {
    #[serde(default)]
    pub value: i64,
    #[serde(default)]
    pub unit: APIKeyQuotaPastDurationUnit,
}

/// A calendar-aligned quota duration (e.g. "current month").
///
/// Mirrors Go `APIKeyQuotaCalendarDuration`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct APIKeyQuotaCalendarDuration {
    #[serde(default)]
    pub unit: APIKeyQuotaCalendarDurationUnit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use serde_json::json;

    #[test]
    fn model_mappings_field_is_typed_round_trip() -> Result<(), serde_json::Error> {
        // OBJ-00 audit regression: `APIKeyProfile::model_mappings` must be a
        // typed `Vec<ModelMapping>` (Go `[]ModelMapping`), not bare
        // `serde_json::Value`. The Go struct uses snake_case `from`/`to` tags;
        // verify they survive a Rust round-trip exactly.
        let payload = json!({
            "name": "p",
            "modelMappings": [
                {"from": "gpt-4", "to": "gpt-4o"},
                {"from": "claude", "to": "claude-opus"}
            ]
        });
        let p: APIKeyProfile = serde_json::from_value(payload.clone())?;
        assert_eq!(p.model_mappings.len(), 2);
        assert_eq!(p.model_mappings[0].from, "gpt-4");
        assert_eq!(p.model_mappings[0].to, "gpt-4o");
        assert_eq!(p.model_mappings[1].from, "claude");
        assert_eq!(p.model_mappings[1].to, "claude-opus");

        // Re-serialize and confirm snake_case `from`/`to` tags are preserved
        // (ModelMapping does NOT use camelCase rename_all — Go tags are bare
        // `from`/`to`).
        let out = serde_json::to_value(&p)?;
        assert_eq!(out, payload);
        Ok(())
    }

    #[test]
    fn api_key_profiles_round_trip() -> Result<(), serde_json::Error> {
        let payload = json!({
            "activeProfile": "prod",
            "profiles": [
                {
                    "name": "prod",
                    "modelMappings": [{"from": "gpt-4", "to": "gpt-4o"}],
                    "quota": {
                        "requests": 1000,
                        "totalTokens": 5000000,
                        "cost": "12.34",
                        "period": {
                            "type": "past_duration",
                            "pastDuration": {"value": 24, "unit": "hour"}
                        }
                    },
                    "loadBalanceStrategy": "weighted",
                    "channelIDs": [1, 2],
                    "channelTags": ["gpu", "cheap"],
                    "channelTagsMatchMode": "all",
                    "modelIDs": ["gpt-4o", "claude"]
                },
                {
                    "name": "empty"
                }
            ]
        });

        let parsed: APIKeyProfiles = serde_json::from_str(&payload.to_string())?;
        assert_eq!(parsed.active_profile, "prod");
        assert_eq!(parsed.profiles.len(), 2);

        let first = &parsed.profiles[0];
        assert_eq!(first.name, "prod");
        assert_eq!(first.model_mappings.len(), 1);
        assert_eq!(first.model_mappings[0].from, "gpt-4");
        assert_eq!(first.model_mappings[0].to, "gpt-4o");
        match &first.quota {
            Some(q) => {
                assert_eq!(q.requests, Some(1000_i64));
                assert_eq!(q.total_tokens, Some(5_000_000_i64));
                assert_eq!(q.cost, Some(Decimal::new(1234, 2)));
                assert_eq!(q.period.r#type, "past_duration");
                match &q.period.past_duration {
                    Some(pd) => {
                        assert_eq!(pd.value, 24);
                        assert_eq!(pd.unit, "hour");
                    }
                    None => panic!("expected past_duration"),
                }
                assert!(q.period.calendar_duration.is_none());
            }
            None => panic!("expected quota"),
        }
        assert_eq!(first.load_balance_strategy.as_deref(), Some("weighted"));
        assert_eq!(first.channel_ids, vec![1_i64, 2]);
        assert_eq!(
            first.channel_tags,
            vec!["gpu".to_string(), "cheap".to_string()]
        );
        assert_eq!(first.channel_tags_match_mode.as_deref(), Some("all"));
        assert_eq!(
            first.model_ids,
            vec!["gpt-4o".to_string(), "claude".to_string()]
        );

        let second = &parsed.profiles[1];
        assert_eq!(second.name, "empty");
        assert!(second.model_mappings.is_empty());
        assert!(second.quota.is_none());
        assert!(second.load_balance_strategy.is_none());
        assert!(second.channel_ids.is_empty());
        assert!(second.channel_tags.is_empty());
        assert!(second.channel_tags_match_mode.is_none());
        assert!(second.model_ids.is_empty());

        // Re-serialize and assert omitempty drops empty selector keys on the
        // empty profile, while preserving internal-capital `channelIDs` /
        // `modelIDs` tags on the rich one.
        let out = serde_json::to_value(&parsed)?;
        let first_out = out
            .get("profiles")
            .and_then(|v| v.get(0))
            .ok_or_else(|| serde::de::Error::custom("missing profiles[0]"))?;
        assert_eq!(first_out.get("channelIDs"), Some(&json!([1, 2])));
        assert_eq!(
            first_out.get("modelIDs"),
            Some(&json!(["gpt-4o", "claude"]))
        );
        assert_eq!(first_out.get("channelTagsMatchMode"), Some(&json!("all")));

        let second_out = out
            .get("profiles")
            .and_then(|v| v.get(1))
            .ok_or_else(|| serde::de::Error::custom("missing profiles[1]"))?;
        assert!(second_out.get("channelIDs").is_none());
        assert!(second_out.get("channelTags").is_none());
        assert!(second_out.get("channelTagsMatchMode").is_none());
        assert!(second_out.get("modelIDs").is_none());
        assert!(second_out.get("quota").is_none());
        Ok(())
    }

    #[test]
    fn api_key_quota_all_time_round_trip() -> Result<(), serde_json::Error> {
        let payload = json!({
            "requests": 10,
            "period": {"type": "all_time"}
        });
        let q: APIKeyQuota = serde_json::from_value(payload.clone())?;
        assert_eq!(q.requests, Some(10));
        assert_eq!(q.total_tokens, None);
        assert_eq!(q.cost, None);
        assert_eq!(q.period.r#type, "all_time");
        assert!(q.period.past_duration.is_none());
        assert!(q.period.calendar_duration.is_none());

        // `total_tokens` / `cost` are None and must drop on re-serialize.
        let out = serde_json::to_value(&q)?;
        assert_eq!(out.get("requests"), Some(&json!(10)));
        assert!(out.get("totalTokens").is_none());
        assert!(out.get("cost").is_none());
        assert_eq!(out.get("period"), Some(&json!({"type": "all_time"})));
        Ok(())
    }

    #[test]
    fn api_key_quota_calendar_duration_round_trip() -> Result<(), serde_json::Error> {
        let payload = json!({
            "period": {
                "type": "calendar_duration",
                "calendarDuration": {"unit": "month"}
            }
        });
        let q: APIKeyQuota = serde_json::from_value(payload.clone())?;
        assert_eq!(q.period.r#type, "calendar_duration");
        match &q.period.calendar_duration {
            Some(cd) => assert_eq!(cd.unit, "month"),
            None => panic!("expected calendar_duration"),
        }
        assert!(q.period.past_duration.is_none());

        let out = serde_json::to_value(&q)?;
        assert_eq!(out, payload);
        Ok(())
    }

    // ---- match_channel_tags semantics -------------------------------------

    fn tags(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn mct_any_matches_on_shared_tag() {
        let allowed = tags(&["gpu", "cheap"]);
        let channel = tags(&["cheap", "cpu"]);
        assert!(match_channel_tags(&allowed, "any", &channel));
        // default mode (empty) behaves as "any".
        assert!(match_channel_tags(&allowed, "", &channel));
        // unknown mode also falls back to "any".
        assert!(match_channel_tags(&allowed, "banana", &channel));
    }

    #[test]
    fn mct_any_no_overlap_returns_false() {
        let allowed = tags(&["gpu"]);
        let channel = tags(&["cpu", "tpu"]);
        assert!(!match_channel_tags(&allowed, "any", &channel));
    }

    #[test]
    fn mct_all_requires_every_allowed_tag() {
        let allowed = tags(&["gpu", "cheap"]);
        // channel has both -> match.
        assert!(match_channel_tags(
            &allowed,
            "all",
            &tags(&["gpu", "cheap", "fast"])
        ));
        // channel missing one -> no match.
        assert!(!match_channel_tags(
            &allowed,
            "all",
            &tags(&["gpu", "fast"])
        ));
    }

    #[test]
    fn mct_none_rejects_any_overlap() {
        let allowed = tags(&["gpu", "cheap"]);
        // disjoint -> match (none of the allowed are present).
        assert!(match_channel_tags(&allowed, "none", &tags(&["cpu", "tpu"])));
        // overlap -> no match.
        assert!(!match_channel_tags(
            &allowed,
            "none",
            &tags(&["cpu", "gpu"])
        ));
    }

    #[test]
    fn mct_empty_channel_tags_is_neither_allowed_nor_overlapping() {
        let allowed = tags(&["gpu"]);
        let empty: Vec<String> = Vec::new();
        // any (OR) with empty channel_tags -> no tag matches -> false.
        assert!(!match_channel_tags(&allowed, "any", &empty));
        // all (AND) with non-empty allowed + empty channel_tags: the allowed
        // tag is absent from the channel set -> false (matches Go).
        assert!(!match_channel_tags(&allowed, "all", &empty));
        // none (NOR) with empty channel_tags -> no overlap -> true.
        assert!(match_channel_tags(&allowed, "none", &empty));
    }

    // ---- APIKeyProfile::match_channel_tags wrapper -------------------------

    #[test]
    fn profile_match_channel_tags_empty_short_circuit() {
        // Empty channel_tags field short-circuits to true regardless of mode,
        // mirroring Go (*APIKeyProfile).MatchChannelTags.
        let empty = APIKeyProfile::default();
        assert!(empty.match_channel_tags(&tags(&["anything"])));

        let tagged_any = APIKeyProfile {
            channel_tags: tags(&["gpu"]),
            channel_tags_match_mode: Some("any".to_string()),
            ..Default::default()
        };
        assert!(tagged_any.match_channel_tags(&tags(&["gpu", "cpu"])));
        assert!(!tagged_any.match_channel_tags(&tags(&["cpu"])));
    }

    // ---- mode helpers ------------------------------------------------------

    #[test]
    fn mode_helpers_match_go() {
        // is_valid
        assert!(is_valid(""));
        assert!(is_valid("any"));
        assert!(is_valid("all"));
        assert!(is_valid("none"));
        assert!(!is_valid("banana"));

        // or_default
        assert_eq!(or_default(""), "any");
        assert_eq!(or_default("any"), "any");
        assert_eq!(or_default("all"), "all");
        assert_eq!(or_default("none"), "none");
        assert_eq!(or_default("banana"), "any");
    }

    #[test]
    fn constants_match_go_literals() {
        // Guards against typos in the constants modules.
        assert_eq!(channel_tags_match_mode::ANY, "any");
        assert_eq!(channel_tags_match_mode::ALL, "all");
        assert_eq!(channel_tags_match_mode::NONE, "none");

        assert_eq!(api_key_quota_period_type::ALL_TIME, "all_time");
        assert_eq!(api_key_quota_period_type::PAST_DURATION, "past_duration");
        assert_eq!(
            api_key_quota_period_type::CALENDAR_DURATION,
            "calendar_duration"
        );

        assert_eq!(api_key_quota_past_duration_unit::MINUTE, "minute");
        assert_eq!(api_key_quota_past_duration_unit::HOUR, "hour");
        assert_eq!(api_key_quota_past_duration_unit::DAY, "day");

        assert_eq!(api_key_quota_calendar_duration_unit::DAY, "day");
        assert_eq!(api_key_quota_calendar_duration_unit::MONTH, "month");
    }
}
