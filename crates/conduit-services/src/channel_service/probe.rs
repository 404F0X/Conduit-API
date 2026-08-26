//! Channel-probe pure decision layer.
//!
//! Minimal port of Go `internal/server/biz/channel_probe.go`'s pure helpers
//! (`shouldRunProbe`, `getIntervalMinutesFromFrequency`) plus the
//! `ChannelProbeSetting` policy type and its
//! `GetIntervalMinutes` / `GetQueryRangeMinutes` table from
//! `internal/server/biz/system.go:509-558`.
//!
//! This seam owns the **decision** of *whether* the probe should run for the
//! current tick and *what window* it should query. The actual probe execution
//! (DB CTE over `request_executions`, token-per-second math, persistence of
//! `ChannelProbePoint`) is DB-bound and lives in the host crate; it is out of
//! scope here. The table values are byte-exact with the Go implementation so
//! scheduler ticks align across ports.

use serde::{Deserialize, Serialize};

/// Probe cadence selected by the system config. Mirrors Go's
/// `ProbeFrequency` string enum (`internal/server/biz/system.go:509-517`).
///
/// Values are the literal wire strings (`"1m"`, `"5m"`, `"30m"`, `"1h"`) so
/// serde parity with Go's JSON is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ProbeFrequency {
    /// `"1m"` — every minute. (Default, matching Go's `default` branch
    /// fallback.)
    #[serde(rename = "1m")]
    #[default]
    OneMinute,
    /// `"5m"` — every 5 minutes.
    #[serde(rename = "5m")]
    FiveMinutes,
    /// `"30m"` — every 30 minutes.
    #[serde(rename = "30m")]
    ThirtyMinutes,
    /// `"1h"` — every hour.
    #[serde(rename = "1h")]
    OneHour,
}

impl ProbeFrequency {
    /// Literal wire string used by Go (`"1m"`, `"5m"`, `"30m"`, `"1h"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OneMinute => "1m",
            Self::FiveMinutes => "5m",
            Self::ThirtyMinutes => "30m",
            Self::OneHour => "1h",
        }
    }

    /// Parse a wire string into a [`ProbeFrequency`]. Unknown strings fall
    /// back to [`ProbeFrequency::OneMinute`], matching Go's `default` branch
    /// in `getIntervalMinutesFromFrequency`.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "1m" => Self::OneMinute,
            "5m" => Self::FiveMinutes,
            "30m" => Self::ThirtyMinutes,
            "1h" => Self::OneHour,
            _ => Self::OneMinute,
        }
    }
}

impl From<ProbeFrequency> for &'static str {
    fn from(f: ProbeFrequency) -> Self {
        f.as_str()
    }
}

/// Channel-probe configuration snapshot. Mirrors Go's `ChannelProbeSetting`
/// (`internal/server/biz/system.go:519-525`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelProbeSetting {
    /// Whether channel probing is active.
    pub enabled: bool,
    /// How often channels are probed.
    pub frequency: ProbeFrequency,
}

impl ChannelProbeSetting {
    /// Build a setting from its raw wire form (`enabled` + frequency string).
    /// Mirrors the implicit JSON construction on the Go side.
    pub fn new(enabled: bool, frequency: ProbeFrequency) -> Self {
        Self { enabled, frequency }
    }

    /// Probe interval in minutes. Mirrors Go
    /// `ChannelProbeSetting.GetIntervalMinutes`
    /// (`internal/server/biz/system.go:544-558`). Unknown frequencies fall
    /// back to 1 minute (Go `default`).
    pub fn interval_minutes(&self) -> i64 {
        interval_minutes_for_frequency(self.frequency)
    }

    /// Query-window length in minutes for the probe's DB CTE. Mirrors Go
    /// `ChannelProbeSetting.GetQueryRangeMinutes`
    /// (`internal/server/biz/system.go:527-542`): `1m -> 10min`,
    /// `5m -> 60min`, `30m -> 720min` (12h), `1h -> 1440min` (24h). Unknown
    /// frequencies fall back to 10 minutes.
    pub fn query_range_minutes(&self) -> i64 {
        match self.frequency {
            ProbeFrequency::OneMinute => 10,
            ProbeFrequency::FiveMinutes => 60,
            ProbeFrequency::ThirtyMinutes => 720,
            ProbeFrequency::OneHour => 1440,
        }
    }
}

/// Interval in minutes for a given frequency. Mirrors Go
/// `getIntervalMinutesFromFrequency` (`channel_probe.go:91-104`).
pub fn interval_minutes_for_frequency(frequency: ProbeFrequency) -> i64 {
    match frequency {
        ProbeFrequency::OneMinute => 1,
        ProbeFrequency::FiveMinutes => 5,
        ProbeFrequency::ThirtyMinutes => 30,
        ProbeFrequency::OneHour => 60,
    }
}

/// Decide whether a probe tick should fire for the given cadence.
///
/// Mirrors Go `shouldRunProbe` (`channel_probe.go:83-88`): the current time
/// is aligned down to the interval boundary, and the probe runs iff that
/// aligned timestamp differs from the last execution's aligned timestamp.
///
/// Inputs are unix-epoch seconds (UTC). The host passes `xtime.UTCNow()` and
/// the persisted `lastExecutionTime` from the probe service.
///
/// Pure: no IO, no clock dependency beyond the args.
pub fn should_run_probe(
    frequency: ProbeFrequency,
    now_unix: i64,
    last_execution_unix: i64,
) -> bool {
    let interval_secs = interval_minutes_for_frequency(frequency) * 60;
    if interval_secs <= 0 {
        // Defensive — never divide by zero. The table above never yields 0,
        // but guarding keeps the function total.
        return true;
    }
    let aligned_now = now_unix - (now_unix.rem_euclid(interval_secs));
    let aligned_last = last_execution_unix - (last_execution_unix.rem_euclid(interval_secs));
    aligned_now != aligned_last
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- Parity with Go channel_probe.go / system.go ----------

    #[test]
    fn s12_interval_minutes_matches_go_table() {
        // Go system.go:544-558 + channel_probe.go:91-104.
        assert_eq!(interval_minutes_for_frequency(ProbeFrequency::OneMinute), 1);
        assert_eq!(
            interval_minutes_for_frequency(ProbeFrequency::FiveMinutes),
            5
        );
        assert_eq!(
            interval_minutes_for_frequency(ProbeFrequency::ThirtyMinutes),
            30
        );
        assert_eq!(interval_minutes_for_frequency(ProbeFrequency::OneHour), 60);
    }

    #[test]
    fn s12_query_range_minutes_matches_go_table() {
        // Go system.go:527-542.
        let one_min = ChannelProbeSetting::new(true, ProbeFrequency::OneMinute);
        assert_eq!(one_min.query_range_minutes(), 10);
        let five_min = ChannelProbeSetting::new(true, ProbeFrequency::FiveMinutes);
        assert_eq!(five_min.query_range_minutes(), 60);
        let thirty_min = ChannelProbeSetting::new(true, ProbeFrequency::ThirtyMinutes);
        assert_eq!(thirty_min.query_range_minutes(), 720);
        let one_hour = ChannelProbeSetting::new(true, ProbeFrequency::OneHour);
        assert_eq!(one_hour.query_range_minutes(), 1440);
    }

    #[test]
    fn s12_should_run_probe_first_tick_always_fires() {
        // First-ever tick: last_execution = 0 is aligned to 0, so any non-zero
        // aligned now differs (mirrors Go's zero-time behaviour).
        let now = 1_700_000_000; // arbitrary unix ts
        assert!(should_run_probe(ProbeFrequency::OneMinute, now, 0));
        assert!(should_run_probe(ProbeFrequency::OneHour, now, 0));
    }

    #[test]
    fn s12_should_run_probe_same_interval_does_not_refire() {
        // Go channel_probe.go:83-88: aligned-time equality suppresses re-fire.
        let now = 1_700_000_000;
        let interval_secs = interval_minutes_for_frequency(ProbeFrequency::FiveMinutes) * 60;
        // Pretend the scheduler ticked `interval_secs` ago (same aligned bucket).
        let last = now - (interval_secs / 2);
        assert!(!should_run_probe(ProbeFrequency::FiveMinutes, now, last));
    }

    #[test]
    fn s12_should_run_probe_next_interval_fires() {
        let now = 1_700_000_000;
        let interval_secs = interval_minutes_for_frequency(ProbeFrequency::FiveMinutes) * 60;
        // Last execution was one full interval ago: aligned timestamps differ.
        let last = now - interval_secs;
        assert!(should_run_probe(ProbeFrequency::FiveMinutes, now, last));
    }

    #[test]
    fn s12_frequency_round_trips_through_wire_string() -> Result<(), serde_json::Error> {
        // Wire strings must match Go's `ProbeFrequency` constants byte-for-byte.
        for (freq, raw) in [
            (ProbeFrequency::OneMinute, "\"1m\""),
            (ProbeFrequency::FiveMinutes, "\"5m\""),
            (ProbeFrequency::ThirtyMinutes, "\"30m\""),
            (ProbeFrequency::OneHour, "\"1h\""),
        ] {
            let serialized = serde_json::to_string(&freq)?;
            assert_eq!(serialized, raw, "serialize mismatch for {raw:?}");
            let deserialized: ProbeFrequency = serde_json::from_str(raw)?;
            assert_eq!(deserialized, freq, "deserialize mismatch for {raw:?}");
        }
        Ok(())
    }

    #[test]
    fn s12_unknown_frequency_string_falls_back_to_one_minute() {
        // Mirrors Go's `default` branch — unknown strings yield the 1-minute
        // cadence.
        assert_eq!(ProbeFrequency::parse("unknown"), ProbeFrequency::OneMinute);
        assert_eq!(ProbeFrequency::parse(""), ProbeFrequency::OneMinute);
    }
}
