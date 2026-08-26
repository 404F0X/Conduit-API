//! API-key quota service — Rust port of `conduit/internal/server/biz/quota.go`.
//!
//! Covers the three concerns of the Go `QuotaService`:
//! 1. **Period/window computation** ([`QuotaPeriod::window`], [`QuotaWindow`]) —
//!    faithfully mirrors Go `quotaWindow`. `all_time` and `past_duration` yield a
//!    rolling window with an **inclusive** end (`created_at <= now`); a
//!    `calendar_duration` yields a calendar-aligned bucket whose end is
//!    **exclusive** (`created_at < start_of_next_bucket`). This inclusive/exclusive
//!    distinction is carried by [`QuotaWindow::end_inclusive`] and matches the Go
//!    `EndInclusive` flag plus the `TestQuotaService_CalendarDuration_ExcludesUsageAtWindowEnd`
//!    regression.
//! 2. **Three-dimension limit check** (requests / total_tokens / cost) — mirrors
//!    Go `(*QuotaService).CheckAPIKeyQuota`: aggregates usage over the window and
//!    rejects when current usage `>= limit` (Go uses `>=` so an exhausted quota
//!    stays exhausted until the window rolls). [`QuotaService::check_policy`]
//!    keeps the `attempted > limit` variant (pre-increment check, used by the
//!    ad-hoc policy path), while [`QuotaService::check_api_key_quota`] is the
//!    entry point that matches Go's pre-request gate semantics exactly.
//! 3. **`quota_exhausted` mapping** ([`QuotaError`] → [`ConduitError`]) — Go renders
//!    the exhausted case as a 429 `quota_exhausted`; the Rust side surfaces it via
//!    [`ConduitError::quota_exhausted`].
//!
//! The repo dependencies ([`QuotaUsageRepo`]) are abstracted as an `async_trait`
//! so the pure logic is testable with the in-memory implementation
//! ([`InMemoryQuotaUsageRepo`]).

use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, FixedOffset, TimeZone, Utc};
use conduit_core::objects::apikey::{
    APIKeyQuota, APIKeyQuotaPeriod as CoreAPIKeyQuotaPeriod, api_key_quota_calendar_duration_unit,
    api_key_quota_past_duration_unit, api_key_quota_period_type,
};
use conduit_core::{ConduitError, ErrorKind};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type QuotaResult<T> = Result<T, QuotaError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum QuotaError {
    #[error(
        "quota exhausted for policy {policy_id}: {limit} limit {limit_value} would become {attempted_value}"
    )]
    Exceeded {
        policy_id: String,
        limit: QuotaLimitKind,
        limit_value: String,
        attempted_value: String,
        period: QuotaPeriod,
        window: QuotaWindow,
    },
    #[error("quota period amount must be greater than zero")]
    InvalidPeriodAmount,
    #[error("quota calendar window is out of range")]
    InvalidCalendarWindow,
    #[error("quota period is malformed: {0}")]
    InvalidPeriod(String),
}

impl From<QuotaError> for ConduitError {
    fn from(err: QuotaError) -> Self {
        let message = err.to_string();
        match err {
            QuotaError::Exceeded { .. } => ConduitError::quota_exhausted(message).with_source(err),
            QuotaError::InvalidPeriodAmount
            | QuotaError::InvalidCalendarWindow
            | QuotaError::InvalidPeriod(_) => {
                ConduitError::new(ErrorKind::InvalidRequest, message).with_source(err)
            }
        }
    }
}

/// Pure-logic quota evaluator plus async repo-driven entry points.
///
/// The synchronous helpers ([`Self::period_window`], [`Self::check_policy`],
/// [`Self::check_profile`]) operate on already-aggregated usage and are unit-test
/// friendly. [`Self::check_api_key_quota`] mirrors Go
/// `(*QuotaService).CheckAPIKeyQuota` and pulls live usage from a
/// [`QuotaUsageRepo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaService;

impl QuotaService {
    pub fn new() -> Self {
        Self
    }

    /// Compute the time window for `period` at `now`. Mirrors Go `quotaWindow`.
    pub fn period_window(
        &self,
        period: &QuotaPeriod,
        now: DateTime<Utc>,
    ) -> QuotaResult<QuotaWindow> {
        period.window(now)
    }

    /// Evaluate one [`QuotaPolicy`] against `current` + `increment` usage. Each
    /// configured limit is rejected only when the **attempted** total strictly
    /// exceeds it (pre-increment check). For the Go pre-request gate that uses
    /// `current >= limit`, see [`Self::check_api_key_quota`].
    pub fn check_policy(
        &self,
        policy: &QuotaPolicy,
        current: &QuotaUsage,
        increment: &QuotaUsage,
        now: DateTime<Utc>,
    ) -> QuotaResult<QuotaCheck> {
        let window = policy.period.window(now)?;
        let attempted = current.checked_add(increment);

        if let Some(limit) = policy.max_requests
            && attempted.requests > limit
        {
            return Err(QuotaError::Exceeded {
                policy_id: policy.id.clone(),
                limit: QuotaLimitKind::Requests,
                limit_value: limit.to_string(),
                attempted_value: attempted.requests.to_string(),
                period: policy.period.clone(),
                window,
            });
        }

        if let Some(limit) = policy.max_tokens
            && attempted.tokens > limit
        {
            return Err(QuotaError::Exceeded {
                policy_id: policy.id.clone(),
                limit: QuotaLimitKind::Tokens,
                limit_value: limit.to_string(),
                attempted_value: attempted.tokens.to_string(),
                period: policy.period.clone(),
                window,
            });
        }

        if let Some(limit) = policy.max_cost
            && attempted.cost > limit
        {
            return Err(QuotaError::Exceeded {
                policy_id: policy.id.clone(),
                limit: QuotaLimitKind::Cost,
                limit_value: decimal_to_string(limit),
                attempted_value: decimal_to_string(attempted.cost),
                period: policy.period.clone(),
                window,
            });
        }

        Ok(QuotaCheck {
            policy_id: policy.id.clone(),
            window,
            attempted,
        })
    }

    pub fn check_profile(
        &self,
        profile: &QuotaProfile,
        current: &QuotaUsage,
        increment: &QuotaUsage,
        now: DateTime<Utc>,
    ) -> QuotaResult<Vec<QuotaCheck>> {
        if !profile.enabled {
            return Ok(Vec::new());
        }

        profile
            .policies
            .iter()
            .map(|policy| self.check_policy(policy, current, increment, now))
            .collect()
    }

    /// Pre-request API-key quota gate — mirrors Go
    /// `(*QuotaService).CheckAPIKeyQuota`. Computes the window for `quota`,
    /// pulls current usage from `repo`, and rejects when current usage is `>=`
    /// any configured limit (Go semantics: an exhausted quota stays exhausted
    /// until the window rolls forward).
    ///
    /// Returns `Ok(None)` when `quota` is `None` (Go returns `{Allowed: true}`
    /// for a nil quota).
    ///
    /// Uses **UTC** as the system timezone for `calendar_duration` bucket
    /// alignment. Callers that have resolved a non-UTC system timezone (Go:
    /// `s.system.TimeLocation(ctx)`) should use
    /// [`Self::check_api_key_quota_in_offset`] instead.
    pub async fn check_api_key_quota(
        &self,
        repo: &dyn QuotaUsageRepo,
        api_key_id: &str,
        quota: Option<&APIKeyQuota>,
        now: DateTime<Utc>,
    ) -> QuotaResult<Option<QuotaCheck>> {
        let utc = match FixedOffset::east_opt(0) {
            Some(off) => off,
            None => return Err(QuotaError::InvalidCalendarWindow),
        };
        self.check_api_key_quota_in_offset(repo, api_key_id, quota, now, utc)
            .await
    }

    /// Timezone-aware variant of [`Self::check_api_key_quota`]. Mirrors Go
    /// `(*QuotaService).CheckAPIKeyQuota` which threads
    /// `s.system.TimeLocation(ctx)` into `quotaWindow`. `offset` only affects
    /// the `calendar_duration` window boundaries; `all_time` / `past_duration`
    /// are timezone-independent (see [`QuotaPeriod::window_in_offset`]).
    pub async fn check_api_key_quota_in_offset(
        &self,
        repo: &dyn QuotaUsageRepo,
        api_key_id: &str,
        quota: Option<&APIKeyQuota>,
        now: DateTime<Utc>,
        offset: FixedOffset,
    ) -> QuotaResult<Option<QuotaCheck>> {
        let Some(quota) = quota else {
            return Ok(None);
        };

        let period = QuotaPeriod::from_core(&quota.period)?;
        let window = period.window_in_offset(now, offset)?;
        let policy_id = format!("api-key/{api_key_id}");

        // Requests dimension (separate count query, mirrors Go `requestCount`).
        // Query the count unconditionally when the dimension is configured so
        // the result is available both for the limit check and for surfacing
        // on `QuotaCheck.attempted.requests` in the early-return path below.
        let requests_count = if quota.requests.is_some() {
            repo.request_count(api_key_id, &window)
                .await
                .map_err(|e| QuotaError::InvalidPeriod(e.to_string()))?
        } else {
            0
        };

        if let Some(limit) = quota.requests {
            // Go `quota.Requests` is `*int64`; a negative configured limit
            // makes `reqCount >= limit` always false (never trips). Mirror
            // that — only enforce when the limit is non-negative. (The prior
            // `unwrap_or(0)` coerced negatives to 0, which always tripped —
            // the opposite of Go semantics.)
            if limit >= 0 {
                let limit_u = u64::try_from(limit).unwrap_or(0);
                if requests_count >= limit_u {
                    return Err(QuotaError::Exceeded {
                        policy_id,
                        limit: QuotaLimitKind::Requests,
                        limit_value: limit.to_string(),
                        attempted_value: requests_count.to_string(),
                        period,
                        window,
                    });
                }
            }
        }

        // Tokens / cost dimensions share the usage aggregate (Go `usageAgg`).
        let need_tokens = quota.total_tokens.is_some();
        let need_cost = quota.cost.is_some();
        if !need_tokens && !need_cost {
            // Only the requests dimension was configured; mirror Go's early
            // return but surface the already-queried request count on
            // `attempted.requests` so callers see real usage rather than a
            // zero default (Go's `QuotaCheckResult` does not carry an
            // attempted field, but the Rust `QuotaCheck` does).
            return Ok(Some(QuotaCheck {
                policy_id,
                window,
                attempted: QuotaUsage::new(requests_count, 0, Decimal::ZERO),
            }));
        }

        let agg = repo
            .usage_aggregate(api_key_id, &window)
            .await
            .map_err(|e| QuotaError::InvalidPeriod(e.to_string()))?;

        if let (Some(limit), tokens) = (quota.total_tokens, agg.tokens) {
            // Negative `*int64` limit ⇒ never trips (Go parity, see requests
            // dimension above).
            if limit >= 0 {
                let limit_u = u64::try_from(limit).unwrap_or(0);
                if tokens >= limit_u {
                    return Err(QuotaError::Exceeded {
                        policy_id,
                        limit: QuotaLimitKind::Tokens,
                        limit_value: limit.to_string(),
                        attempted_value: tokens.to_string(),
                        period,
                        window,
                    });
                }
            }
        }

        if let (Some(limit), cost) = (quota.cost, agg.cost)
            && cost >= limit
        {
            return Err(QuotaError::Exceeded {
                policy_id,
                limit: QuotaLimitKind::Cost,
                limit_value: decimal_to_string(limit),
                attempted_value: decimal_to_string(cost),
                period,
                window,
            });
        }

        Ok(Some(QuotaCheck {
            policy_id,
            window,
            attempted: QuotaUsage::new(agg.requests, agg.tokens, agg.cost),
        }))
    }

    // ---- S10: dashboard / display path (Go GetQuota + ProfileQuotaUsages) ----
    //
    // Go exposes a second, NON-gating entry point on `*QuotaService` that the
    // admin + OpenAPI GraphQL resolvers consume for the dashboard "API key
    // quota usage" widget:
    //
    //   func (s *QuotaService) GetQuota(ctx, apiKeyID, quota) (QuotaResult, error)
    //   func (s *QuotaService) ProfileQuotaUsages(ctx, apiKey) ([]ProfileQuotaUsage, error)
    //
    // The two differ from `CheckAPIKeyQuota` in three load-bearing ways:
    // 1. They NEVER reject. Even when the configured limit is already reached,
    //    Go returns the aggregate + window so the UI can render "120 / 100
    //    (exhausted)". The Rust gate-side `check_api_key_quota` returns
    //    `Err(QuotaError::Exceeded)` for the same input.
    // 2. They ALWAYS query both aggregates — `requestCount` AND `usageAgg`
    //    (Go: `needTokens=true, needCost=true`), regardless of which dimensions
    //    are configured on `quota`. This is why the widget can show a token
    //    total even on a quota that only gates on `requests`.
    // 3. They have no short-circuit when `quota == nil` for `ProfileQuotaUsages`
    //    (it simply iterates profiles with non-nil quota); `GetQuota(nil)`
    //    returns an empty `QuotaResult` rather than `Allowed: true`.
    //
    // The Rust ports below mirror these semantics exactly so the dashboard
    // pipeline (`ProfileQuotaUsages` -> `APIKeyProfileQuotaUsage[]` GraphQL
    // type) renders the same shape the React frontend expects.

    /// Dashboard usage result for one API-key quota. Mirrors Go `QuotaResult`
    /// (`{Window, Usage}`) — distinct from [`QuotaCheck`] (which carries the
    /// `policy_id` + `attempted` fields the pre-request gate surfaces).
    ///
    /// (The [`QuotaUsageSnapshot`] / [`ProfileQuotaUsage`] structs are defined
    /// at module scope — Rust does not let `impl`-block items appear in
    /// sibling function signatures inside the same block.)
    ///
    /// Read-only dashboard aggregate for one API-key quota. Mirrors Go
    /// `(*QuotaService).GetQuota`: computes the window in `offset`, queries
    /// `requestCount` and `usageAgg` UNCONDITIONALLY (both dimensions on,
    /// regardless of which limits the quota actually configures), and returns
    /// the snapshot WITHOUT any limit check. The dashboard renders the
    /// "current / limit" comparison itself from the [`APIKeyQuota`] +
    /// [`QuotaUsage`] pair.
    ///
    /// Returns `Ok(None)` when `quota` is `None` (Go returns an empty
    /// `QuotaResult{}` for a nil quota; `None` is the idiomatic Rust shape and
    /// the caller — [`Self::profile_quota_usages`] — filters it out).
    pub async fn get_quota_in_offset(
        &self,
        repo: &dyn QuotaUsageRepo,
        api_key_id: &str,
        quota: Option<&APIKeyQuota>,
        now: DateTime<Utc>,
        offset: FixedOffset,
    ) -> QuotaResult<Option<QuotaUsageSnapshot>> {
        let Some(quota) = quota else {
            return Ok(None);
        };

        let period = QuotaPeriod::from_core(&quota.period)?;
        let window = period.window_in_offset(now, offset)?;

        // Go GetQuota fires both queries unconditionally (needTokens=true,
        // needCost=true) and via RunWithSystemBypass. The two calls are
        // independent; we run them sequentially to keep the lib build free of
        // the `tokio` dev-dependency (a future caller that wants concurrency
        // can `.spawn` the two repo futures itself — the trait is already
        // `Send`+`Sync`).
        let req_count = repo
            .request_count(api_key_id, &window)
            .await
            .map_err(|e| QuotaError::InvalidPeriod(e.to_string()))?;
        let agg = repo
            .usage_aggregate(api_key_id, &window)
            .await
            .map_err(|e| QuotaError::InvalidPeriod(e.to_string()))?;

        Ok(Some(QuotaUsageSnapshot {
            window,
            usage: QuotaUsage::new(req_count, agg.tokens, agg.cost),
        }))
    }

    /// UTC-default wrapper around [`Self::get_quota_in_offset`] — matches Go's
    /// `time.UTC` fallback when `SystemService.TimeLocation` resolves to UTC
    /// (settings missing / empty / unparseable timezone string).
    pub async fn get_quota(
        &self,
        repo: &dyn QuotaUsageRepo,
        api_key_id: &str,
        quota: Option<&APIKeyQuota>,
        now: DateTime<Utc>,
    ) -> QuotaResult<Option<QuotaUsageSnapshot>> {
        let utc = match FixedOffset::east_opt(0) {
            Some(off) => off,
            None => return Err(QuotaError::InvalidCalendarWindow),
        };
        self.get_quota_in_offset(repo, api_key_id, quota, now, utc)
            .await
    }

    /// One profile's quota snapshot, as the dashboard GraphQL resolver emits
    /// it. Mirrors Go `biz.ProfileQuotaUsage` (`{ProfileName, Quota, Window,
    /// Usage}`). The Rust caller carries the profile's display name + quota
    /// payload; this helper resolves the window + live usage for that one
    /// profile.
    ///
    /// Dashboard roll-up: for each `(profile_name, quota)` pair, resolve the
    /// live window + usage. Mirrors Go
    /// `(*QuotaService).ProfileQuotaUsages`:
    /// - skips profiles whose `quota` is `None` (Go: `if p.Quota == nil
    ///   continue`);
    /// - otherwise calls [`Self::get_quota_in_offset`] for each profile;
    /// - returns the snapshots in input order (Go appends in profile-iteration
    ///   order).
    ///
    /// `profiles` is a slice of `(profile_name, Option<APIKeyQuota>)` tuples so
    /// the caller can stream them from whatever profile store it has (the Go
    /// path reads `apiKey.Profiles.Profiles`). The caller is responsible for
    /// having already applied authorization on the parent API key (Go doc:
    /// "The caller is responsible for loading the key").
    pub async fn profile_quota_usages(
        &self,
        repo: &dyn QuotaUsageRepo,
        api_key_id: &str,
        profiles: &[(String, Option<APIKeyQuota>)],
        now: DateTime<Utc>,
        offset: FixedOffset,
    ) -> QuotaResult<Vec<ProfileQuotaUsage>> {
        let mut out = Vec::with_capacity(profiles.len());
        for (profile_name, quota) in profiles {
            let Some(quota) = quota else {
                continue;
            };
            let snapshot = self
                .get_quota_in_offset(repo, api_key_id, Some(quota), now, offset)
                .await?;
            let Some(snapshot) = snapshot else {
                continue;
            };
            out.push(ProfileQuotaUsage {
                profile_name: profile_name.clone(),
                quota: quota.clone(),
                window: snapshot.window,
                usage: snapshot.usage,
            });
        }
        Ok(out)
    }
}

impl Default for QuotaService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaProfile {
    pub id: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub policies: Vec<QuotaPolicy>,
}

impl QuotaProfile {
    pub fn new(id: impl Into<String>, policies: Vec<QuotaPolicy>) -> Self {
        Self {
            id: id.into(),
            enabled: true,
            policies,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaPolicy {
    pub id: String,
    pub period: QuotaPeriod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_requests: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost: Option<Decimal>,
}

impl QuotaPolicy {
    pub fn new(id: impl Into<String>, period: QuotaPeriod) -> Self {
        Self {
            id: id.into(),
            period,
            max_requests: None,
            max_tokens: None,
            max_cost: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuotaPeriod {
    AllTime,
    PastDuration {
        unit: QuotaDurationUnit,
        amount: u32,
    },
    CalendarDuration {
        unit: QuotaCalendarUnit,
        amount: u32,
    },
}

impl QuotaPeriod {
    pub fn all_time() -> Self {
        Self::AllTime
    }

    pub fn past_duration(unit: QuotaDurationUnit, amount: u32) -> Self {
        Self::PastDuration { unit, amount }
    }

    pub fn calendar_duration(unit: QuotaCalendarUnit, amount: u32) -> Self {
        Self::CalendarDuration { unit, amount }
    }

    /// Map the Go-shaped [`CoreAPIKeyQuotaPeriod`] (string-tagged union from the
    /// persisted API-key JSON) into the typed Rust enum. Mirrors Go's
    /// `quotaWindow` switch on `period.Type`.
    pub fn from_core(period: &CoreAPIKeyQuotaPeriod) -> QuotaResult<Self> {
        match period.r#type.as_str() {
            api_key_quota_period_type::ALL_TIME => Ok(Self::AllTime),
            api_key_quota_period_type::PAST_DURATION => {
                let pd = period
                    .past_duration
                    .as_ref()
                    .ok_or_else(|| QuotaError::InvalidPeriod("pastDuration is required".into()))?;
                if pd.value <= 0 {
                    return Err(QuotaError::InvalidPeriodAmount);
                }
                let amount =
                    u32::try_from(pd.value).map_err(|_| QuotaError::InvalidPeriodAmount)?;
                let unit = match pd.unit.as_str() {
                    api_key_quota_past_duration_unit::MINUTE => QuotaDurationUnit::Minute,
                    api_key_quota_past_duration_unit::HOUR => QuotaDurationUnit::Hour,
                    api_key_quota_past_duration_unit::DAY => QuotaDurationUnit::Day,
                    other => {
                        return Err(QuotaError::InvalidPeriod(format!(
                            "unknown pastDuration.unit: {other}"
                        )));
                    }
                };
                Ok(Self::PastDuration { unit, amount })
            }
            api_key_quota_period_type::CALENDAR_DURATION => {
                // Go's calendar duration carries only a unit; amount is implicitly 1.
                // The Rust enum keeps the explicit `amount` so multi-bucket windows
                // (e.g. "this + last month") remain expressible.
                let cd = period.calendar_duration.as_ref().ok_or_else(|| {
                    QuotaError::InvalidPeriod("calendarDuration is required".into())
                })?;
                let unit = match cd.unit.as_str() {
                    api_key_quota_calendar_duration_unit::DAY => QuotaCalendarUnit::Day,
                    api_key_quota_calendar_duration_unit::MONTH => QuotaCalendarUnit::Month,
                    other => {
                        return Err(QuotaError::InvalidPeriod(format!(
                            "unknown calendarDuration.unit: {other}"
                        )));
                    }
                };
                Ok(Self::CalendarDuration { unit, amount: 1 })
            }
            other => Err(QuotaError::InvalidPeriod(format!(
                "unknown period.type: {other}"
            ))),
        }
    }

    /// Compute the half-open / inclusive window for this period at `now`,
    /// **assuming the system timezone is UTC**. This is a convenience wrapper
    /// around [`Self::window_in_offset`] that passes a zero `FixedOffset`,
    /// matching Go's `quotaWindow(now, period, time.UTC)` default when
    /// `SystemService.TimeLocation` returns UTC (settings missing / empty /
    /// unparseable timezone string).
    ///
    /// Mirrors Go `quotaWindow`:
    /// - `all_time` → `[unbounded, now]` inclusive end.
    /// - `past_duration` → `[now - duration, now]` inclusive end (timezone-
    ///   independent: rolling duration is computed on the absolute instant).
    /// - `calendar_duration` → `[bucket_start, next_bucket_start)` exclusive
    ///   end, with bucket boundaries aligned to **UTC** midnight. For a
    ///   non-UTC system timezone use [`Self::window_in_offset`].
    pub fn window(&self, now: DateTime<Utc>) -> QuotaResult<QuotaWindow> {
        // `east_opt(0)` is the UTC offset. The only failure mode is |secs| >
        // 86_399, which 0 never triggers, so this `match` never falls through
        // to the error arm (kept to satisfy `deny(clippy::unwrap_used)`).
        let utc = match FixedOffset::east_opt(0) {
            Some(off) => off,
            None => return Err(QuotaError::InvalidCalendarWindow),
        };
        self.window_in_offset(now, utc)
    }

    /// Compute the window with `calendar_duration` bucket boundaries aligned
    /// to **`offset`** midnight (Go: `quotaWindow(now, period, loc)`). This is
    /// the timezone-aware entry point S07 requires.
    ///
    /// `all_time` and `past_duration` are timezone-independent in Go (they
    /// operate on the absolute instant), so `offset` only affects the
    /// `calendar_duration` branches — exactly matching Go's `quotaWindow`,
    /// which only consults `loc` inside the `calendar_duration` case
    /// (`nowLocal := now.In(loc); time.Date(nowLocal.Year(), ..., loc)`).
    ///
    /// Callers that have an IANA timezone string (e.g. from
    /// `SystemGeneralSettings.Timezone`) must resolve it to a `FixedOffset`
    /// first — the workspace does not depend on `chrono-tz`, and Go's
    /// `time.LoadLocation` only ever yields a fixed offset for any given
    /// instant anyway (DST transitions move the offset, but the bucket
    /// boundary computation re-resolves per call, so a single `FixedOffset`
    /// captures the correct local-date arithmetic for the `now` being
    /// evaluated).
    pub fn window_in_offset(
        &self,
        now: DateTime<Utc>,
        offset: FixedOffset,
    ) -> QuotaResult<QuotaWindow> {
        match *self {
            Self::AllTime => Ok(QuotaWindow {
                start: None,
                end: Some(now),
                end_inclusive: true,
            }),
            Self::PastDuration { unit, amount } => {
                let duration = unit.duration(amount)?;
                Ok(QuotaWindow {
                    start: Some(now - duration),
                    end: Some(now),
                    end_inclusive: true,
                })
            }
            Self::CalendarDuration { unit, amount } => unit.window_in_offset(amount, now, offset),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaDurationUnit {
    Minute,
    Hour,
    Day,
}

impl QuotaDurationUnit {
    fn duration(self, amount: u32) -> QuotaResult<Duration> {
        if amount == 0 {
            return Err(QuotaError::InvalidPeriodAmount);
        }

        let amount = i64::from(amount);
        Ok(match self {
            Self::Minute => Duration::minutes(amount),
            Self::Hour => Duration::hours(amount),
            Self::Day => Duration::days(amount),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaCalendarUnit {
    Day,
    Month,
}

impl QuotaCalendarUnit {
    /// Calendar-aligned window. Mirrors Go `quotaWindow`'s
    /// `calendar_duration` branch: whole calendar buckets with an **exclusive**
    /// upper bound (the next bucket's start), so a log timestamped exactly at
    /// `next_bucket_start` is **not** counted (matches Go
    /// `TestQuotaService_CalendarDuration_ExcludesUsageAtWindowEnd`).
    ///
    /// Bucket boundaries are aligned to **local midnight in `offset`** (Go:
    /// `nowLocal := now.In(loc); time.Date(nowLocal.Year(), nowLocal.Month(),
    /// nowLocal.Day(), 0,0,0,0, loc)`). For `offset = UTC` (S05/S06 default)
    /// the boundaries collapse to UTC midnight; for a non-UTC offset the
    /// bucket starts at `local_midnight_in_offset.as_utc()` (S07).
    fn window_in_offset(
        self,
        amount: u32,
        now: DateTime<Utc>,
        offset: FixedOffset,
    ) -> QuotaResult<QuotaWindow> {
        if amount == 0 {
            return Err(QuotaError::InvalidPeriodAmount);
        }

        // Go: nowLocal := now.In(loc). The local wall-clock components drive
        // the bucket boundary; the offset carries them back to UTC.
        let now_local = now.with_timezone(&offset);

        match self {
            Self::Day => {
                let today_start_local = local_midnight(now_local, offset)?;
                let today_start_utc = today_start_local.with_timezone(&Utc);
                Ok(QuotaWindow {
                    // Calendar windows include whole buckets, not a rolling
                    // duration from now. Go uses AddDate(0,0,1) which is a
                    // fixed 24h on the bucket-start instant (no DST jitter at
                    // the day granularity Go supports).
                    start: Some(today_start_utc - Duration::days(i64::from(amount - 1))),
                    end: Some(today_start_utc + Duration::days(1)),
                    end_inclusive: false,
                })
            }
            Self::Month => {
                let amount =
                    i32::try_from(amount).map_err(|_| QuotaError::InvalidCalendarWindow)?;
                // Shift on the LOCAL month index (Go uses AddDate on the
                // loc-typed time, which advances calendar months in `loc`).
                // Doing the arithmetic on the UTC instant would be wrong once
                // the local-month-start crosses a UTC date boundary (e.g.
                // Shanghai 2026-01-01 00:00 == 2025-12-31T16:00Z, whose UTC
                // year/month is 2025-12 — shift_months(+1) on that yields
                // 2026-01-01 instead of 2026-02-01).
                let start_local =
                    shift_months_local(now_local.year(), now_local.month(), -(amount - 1), offset)?;
                let end_local = shift_months_local(now_local.year(), now_local.month(), 1, offset)?;
                Ok(QuotaWindow {
                    start: Some(start_local.with_timezone(&Utc)),
                    end: Some(end_local.with_timezone(&Utc)),
                    end_inclusive: false,
                })
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<DateTime<Utc>>,
    /// When true, a timestamp equal to `end` is considered inside the window
    /// (matches Go `EndInclusive`). `all_time` and `past_duration` set this;
    /// `calendar_duration` does not.
    #[serde(default)]
    pub end_inclusive: bool,
}

impl QuotaWindow {
    pub fn all_time() -> Self {
        // For callers that want the "no boundaries" shape (e.g. legacy test
        // helpers). The Go-faithful `all_time` window produced by
        // [`QuotaPeriod::window`] uses `end = Some(now)` + `end_inclusive = true`.
        Self {
            start: None,
            end: None,
            end_inclusive: true,
        }
    }

    /// Membership test honoring [`Self::end_inclusive`]. Mirrors the Go
    /// `created_at >= start && (end_inclusive ? created_at <= end : created_at < end)`
    /// filter applied by `requestCount` / `usageAgg`.
    pub fn contains(&self, timestamp: DateTime<Utc>) -> bool {
        let after_start = self.start.is_none_or(|start| timestamp >= start);
        let before_end = match self.end {
            None => true,
            Some(end) if self.end_inclusive => timestamp <= end,
            Some(end) => timestamp < end,
        };
        after_start && before_end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaUsage {
    #[serde(default)]
    pub requests: u64,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub cost: Decimal,
}

impl QuotaUsage {
    pub fn new(requests: u64, tokens: u64, cost: Decimal) -> Self {
        Self {
            requests,
            tokens,
            cost,
        }
    }

    fn checked_add(self, other: &Self) -> Self {
        Self {
            requests: self.requests.saturating_add(other.requests),
            tokens: self.tokens.saturating_add(other.tokens),
            cost: self.cost + other.cost,
        }
    }
}

impl Default for QuotaUsage {
    fn default() -> Self {
        Self {
            requests: 0,
            tokens: 0,
            cost: Decimal::ZERO,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaCheck {
    pub policy_id: String,
    pub window: QuotaWindow,
    pub attempted: QuotaUsage,
}

// ---- S10: dashboard / display-path result types ---------------------------
//
// These mirror Go `biz.QuotaResult` and `biz.ProfileQuotaUsage` — the shapes
// `(*QuotaService).GetQuota` / `ProfileQuotaUsages` return so the admin +
// OpenAPI GraphQL resolvers can render the dashboard "API key quota usage"
// widget. They are defined at module scope (not inside the `impl QuotaService`
// block) because Rust forbids `impl`-block items from appearing in sibling
// function signatures inside the same block.

/// Read-only dashboard aggregate for one API-key quota. Mirrors Go `QuotaResult`
/// (`{Window, Usage}`) — distinct from [`QuotaCheck`] (which carries the
/// `policy_id` + `attempted` fields the pre-request gate surfaces).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaUsageSnapshot {
    pub window: QuotaWindow,
    pub usage: QuotaUsage,
}

/// One profile's quota snapshot, as the dashboard GraphQL resolver emits it.
/// Mirrors Go `biz.ProfileQuotaUsage` (`{ProfileName, Quota, Window, Usage}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileQuotaUsage {
    pub profile_name: String,
    pub quota: APIKeyQuota,
    pub window: QuotaWindow,
    pub usage: QuotaUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaLimitKind {
    Requests,
    Tokens,
    Cost,
}

impl std::fmt::Display for QuotaLimitKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Requests => f.write_str("requests"),
            Self::Tokens => f.write_str("tokens"),
            Self::Cost => f.write_str("cost"),
        }
    }
}

// --- helpers ---------------------------------------------------------------

fn default_enabled() -> bool {
    true
}

/// Local-midnight `DateTime` in `offset` for the calendar date of `now_local`
/// (Go: `time.Date(nowLocal.Year(), nowLocal.Month(), nowLocal.Day(),
/// 0,0,0,0, loc)`). Returns a `DateTime<FixedOffset>` so the caller can
/// `.with_timezone(&Utc)` to recover the absolute bucket-start instant.
fn local_midnight(
    now_local: DateTime<FixedOffset>,
    offset: FixedOffset,
) -> QuotaResult<DateTime<FixedOffset>> {
    let date =
        chrono::NaiveDate::from_ymd_opt(now_local.year(), now_local.month(), now_local.day())
            .ok_or(QuotaError::InvalidCalendarWindow)?;
    let ndt = date
        .and_hms_opt(0, 0, 0)
        .ok_or(QuotaError::InvalidCalendarWindow)?;
    offset
        .from_local_datetime(&ndt)
        .single()
        .ok_or(QuotaError::InvalidCalendarWindow)
}

/// First-of-month local-midnight `DateTime` in `offset` (Go:
/// `time.Date(nowLocal.Year(), nowLocal.Month(), 1, 0,0,0,0, loc)`).
fn local_month_start(
    year: i32,
    month: u32,
    offset: FixedOffset,
) -> QuotaResult<DateTime<FixedOffset>> {
    let date =
        chrono::NaiveDate::from_ymd_opt(year, month, 1).ok_or(QuotaError::InvalidCalendarWindow)?;
    let ndt = date
        .and_hms_opt(0, 0, 0)
        .ok_or(QuotaError::InvalidCalendarWindow)?;
    offset
        .from_local_datetime(&ndt)
        .single()
        .ok_or(QuotaError::InvalidCalendarWindow)
}

/// Shift `(year, month)` by `delta_months` calendar months and return the
/// first-of-month local-midnight `DateTime` in `offset`. Mirrors Go's
/// `t.AddDate(0, delta, 0)` operating on a `loc`-typed time: month arithmetic
/// happens in the **local** calendar (so a Shanghai 2026-01-01 boundary
/// advances to 2026-02-01, not to a UTC-December-derived date).
fn shift_months_local(
    year: i32,
    month: u32,
    delta_months: i32,
    offset: FixedOffset,
) -> QuotaResult<DateTime<FixedOffset>> {
    let month_index = year
        .checked_mul(12)
        .and_then(|m| m.checked_add((month as i32) - 1))
        .and_then(|m| m.checked_add(delta_months))
        .ok_or(QuotaError::InvalidCalendarWindow)?;
    let new_year = month_index.div_euclid(12);
    let new_month = u32::try_from(month_index.rem_euclid(12) + 1)
        .map_err(|_| QuotaError::InvalidCalendarWindow)?;
    local_month_start(new_year, new_month, offset)
}

/// Render a [`Decimal`] for the `Exceeded.attempted_value` / `limit_value`
/// fields without stripping trailing scale digits. We preserve the value's
/// own scale (e.g. `Decimal::new(100, 2)` renders as `"1.00"` rather than the
/// normalized `"1"`) so callers can assert on the exact digit sequence they
/// configured. Stripping trailing zeros would lose information the upstream
/// quota author chose to express.
fn decimal_to_string(value: Decimal) -> String {
    value.to_string()
}

// =========================================================================
// Channel + provider quota enforcement (S11/S12/S13).
//
// These are the OTHER two quota kinds beside the API-key profile quota
// (`check_api_key_quota` above). They mirror the Go split between
// `biz.QuotaChannelStatus.EffectiveStatus` (per-limit "worst wins" ranking)
// and `orchestrator.ProviderQuotaSelector.Select` (filter exhausted channels)
// plus `biz.QuotaEnforcementSettings` (disabled / exhausted_only / de_prioritize).
//
// All functions here are PURE: they take already-loaded status snapshots and
// return decisions or `ConduitError`s — no IO. The orchestrator's candidate
// pipeline (conduit-orchestrator) consumes these decisions to filter / reweight
// candidates at routing time.
// =========================================================================

/// HTTP status Go assigns to "all channels quota exhausted for model X"
/// (`orchestrator.QuotaExhaustedError` → `streamErrorStatus` → 503). This is
/// deliberately distinct from the API-key profile quota's 429
/// (`ConduitError::quota_exhausted` default) so the React frontend can tell
/// "retry later against another channel" (503) apart from "this key is spent"
/// (429). See `conduit/internal/server/api/chat.go` `streamErrorStatus` +
/// `wrapQuotaExhaustedAsResponseError`.
pub const CHANNEL_QUOTA_EXHAUSTED_HTTP_STATUS: u16 = 503;

/// `quota_exhausted` error code, shared by both the API-key and channel quota
/// paths (Go uses the same `errCodeQuotaExhausted` constant for both). The
/// distinguishing signal is [`CHANNEL_QUOTA_EXHAUSTED_HTTP_STATUS`] vs the
/// api-key path's 429.
pub const CHANNEL_QUOTA_EXHAUSTED_CODE: &str = "quota_exhausted";

/// Provider-quota status bucket. Mirrors Go
/// `providerquotastatus.Status` (`available` / `warning` / `exhausted` /
/// `unknown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderQuotaStatusKind {
    Available,
    Warning,
    Exhausted,
    Unknown,
}

impl ProviderQuotaStatusKind {
    /// Parse the wire string Go persists in `ProviderQuotaStatus.Status` /
    /// `QuotaLimitStatus.Status`. Unknown values map to
    /// [`ProviderQuotaStatusKind::Unknown`] (Go's `quotaStatusRank` default
    /// branch also collapses unknowns to rank -1).
    pub fn from_str_ci(value: &str) -> Self {
        match value {
            "available" => Self::Available,
            "warning" => Self::Warning,
            "exhausted" => Self::Exhausted,
            _ => Self::Unknown,
        }
    }

    /// Mirrors Go `quotaStatusRank`: available=0, warning=1, exhausted=2,
    /// unknown=-1. Higher rank = worse. Used by [`QuotaChannelStatus`]'s
    /// worst-limit aggregation.
    pub const fn rank(self) -> i8 {
        match self {
            Self::Available => 0,
            Self::Warning => 1,
            Self::Exhausted => 2,
            Self::Unknown => -1,
        }
    }

    /// `true` for `available` / `warning` — mirrors Go
    /// `provider_quota.IsReadyStatus`.
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Available | Self::Warning)
    }
}

/// Limit dimension a provider exposes. Mirrors Go
/// `provider_quota.QuotaLimitType` (`token` / `image` /
/// `subscription_cycle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaLimitType {
    Token,
    Image,
    SubscriptionCycle,
}

impl QuotaLimitType {
    /// Mirrors Go `provider_quota.RequestModality(isImageRequest)`.
    pub fn from_request_modality(is_image_request: bool) -> Self {
        if is_image_request {
            Self::Image
        } else {
            Self::Token
        }
    }
}

/// One provider-reported per-limit snapshot. Mirrors Go
/// `provider_quota.QuotaLimitStatus` (type / status / usage_ratio / ready /
/// optional next_reset_at). The Rust port drops `next_reset_at` because the
/// pure enforcement decision never consults it (only the scheduler's
/// `nextCheckIntervalForStatus` does, which lives in
/// `provider_quota_service.rs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaLimitStatus {
    #[serde(rename = "type")]
    pub limit_type: QuotaLimitType,
    pub status: ProviderQuotaStatusKind,
    #[serde(default)]
    pub usage_ratio: f64,
    #[serde(default)]
    pub ready: bool,
}

impl QuotaLimitStatus {
    pub fn new(
        limit_type: QuotaLimitType,
        status: ProviderQuotaStatusKind,
        usage_ratio: f64,
    ) -> Self {
        Self {
            limit_type,
            status,
            usage_ratio,
            ready: status.is_ready(),
        }
    }
}

/// Channel-level roll-up of the provider-quota snapshot. Mirrors Go
/// `biz.QuotaChannelStatus` (channel-wide Status + Ready + per-limit slice).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaChannelStatus {
    pub status: ProviderQuotaStatusKind,
    #[serde(default)]
    pub ready: bool,
    #[serde(default)]
    pub limits: Vec<QuotaLimitStatus>,
}

impl QuotaChannelStatus {
    pub fn new(status: ProviderQuotaStatusKind, ready: bool) -> Self {
        Self {
            status,
            ready,
            limits: Vec::new(),
        }
    }

    /// Effective quota status for `limit_type`. Faithful port of Go
    /// `(*QuotaChannelStatus).EffectiveStatus`:
    ///
    /// 1. If the channel-level status is `Exhausted`, short-circuit to
    ///    `(Exhausted, false)` regardless of per-limit data — a channel marked
    ///    exhausted at the top level is fully unavailable (this means a future
    ///    provider setting channel-level "exhausted" for one limit type also
    ///    blocks token-limit queries).
    /// 2. With no per-limit data, return the channel-level `(status, ready)`.
    /// 3. Among limits matching `limit_type`, pick the worst status (highest
    ///    [`ProviderQuotaStatusKind::rank`]); on a rank tie, AND the `ready`
    ///    flags (mirror Go `worstReady = worstReady && l.Ready`).
    /// 4. No matching limit type → `(Unknown, true)`: missing data must NOT
    ///    block routing (distinct from a per-limit "unknown" where ready=false).
    pub fn effective_status(&self, limit_type: QuotaLimitType) -> (ProviderQuotaStatusKind, bool) {
        // (1) Channel-level exhausted short-circuit.
        if self.status == ProviderQuotaStatusKind::Exhausted {
            return (ProviderQuotaStatusKind::Exhausted, false);
        }

        // (2) No per-limit data: fall back to channel-level snapshot.
        if self.limits.is_empty() {
            return (self.status, self.ready);
        }

        // (3) Worst-status-wins among matching limits.
        let mut worst_status: Option<ProviderQuotaStatusKind> = None;
        let mut worst_ready = true;
        let mut found = false;

        for limit in &self.limits {
            if limit.limit_type != limit_type {
                continue;
            }

            let ls = limit.status;
            match worst_status {
                None => {
                    worst_status = Some(ls);
                    worst_ready = limit.ready;
                    found = true;
                }
                Some(current) => {
                    if ls.rank() > current.rank() {
                        worst_status = Some(ls);
                        worst_ready = limit.ready;
                    } else if ls.rank() == current.rank() {
                        worst_ready = worst_ready && limit.ready;
                    }
                }
            }
        }

        // (4) No matching limit type → Unknown + ready=true.
        if !found {
            return (ProviderQuotaStatusKind::Unknown, true);
        }

        (
            worst_status.unwrap_or(ProviderQuotaStatusKind::Unknown),
            worst_ready,
        )
    }
}

// ---- S11: channel quota enforcement ----------------------------------------

impl QuotaService {
    /// Enforce the **channel-level** provider-quota gate for one channel.
    /// Mirrors the Go `orchestrator.ProviderQuotaSelector.Select` filter rule:
    /// a channel whose `effective_status` for the request's limit type is
    /// `Exhausted` is rejected; `Available` / `Warning` / `Unknown` pass.
    ///
    /// **Error-code separation (S11):** unlike the API-key profile quota
    /// (which surfaces as HTTP 429 via [`ConduitError::quota_exhausted`]), a
    /// channel-level quota rejection maps to HTTP **503 Service Unavailable**
    /// — matching Go's `orchestrator.QuotaExhaustedError` →
    /// `streamErrorStatus`/`wrapQuotaExhaustedAsResponseError`. The error
    /// `code` stays `quota_exhausted` (Go reuses `errCodeQuotaExhausted` for
    /// both), but the HTTP status differs so the React frontend can
    /// distinguish "key spent, stop retrying" (429) from "this channel is
    /// spent, try another" (503).
    ///
    /// Returns `Ok(())` when the channel may serve the request.
    pub fn enforce_channel_quota(
        &self,
        channel_status: &QuotaChannelStatus,
        limit_type: QuotaLimitType,
    ) -> Result<(), ConduitError> {
        let (effective, _ready) = channel_status.effective_status(limit_type);

        if effective == ProviderQuotaStatusKind::Exhausted {
            let message = format!(
                "channel quota exhausted (effective status: {effective:?}, limit type: {limit_type:?})"
            );
            return Err(ConduitError::quota_exhausted(message)
                .with_http_status(CHANNEL_QUOTA_EXHAUSTED_HTTP_STATUS)
                .with_code(CHANNEL_QUOTA_EXHAUSTED_CODE));
        }

        Ok(())
    }
}

// ---- S12: provider quota exhausted_only vs disabled ------------------------

/// Pure enforcement decision returned by [`QuotaService::enforce_provider_quota`].
/// `Allow` = the candidate may serve; `Block` = filter it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderQuotaDecision {
    Allow,
    Block,
}

impl QuotaService {
    /// Decide whether a provider-quota snapshot should block a candidate,
    /// given the system-wide enforcement settings. Pure port of the Go
    /// `orchestrator.ProviderQuotaSelector.Select` early-return + filter
    /// (`conduit/internal/server/orchestrator/candidates_quota.go`):
    ///
    /// - **`disabled` (`enabled = false`)** → always [`ProviderQuotaDecision::Allow`]
    ///   (Go: `if !settings.Enabled { return candidates, nil }`). Quota
    ///   enforcement is fully off; no candidate is ever blocked by provider
    ///   quota. (The `de_prioritize` mode also takes this early return in the
    ///   selector — deprioritization is the load-balancer's job, not the
    ///   filter's.)
    /// - **`exhausted_only` (`enabled = true`)** → block only when `status`
    ///   is [`ProviderQuotaStatusKind::Exhausted`]. `Available` / `Warning` /
    ///   `Unknown` all pass (Go `EffectiveStatus` switch).
    ///
    /// Note: the `de_prioritize` mode is intentionally NOT modeled as a
    /// "Block" here — it never filters candidates, it only reweights them at
    /// load-balance time (Go `QuotaAwareStrategy`). The caller should consult
    /// [`crate::ProviderQuotaEnforcementMode`] directly when it needs
    /// deprioritization scoring.
    pub fn enforce_provider_quota(
        &self,
        enabled: bool,
        status: ProviderQuotaStatusKind,
    ) -> ProviderQuotaDecision {
        // disabled setting → never block.
        if !enabled {
            return ProviderQuotaDecision::Allow;
        }

        // exhausted_only → block only on Exhausted. (de_prioritize mode never
        // reaches the filter — Go's selector early-returns for it — so from
        // the pure decision's standpoint it's also Allow at this layer.)
        match status {
            ProviderQuotaStatusKind::Exhausted => ProviderQuotaDecision::Block,
            ProviderQuotaStatusKind::Available
            | ProviderQuotaStatusKind::Warning
            | ProviderQuotaStatusKind::Unknown => ProviderQuotaDecision::Allow,
        }
    }
}

// ---- S13: concurrency-safe decrement plan ----------------------------------

/// Which quota bucket a decrement should hit. Mirrors the three Go call sites
/// that consume quota: `QuotaService.CheckAPIKeyQuota` (api-key profile),
/// `ProviderQuotaSelector` (channel/provider), and the post-request usage
/// append (`UsageLog` create).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaDecrementKind {
    /// API-key profile quota bucket — keyed by `(api_key_id, profile_id)`.
    ApiKeyProfile,
    /// Channel/provider quota bucket — keyed by `channel_id`. This is the
    /// remote provider's quota (e.g. Claude Code / Codex weekly window); the
    /// decrement is a *projection* for the next pre-request gate, not a
    /// write to the provider.
    Channel,
}

/// Atomic-decrement strategy recommendation. **This is the plan, not the
/// IO.** The orchestrator/DB layer consumes the plan and performs the actual
/// decrement inside the strategy it picks.
///
/// Mirrors the concurrency contract Go relies on:
/// - API-key profile quota: Go recomputes `requestCount` / `usageAgg` via SQL
///   `COUNT`/`SUM` on `UsageLog` rows, so the "decrement" is really
///   "append the usage row inside the request transaction" — race-safe
///   because the pre-request gate re-queries the aggregate. Rust plan:
///   [`DecrementStrategy::TransactionalAppend`].
/// - Channel/provider quota: Go caches `QuotaChannelStatus` in a
///   `sync.Map` (`ProviderQuotaService.quotaCache`); the cache is refreshed
///   by the scheduled quota checker, not decremented per-request. The
///   pre-request gate reads the cached snapshot. Rust plan:
///   [`DecrementStrategy::AtomicCacheSnapshot`] — the caller should read the
///   cached snapshot atomically (no per-request decrement; the scheduler
///   overwrites it periodically).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecrementPlan {
    pub kind: QuotaDecrementKind,
    /// Positive amount to subtract from the bucket (requests count, or
    /// token/cost units). The plan never decrements more than the bucket
    /// holds — callers MUST clamp at zero (Go's `usageAgg` semantics are
    /// additive, never negative).
    pub amount: u64,
    pub strategy: DecrementStrategy,
}

/// How the decrement should be applied. See [`DecrementPlan`] for the
/// rationale behind each variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecrementStrategy {
    /// Append the usage row inside the request's DB transaction; the
    /// pre-request gate re-aggregates rows. Race-safe because SQL
    /// `COUNT`/`SUM` sees a consistent snapshot. Used for API-key profile
    /// quota.
    TransactionalAppend,
    /// Read the cached snapshot atomically (`ArcSwap` / `RwLock` read); do
    /// NOT decrement per-request. The scheduler overwrites the snapshot
    /// periodically (Go's `runQuotaCheck` → `updateQuotaCache`). Used for
    /// channel/provider quota.
    AtomicCacheSnapshot,
}

impl QuotaService {
    /// Plan the concurrency-safe decrement for `amount` units against the
    /// `kind` quota bucket. **Pure: returns the plan, performs no IO.**
    ///
    /// The orchestrator/DB layer consumes the returned [`DecrementPlan`] and
    /// executes the strategy. This keeps the quota decision logic testable
    /// without a DB or cache harness, while still pinning the concurrency
    /// contract (S13): API-key profile quota is transactional (SQL aggregate
    /// re-query); channel/provider quota is a read-only cached snapshot.
    pub fn plan_quota_decrement(&self, kind: QuotaDecrementKind, amount: u64) -> DecrementPlan {
        let strategy = match kind {
            QuotaDecrementKind::ApiKeyProfile => DecrementStrategy::TransactionalAppend,
            QuotaDecrementKind::Channel => DecrementStrategy::AtomicCacheSnapshot,
        };

        DecrementPlan {
            kind,
            amount,
            strategy,
        }
    }
}

// =========================================================================
// Repo abstraction (async_trait) for live usage queries.
// =========================================================================

/// Aggregated usage for the `(api_key_id, window)` bucket, returned by
/// [`QuotaUsageRepo::usage_aggregate`]. Mirrors Go's `(usageAggResult,
/// requestCount)` pair rolled into one struct so a single repo round-trip can
/// satisfy all three dimensions when needed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuotaUsageAggregate {
    pub requests: u64,
    pub tokens: u64,
    pub cost: Decimal,
}

/// Repo surface the [`QuotaService`] needs to evaluate an API-key quota. This is
/// a narrower view of the DB-layer `UsageRepo`: only the two queries Go's
/// `QuotaService` issues (`requestCount` + `usageAgg`), scoped to a single
/// `api_key_id` and a [`QuotaWindow`]. The implementation is responsible for
/// honoring [`QuotaWindow::end_inclusive`].
#[async_trait]
pub trait QuotaUsageRepo: Send + Sync {
    /// Number of usage rows for `api_key_id` inside `window`. Mirrors Go
    /// `(*QuotaService).requestCount`.
    async fn request_count(
        &self,
        api_key_id: &str,
        window: &QuotaWindow,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>>;

    /// Token + cost totals (plus request count) for `api_key_id` inside
    /// `window`. Mirrors Go `(*QuotaService).usageAgg` with both flags on.
    async fn usage_aggregate(
        &self,
        api_key_id: &str,
        window: &QuotaWindow,
    ) -> Result<QuotaUsageAggregate, Box<dyn std::error::Error + Send + Sync>>;
}

/// In-memory [`QuotaUsageRepo`] for unit tests. Each row carries the
/// `api_key_id`, `created_at`, token count and cost it contributed.
#[derive(Debug, Default)]
pub struct InMemoryQuotaUsageRepo {
    rows: Mutex<Vec<QuotaUsageRow>>,
}

#[derive(Debug, Clone)]
struct QuotaUsageRow {
    api_key_id: String,
    created_at: DateTime<Utc>,
    tokens: u64,
    cost: Decimal,
}

impl InMemoryQuotaUsageRepo {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one usage event for `api_key_id` at `created_at`.
    pub fn record(
        &self,
        api_key_id: impl Into<String>,
        created_at: DateTime<Utc>,
        tokens: u64,
        cost: Decimal,
    ) -> &Self {
        // NOTE(Nietzsche): workspace 禁止 `.expect()`；poison 是逻辑不可能状态，
        // 这里静默忽略 poison（取 inner guard）以保持原有 panic-free 语义。
        let mut guard = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(QuotaUsageRow {
            api_key_id: api_key_id.into(),
            created_at,
            tokens,
            cost,
        });
        self
    }
}

#[async_trait]
impl QuotaUsageRepo for InMemoryQuotaUsageRepo {
    async fn request_count(
        &self,
        api_key_id: &str,
        window: &QuotaWindow,
    ) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
        let guard = self.rows.lock().map_err(|_| {
            Box::<dyn std::error::Error + Send + Sync>::from("quota usage repo poisoned")
        })?;
        let count = guard
            .iter()
            .filter(|row| row.api_key_id == api_key_id && window.contains(row.created_at))
            .count() as u64;
        Ok(count)
    }

    async fn usage_aggregate(
        &self,
        api_key_id: &str,
        window: &QuotaWindow,
    ) -> Result<QuotaUsageAggregate, Box<dyn std::error::Error + Send + Sync>> {
        let guard = self.rows.lock().map_err(|_| {
            Box::<dyn std::error::Error + Send + Sync>::from("quota usage repo poisoned")
        })?;
        let mut agg = QuotaUsageAggregate::default();
        for row in guard
            .iter()
            .filter(|row| row.api_key_id == api_key_id && window.contains(row.created_at))
        {
            agg.requests += 1;
            agg.tokens = agg.tokens.saturating_add(row.tokens);
            agg.cost += row.cost;
        }
        Ok(agg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use conduit_core::objects::apikey::{APIKeyQuotaCalendarDuration, APIKeyQuotaPastDuration};
    use rust_decimal::Decimal;

    fn dt(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
        match Utc
            .with_ymd_and_hms(year, month, day, hour, minute, second)
            .single()
        {
            Some(value) => value,
            None => panic!("test timestamp must be valid"),
        }
    }

    fn service() -> QuotaService {
        QuotaService::new()
    }

    // ---- window computation (period/window) --------------------------------

    #[test]
    fn rolling_minute_window_uses_start_inclusive_end_inclusive() -> QuotaResult<()> {
        let now = dt(2026, 6, 24, 12, 30, 15);
        let window = service().period_window(
            &QuotaPeriod::past_duration(QuotaDurationUnit::Minute, 1),
            now,
        )?;

        assert_eq!(window.start, Some(dt(2026, 6, 24, 12, 29, 15)));
        assert_eq!(window.end, Some(now));
        assert!(window.end_inclusive);
        // Start is inclusive.
        assert!(window.contains(dt(2026, 6, 24, 12, 29, 15)));
        // Just before start is excluded.
        assert!(!window.contains(dt(2026, 6, 24, 12, 29, 14)));
        // End is inclusive (matches Go `EndInclusive=true`).
        assert!(window.contains(now));
        Ok(())
    }

    #[test]
    fn rolling_hour_window_crosses_day_boundary() -> QuotaResult<()> {
        let now = dt(2026, 6, 24, 0, 15, 0);
        let window = service()
            .period_window(&QuotaPeriod::past_duration(QuotaDurationUnit::Hour, 2), now)?;

        assert_eq!(window.start, Some(dt(2026, 6, 23, 22, 15, 0)));
        assert_eq!(window.end, Some(now));
        assert!(window.end_inclusive);
        Ok(())
    }

    #[test]
    fn rolling_day_window_crosses_month_boundary() -> QuotaResult<()> {
        let now = dt(2026, 3, 1, 1, 0, 0);
        let window =
            service().period_window(&QuotaPeriod::past_duration(QuotaDurationUnit::Day, 1), now)?;

        assert_eq!(window.start, Some(dt(2026, 2, 28, 1, 0, 0)));
        assert_eq!(window.end, Some(now));
        Ok(())
    }

    #[test]
    fn calendar_day_window_uses_whole_utc_day_with_exclusive_end() -> QuotaResult<()> {
        let now = dt(2026, 6, 24, 12, 30, 15);
        let window = service().period_window(
            &QuotaPeriod::calendar_duration(QuotaCalendarUnit::Day, 1),
            now,
        )?;

        assert_eq!(window.start, Some(dt(2026, 6, 24, 0, 0, 0)));
        assert_eq!(window.end, Some(dt(2026, 6, 25, 0, 0, 0)));
        assert!(!window.end_inclusive);
        assert!(window.contains(dt(2026, 6, 24, 23, 59, 59)));
        // Exactly at the next bucket start -> excluded (Go parity:
        // TestQuotaService_CalendarDuration_ExcludesUsageAtWindowEnd).
        assert!(!window.contains(dt(2026, 6, 25, 0, 0, 0)));
        Ok(())
    }

    #[test]
    fn calendar_month_window_crosses_year_boundary() -> QuotaResult<()> {
        let now = dt(2026, 1, 15, 8, 0, 0);
        let window = service().period_window(
            &QuotaPeriod::calendar_duration(QuotaCalendarUnit::Month, 2),
            now,
        )?;

        assert_eq!(window.start, Some(dt(2025, 12, 1, 0, 0, 0)));
        assert_eq!(window.end, Some(dt(2026, 2, 1, 0, 0, 0)));
        assert!(window.contains(dt(2025, 12, 1, 0, 0, 0)));
        assert!(!window.contains(dt(2026, 2, 1, 0, 0, 0)));
        Ok(())
    }

    // ---- S07: calendar duration day/month with system timezone ---------------
    //
    // Direct ports of Go `TestQuotaWindow_CalendarDay_Timezone` and
    // `TestQuotaWindow_CalendarMonth_Timezone` (quota_test.go:330-368). Both
    // pin the rule that the calendar bucket boundaries are aligned to **local
    // midnight in the system timezone**, not UTC midnight.

    /// Mirror of Go `time.LoadLocation("Asia/Shanghai")` for the timezone
    /// golden tests. Shanghai is UTC+8 year-round (no DST), so a fixed
    /// `FixedOffset` captures it exactly.
    fn shanghai_offset() -> FixedOffset {
        // east_opt(8 * 3600) = UTC+8; the secs value is always in range.
        match FixedOffset::east_opt(8 * 3600) {
            Some(off) => off,
            None => panic!("UTC+8 must parse"),
        }
    }

    #[test]
    fn calendar_day_window_aligns_to_local_midnight_in_system_timezone() -> QuotaResult<()> {
        // Go: TestQuotaWindow_CalendarDay_Timezone
        //   now = 2026-01-20T01:02:03Z, loc = Asia/Shanghai (UTC+8)
        //   expected start = 2026-01-19T16:00:00Z (Shanghai 2026-01-20 00:00)
        //   expected end   = 2026-01-20T16:00:00Z (Shanghai 2026-01-21 00:00)
        let now = dt(2026, 1, 20, 1, 2, 3);
        let period = QuotaPeriod::calendar_duration(QuotaCalendarUnit::Day, 1);
        let window = period.window_in_offset(now, shanghai_offset())?;

        assert_eq!(window.start, Some(dt(2026, 1, 19, 16, 0, 0)));
        assert_eq!(window.end, Some(dt(2026, 1, 20, 16, 0, 0)));
        assert!(!window.end_inclusive);
        // A log at 2026-01-19T16:00:00Z (Shanghai midnight) is included.
        assert!(window.contains(dt(2026, 1, 19, 16, 0, 0)));
        // A log at 2026-01-20T15:59:59Z (Shanghai 23:59:59) is included.
        assert!(window.contains(dt(2026, 1, 20, 15, 59, 59)));
        // A log at 2026-01-20T16:00:00Z (next Shanghai midnight) is excluded
        // (exclusive end).
        assert!(!window.contains(dt(2026, 1, 20, 16, 0, 0)));
        Ok(())
    }

    #[test]
    fn calendar_month_window_aligns_to_local_first_of_month_in_system_timezone() -> QuotaResult<()>
    {
        // Go: TestQuotaWindow_CalendarMonth_Timezone
        //   now = 2026-01-20T01:02:03Z, loc = Asia/Shanghai (UTC+8)
        //   expected start = 2025-12-31T16:00:00Z (Shanghai 2026-01-01 00:00)
        //   expected end   = 2026-01-31T16:00:00Z (Shanghai 2026-02-01 00:00)
        let now = dt(2026, 1, 20, 1, 2, 3);
        let period = QuotaPeriod::calendar_duration(QuotaCalendarUnit::Month, 1);
        let window = period.window_in_offset(now, shanghai_offset())?;

        assert_eq!(window.start, Some(dt(2025, 12, 31, 16, 0, 0)));
        assert_eq!(window.end, Some(dt(2026, 1, 31, 16, 0, 0)));
        assert!(!window.end_inclusive);
        assert!(window.contains(dt(2025, 12, 31, 16, 0, 0)));
        // A log at 2026-01-31T15:59:59Z (Shanghai 2026-01-31 23:59:59) is
        // inside the January bucket.
        assert!(window.contains(dt(2026, 1, 31, 15, 59, 59)));
        // A log at 2026-01-31T16:00:00Z (Shanghai 2026-02-01 00:00, exclusive
        // end) is excluded.
        assert!(!window.contains(dt(2026, 1, 31, 16, 0, 0)));
        Ok(())
    }

    #[test]
    fn calendar_day_window_negative_offset_aligns_westward() -> QuotaResult<()> {
        // Complementary case: US Pacific (UTC-8, standard time). A log at
        // 2026-01-20T08:00:00Z is 2026-01-20T00:00:00-08:00 = Pacific
        // midnight, so it sits at the bucket start.
        let pacific = match FixedOffset::west_opt(8 * 3600) {
            Some(off) => off,
            None => return Err(QuotaError::InvalidCalendarWindow),
        };
        let now = dt(2026, 1, 20, 15, 0, 0);
        let period = QuotaPeriod::calendar_duration(QuotaCalendarUnit::Day, 1);
        let window = period.window_in_offset(now, pacific)?;

        assert_eq!(window.start, Some(dt(2026, 1, 20, 8, 0, 0)));
        assert_eq!(window.end, Some(dt(2026, 1, 21, 8, 0, 0)));
        assert!(window.contains(dt(2026, 1, 20, 8, 0, 0)));
        assert!(!window.contains(dt(2026, 1, 21, 8, 0, 0)));
        Ok(())
    }

    #[test]
    fn calendar_window_utc_offset_collapses_to_utc_midnight() -> QuotaResult<()> {
        // Pin the contract that `window()` (UTC default) and
        // `window_in_offset(now, UTC)` agree, so the S05/S06 UTC tests still
        // exercise the same code path as the production default.
        let now = dt(2026, 6, 24, 12, 30, 15);
        let period = QuotaPeriod::calendar_duration(QuotaCalendarUnit::Day, 1);
        let via_utc_default = period.window(now)?;
        let via_zero_offset = period.window_in_offset(
            now,
            match FixedOffset::east_opt(0) {
                Some(off) => off,
                None => return Err(QuotaError::InvalidCalendarWindow),
            },
        )?;
        assert_eq!(via_utc_default, via_zero_offset);
        Ok(())
    }

    // ---- S09: minute quota cross-boundary membership -------------------------
    //
    // Mirrors Go `TestQuotaService_PastDurationMinute_RequestCountExceeded`
    // (quota_test.go:186) and
    // `TestQuotaService_PastDurationMinute_IncludesUsageAtWindowEnd`
    // (quota_test.go:243). These pin the minute-window cross-boundary rule:
    // a usage log timestamped exactly at the window's inclusive end is
    // counted, while one even one second past the end falls into the next
    // minute's window.

    #[tokio::test]
    async fn past_duration_minute_rejects_when_prior_request_inside_window() -> QuotaResult<()> {
        // Go: TestQuotaService_PastDurationMinute_RequestCountExceeded.
        // A single prior usage at `now - 10s` is inside the 1-minute rolling
        // window, so the new request (which would be the 2nd) hits the
        // `reqCount >= 1` gate and is rejected.
        let now = dt(2026, 6, 24, 12, 30, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", dt(2026, 6, 24, 12, 29, 50), 0, Decimal::ZERO);

        let mut quota = core_quota(core_past_duration(
            1,
            api_key_quota_past_duration_unit::MINUTE,
        ));
        quota.requests = Some(1);

        let err = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await;
        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Requests,
                attempted_value,
                ..
            }) if attempted_value == "1"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn past_duration_minute_includes_usage_exactly_at_window_end() -> QuotaResult<()> {
        // Go: TestQuotaService_PastDurationMinute_IncludesUsageAtWindowEnd.
        // A usage log timestamped exactly at `now` (the inclusive end of the
        // `[now-1m, now]` window) is counted.
        let now = dt(2026, 6, 24, 12, 30, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now, 10, Decimal::new(100, 0));

        let mut quota = core_quota(core_past_duration(
            1,
            api_key_quota_past_duration_unit::MINUTE,
        ));
        quota.total_tokens = Some(10);
        // At-limit (>=) so the inclusive-end log must trip the gate.
        let err = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await;
        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Tokens,
                attempted_value,
                ..
            }) if attempted_value == "10"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn past_duration_minute_excludes_usage_one_second_past_window_start() -> QuotaResult<()> {
        // Cross-boundary membership: a log one second BEFORE the window start
        // belongs to the previous minute and must NOT count.
        let now = dt(2026, 6, 24, 12, 30, 0);
        let window_start = dt(2026, 6, 24, 12, 29, 0);
        let just_before = window_start - Duration::seconds(1); // 12:28:59
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", just_before, 999, Decimal::ZERO);
        // Also record a log exactly at the window start (12:29:00) — must count.
        repo.record("k-1", window_start, 1, Decimal::ZERO);

        let mut quota = core_quota(core_past_duration(
            1,
            api_key_quota_past_duration_unit::MINUTE,
        ));
        quota.total_tokens = Some(2);

        let result = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await?;
        // Only the 12:29:00 log (1 token) is inside; 999-token log excluded.
        let check = result.ok_or(QuotaError::InvalidPeriod("expected check".into()))?;
        assert_eq!(check.attempted.tokens, 1);
        Ok(())
    }

    #[tokio::test]
    async fn past_duration_minute_two_logs_straddling_minute_boundary() -> QuotaResult<()> {
        // Cross-boundary aggregate: two logs, one inside the current 1-minute
        // window and one in the previous minute. Only the in-window one
        // contributes to the count.
        let now = dt(2026, 6, 24, 12, 30, 15);
        let repo = InMemoryQuotaUsageRepo::new();
        // Previous minute bucket (excluded).
        repo.record("k-1", dt(2026, 6, 24, 12, 28, 30), 0, Decimal::ZERO);
        // Current minute bucket (included).
        repo.record("k-1", dt(2026, 6, 24, 12, 30, 0), 0, Decimal::ZERO);

        let mut quota = core_quota(core_past_duration(
            1,
            api_key_quota_past_duration_unit::MINUTE,
        ));
        quota.requests = Some(1);

        let err = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await;
        // 1 prior request inside window → `>= 1` → rejected.
        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Requests,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn all_time_window_runs_up_to_now_inclusive() -> QuotaResult<()> {
        let now = dt(2026, 6, 24, 12, 0, 0);
        let window = service().period_window(&QuotaPeriod::all_time(), now)?;

        assert_eq!(window.start, None);
        assert_eq!(window.end, Some(now));
        assert!(window.end_inclusive);
        assert!(window.contains(dt(1970, 1, 1, 0, 0, 0)));
        assert!(window.contains(now));
        // After `now` is outside the window (future log can't have happened yet).
        assert!(!window.contains(dt(2026, 6, 24, 12, 0, 1)));
        Ok(())
    }

    // ---- Direct Go-golden window mirrors (quota_test.go:301 / 318) ----------
    //
    // The semantic window rules are already covered above; these two tests pin
    // the EXACT Go golden timestamps + amounts so a regression in `period_window`
    // is caught against the same inputs the Go suite uses.

    #[test]
    fn quota_window_past_duration_minute_go_golden() -> QuotaResult<()> {
        // Go: TestQuotaWindow_PastDurationMinute (quota_test.go:301).
        //   now  = 2026-01-20T01:02:03Z
        //   value=5, unit=minute
        //   start=now-5m = 2026-01-20T00:57:03Z, end=now, EndInclusive=true.
        let now = dt(2026, 1, 20, 1, 2, 3);
        let window = service().period_window(
            &QuotaPeriod::past_duration(QuotaDurationUnit::Minute, 5),
            now,
        )?;
        assert_eq!(window.start, Some(dt(2026, 1, 20, 0, 57, 3)));
        assert_eq!(window.end, Some(now));
        assert!(window.end_inclusive);
        Ok(())
    }

    #[test]
    fn quota_window_all_time_go_golden() -> QuotaResult<()> {
        // Go: TestQuotaWindow_AllTime (quota_test.go:318).
        //   now  = 2026-01-20T01:02:03Z
        //   start=nil, end=now, EndInclusive=true.
        let now = dt(2026, 1, 20, 1, 2, 3);
        let window = service().period_window(&QuotaPeriod::all_time(), now)?;
        assert_eq!(window.start, None);
        assert_eq!(window.end, Some(now));
        assert!(window.end_inclusive);
        Ok(())
    }

    // ---- check_policy: three-dimension limit checks (attempted > limit) ----

    #[test]
    fn request_limit_allows_exact_limit_and_rejects_over_limit() -> QuotaResult<()> {
        let service = service();
        let mut policy = QuotaPolicy::new("minute-requests", QuotaPeriod::all_time());
        policy.max_requests = Some(10);

        let exact = service.check_policy(
            &policy,
            &QuotaUsage::new(8, 0, Decimal::ZERO),
            &QuotaUsage::new(2, 0, Decimal::ZERO),
            dt(2026, 6, 24, 0, 0, 0),
        )?;

        assert_eq!(exact.attempted.requests, 10);

        let err = service.check_policy(
            &policy,
            &QuotaUsage::new(9, 0, Decimal::ZERO),
            &QuotaUsage::new(2, 0, Decimal::ZERO),
            dt(2026, 6, 24, 0, 0, 0),
        );

        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Requests,
                attempted_value,
                ..
            }) if attempted_value == "11"
        ));
        Ok(())
    }

    #[test]
    fn token_limit_rejects_over_limit() {
        let service = service();
        let mut policy = QuotaPolicy::new("daily-tokens", QuotaPeriod::all_time());
        policy.max_tokens = Some(100);

        let err = service.check_policy(
            &policy,
            &QuotaUsage::new(0, 90, Decimal::ZERO),
            &QuotaUsage::new(0, 11, Decimal::ZERO),
            dt(2026, 6, 24, 0, 0, 0),
        );

        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Tokens,
                attempted_value,
                ..
            }) if attempted_value == "101"
        ));
    }

    #[test]
    fn cost_limit_rejects_over_limit() {
        let service = service();
        let mut policy = QuotaPolicy::new("monthly-cost", QuotaPeriod::all_time());
        policy.max_cost = Some(Decimal::new(100, 2));

        let err = service.check_policy(
            &policy,
            &QuotaUsage::new(0, 0, Decimal::new(90, 2)),
            &QuotaUsage::new(0, 0, Decimal::new(11, 2)),
            dt(2026, 6, 24, 0, 0, 0),
        );

        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Cost,
                attempted_value,
                ..
            }) if attempted_value == "1.01"
        ));
    }

    // ---- check_api_key_quota: live repo gate (Go CheckAPIKeyQuota parity) --

    fn core_quota(period: CoreAPIKeyQuotaPeriod) -> APIKeyQuota {
        APIKeyQuota {
            requests: None,
            total_tokens: None,
            cost: None,
            period,
        }
    }

    fn core_past_duration(value: i64, unit: &str) -> CoreAPIKeyQuotaPeriod {
        CoreAPIKeyQuotaPeriod {
            r#type: api_key_quota_period_type::PAST_DURATION.into(),
            past_duration: Some(APIKeyQuotaPastDuration {
                value,
                unit: unit.into(),
            }),
            calendar_duration: None,
        }
    }

    fn core_calendar(unit: &str) -> CoreAPIKeyQuotaPeriod {
        CoreAPIKeyQuotaPeriod {
            r#type: api_key_quota_period_type::CALENDAR_DURATION.into(),
            past_duration: None,
            calendar_duration: Some(APIKeyQuotaCalendarDuration { unit: unit.into() }),
        }
    }

    fn core_all_time() -> CoreAPIKeyQuotaPeriod {
        CoreAPIKeyQuotaPeriod {
            r#type: api_key_quota_period_type::ALL_TIME.into(),
            past_duration: None,
            calendar_duration: None,
        }
    }

    #[tokio::test]
    async fn check_api_key_quota_none_quota_returns_none() -> QuotaResult<()> {
        let repo = InMemoryQuotaUsageRepo::new();
        let result = service()
            .check_api_key_quota(&repo, "k-1", None, dt(2026, 6, 24, 0, 0, 0))
            .await?;
        assert!(result.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn check_api_key_quota_requests_under_limit_passes() -> QuotaResult<()> {
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        // 3 prior requests inside the rolling minute window.
        repo.record("k-1", dt(2026, 6, 24, 11, 59, 30), 0, Decimal::ZERO)
            .record("k-1", dt(2026, 6, 24, 11, 59, 45), 0, Decimal::ZERO)
            .record("k-1", now, 0, Decimal::ZERO);

        let mut quota = core_quota(core_past_duration(
            1,
            api_key_quota_past_duration_unit::MINUTE,
        ));
        quota.requests = Some(10);

        let result = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await?;
        let check = result.ok_or(QuotaError::InvalidPeriod("expected check".into()))?;
        assert_eq!(check.attempted.requests, 3);
        Ok(())
    }

    #[tokio::test]
    async fn check_api_key_quota_requests_at_limit_rejects_with_ge_semantics() -> QuotaResult<()> {
        // Go uses `reqCount >= *quota.Requests`, so reaching the limit exhausts
        // the quota until the window rolls forward.
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now, 0, Decimal::ZERO)
            .record("k-1", now, 0, Decimal::ZERO);

        let mut quota = core_quota(core_past_duration(
            1,
            api_key_quota_past_duration_unit::MINUTE,
        ));
        quota.requests = Some(2);

        let err = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await;
        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Requests,
                attempted_value,
                ..
            }) if attempted_value == "2"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn check_api_key_quota_negative_requests_limit_never_trips() -> QuotaResult<()> {
        // Go `quota.Requests` is `*int64`; a negative limit makes
        // `reqCount >= limit` always false, so the quota never trips. Mirror.
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now, 5, Decimal::ZERO);

        let mut quota = core_quota(core_past_duration(
            1,
            api_key_quota_past_duration_unit::MINUTE,
        ));
        quota.requests = Some(-5);

        // No tokens/cost dimension ⇒ early-allow path; must not reject.
        let outcome = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await;
        assert!(
            outcome.is_ok(),
            "negative limit must never trip: {outcome:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn check_api_key_quota_tokens_dimension_rejects_at_limit() -> QuotaResult<()> {
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now, 4_000, Decimal::ZERO)
            .record("k-1", now, 1_000, Decimal::ZERO);

        let mut quota = core_quota(core_all_time());
        quota.total_tokens = Some(5_000);

        let err = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await;
        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Tokens,
                attempted_value,
                ..
            }) if attempted_value == "5000"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn check_api_key_quota_cost_dimension_rejects_at_limit() -> QuotaResult<()> {
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now, 0, Decimal::new(75, 2))
            .record("k-1", now, 0, Decimal::new(25, 2));

        let mut quota = core_quota(core_all_time());
        quota.cost = Some(Decimal::new(100, 2)); // 1.00

        let err = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await;
        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Cost,
                attempted_value,
                ..
            }) if attempted_value == "1.00"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn check_api_key_quota_calendar_excludes_usage_at_window_end() -> QuotaResult<()> {
        // Direct port of Go's
        // `TestQuotaService_CalendarDuration_ExcludesUsageAtWindowEnd`: a usage
        // log timestamped exactly at the window's end (next bucket start) must
        // NOT count. The Go test manually constructs the window
        // {Start: 2026-01-20T00:00:00Z, End: 2026-01-21T00:00:00Z} and records
        // a log at `2026-01-21T00:00:00Z`. Here we go through
        // `check_api_key_quota`, which derives the window from `now`: setting
        // `now` inside the 2026-01-20 calendar day yields the same window
        // [2026-01-20T00:00:00Z, 2026-01-21T00:00:00Z), so the log at the
        // exclusive end is excluded.
        let now = dt(2026, 1, 20, 12, 0, 0);
        let window_end = dt(2026, 1, 21, 0, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", window_end, 10, Decimal::new(100, 0));

        let mut quota = core_quota(core_calendar(api_key_quota_calendar_duration_unit::DAY));
        // The window for `calendar day` with `now = 2026-01-20T12:00:00Z` is
        // [2026-01-20T00:00:00Z, 2026-01-21T00:00:00Z); the log sits at the
        // exclusive end and so must be excluded.
        quota.requests = Some(1);

        let result = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await?;
        assert!(result.is_some());
        Ok(())
    }

    // ---- Direct Go-golden dimension×period mirrors (quota_test.go) ----------
    //
    // These pin the three period×dimension combinations Go's DB-backed tests
    // exercise that the prior Rust suite covered only with the `all_time`
    // period: the all_time+requests gate (quota_test.go:21), the
    // past_duration-hour+tokens gate with an out-of-window log that must NOT
    // count (quota_test.go:96), and the calendar-day+cost gate (quota_test.go:427).
    // The ent/DB-backed versions remain pending a Rust ent harness; these mirror
    // the pure logic via InMemoryQuotaUsageRepo (same convention as the existing
    // `past_duration_minute_*` Rust tests that mirror quota_test.go:186/243).

    #[tokio::test]
    async fn check_api_key_quota_all_time_requests_exceeded_go_golden() -> QuotaResult<()> {
        // Go: TestQuotaService_AllTime_RequestCountExceeded (quota_test.go:21).
        //   Two prior completed requests inside the all_time window (window has
        //   no start, end=now inclusive). requests=2 → Go `reqCount >= 2` →
        //   rejected with message "requests quota exceeded: 2/2".
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now - Duration::hours(2), 0, Decimal::ZERO)
            .record("k-1", now - Duration::hours(1), 0, Decimal::ZERO);

        let mut quota = core_quota(core_all_time());
        quota.requests = Some(2);

        let err = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await;
        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Requests,
                attempted_value,
                ..
            }) if attempted_value == "2"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn past_duration_hour_tokens_excludes_out_of_window_log_go_golden() -> QuotaResult<()> {
        // Go: TestQuotaService_PastDuration_TotalTokensExceeded (quota_test.go:96).
        //   1-hour rolling window; an in-window log @ now-29m carries 150 tokens
        //   (≥ limit 100 → rejected), and an older log @ now-3h with 20 tokens
        //   must NOT count. attempted_value = "150" (not "170").
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        // In-window (29 minutes ago): 150 tokens, cost 1.0 (Go SetTotalCost(1.0)).
        repo.record("k-1", now - Duration::minutes(29), 150, Decimal::new(10, 0))
            // Out-window (3 hours ago): 20 tokens — must be excluded.
            .record("k-1", now - Duration::hours(3), 20, Decimal::new(10, 0));

        let mut quota = core_quota(core_past_duration(
            1,
            api_key_quota_past_duration_unit::HOUR,
        ));
        quota.total_tokens = Some(100);

        let err = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await;
        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Tokens,
                attempted_value,
                ..
            }) if attempted_value == "150"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn calendar_day_cost_exceeded_go_golden() -> QuotaResult<()> {
        // Go: TestQuotaService_CalendarDay_CostExceeded (quota_test.go:427).
        //   Calendar-day window; one log @ now with total_cost = 11.0 against a
        //   cost limit of decimal.NewFromFloat(10.0) = 10. Go uses
        //   `usageAgg.TotalCost.GreaterThanOrEqual(*quota.Cost)` → 11 >= 10 →
        //   rejected with "cost quota exceeded: 11/10".
        //
        //   Decimal note: Go round-trips total_cost through a float64 SUM and
        //   rehydrates via `decimal.NewFromFloat(11.0)` which renders as "11"
        //   (scale 0). We mirror that exact scale with `Decimal::new(11, 0)`
        //   and `Decimal::new(10, 0)` (NOT `dec!`, which the workspace lacks).
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        // Go SetTotalCost(11.0) → decimal.NewFromFloat(11.0) → "11".
        repo.record("k-1", now, 2, Decimal::new(11, 0));

        let mut quota = core_quota(core_calendar(api_key_quota_calendar_duration_unit::DAY));
        // Go decimal.NewFromFloat(10.0) → "10".
        quota.cost = Some(Decimal::new(10, 0));

        let err = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await;
        assert!(matches!(
            err,
            Err(QuotaError::Exceeded {
                limit: QuotaLimitKind::Cost,
                attempted_value,
                ..
            }) if attempted_value == "11"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn check_api_key_quota_all_dimensions_pass_returns_check() -> QuotaResult<()> {
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now, 100, Decimal::new(10, 2));

        let mut quota = core_quota(core_all_time());
        quota.requests = Some(10);
        quota.total_tokens = Some(1_000);
        quota.cost = Some(Decimal::new(100, 2));

        let result = service()
            .check_api_key_quota(&repo, "k-1", Some(&quota), now)
            .await?;
        let check = result.ok_or(QuotaError::InvalidPeriod("expected check".into()))?;
        assert_eq!(check.attempted.requests, 1);
        assert_eq!(check.attempted.tokens, 100);
        assert_eq!(check.attempted.cost, Decimal::new(10, 2));
        Ok(())
    }

    // ---- S10: get_quota / profile_quota_usages (dashboard display path) -----
    //
    // Mirror Go (*QuotaService).GetQuota / ProfileQuotaUsages: read-only
    // aggregate that ALWAYS queries both dimensions and NEVER rejects, even
    // when the limit is already exhausted. This is what the dashboard "API key
    // quota usage" widget consumes.

    #[tokio::test]
    async fn get_quota_none_quota_returns_none() -> QuotaResult<()> {
        let repo = InMemoryQuotaUsageRepo::new();
        let result = service()
            .get_quota(&repo, "k-1", None, dt(2026, 6, 24, 0, 0, 0))
            .await?;
        assert!(result.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn get_quota_returns_usage_without_rejecting_at_limit() -> QuotaResult<()> {
        // S10 distinction from the gate path: even when current usage is at or
        // over the limit, GetQuota returns the snapshot (the dashboard renders
        // the over-limit ratio itself). Go GetQuota has no `>=` check.
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now, 4_000, Decimal::new(40, 2)).record(
            "k-1",
            now,
            1_500,
            Decimal::new(60, 2),
        );

        let mut quota = core_quota(core_all_time());
        quota.total_tokens = Some(5_000); // 5500 >= 5000 — would gate-reject.
        quota.cost = Some(Decimal::new(100, 2)); // 1.00 >= 1.00 — would gate-reject.

        let snapshot = service()
            .get_quota(&repo, "k-1", Some(&quota), now)
            .await?
            .ok_or(QuotaError::InvalidPeriod("expected snapshot".into()))?;

        assert_eq!(snapshot.usage.requests, 2);
        assert_eq!(
            snapshot.usage.tokens, 5_500,
            "tokens must aggregate even when over limit"
        );
        assert_eq!(snapshot.usage.cost, Decimal::new(100, 2));
        Ok(())
    }

    #[tokio::test]
    async fn get_quota_queries_both_dimensions_when_no_limits_configured() -> QuotaResult<()> {
        // Go GetQuota always fires requestCount + usageAgg(needTokens=true,
        // needCost=true), so the dashboard can show token totals on a quota
        // that only gates requests. The gate path skips agg when neither tokens
        // nor cost is set; GetQuota does not.
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now, 750, Decimal::new(12, 2));

        // No limits configured at all — pure display quota.
        let quota = core_quota(core_all_time());

        let snapshot = service()
            .get_quota(&repo, "k-1", Some(&quota), now)
            .await?
            .ok_or(QuotaError::InvalidPeriod("expected snapshot".into()))?;

        assert_eq!(snapshot.usage.requests, 1);
        assert_eq!(snapshot.usage.tokens, 750);
        assert_eq!(snapshot.usage.cost, Decimal::new(12, 2));
        Ok(())
    }

    #[tokio::test]
    async fn get_quota_honors_calendar_window_exclusive_end() -> QuotaResult<()> {
        // S10 parity with the gate path: GetQuota must use the same window
        // semantics (calendar_duration → exclusive end). A log at the next
        // bucket start must NOT appear in the dashboard aggregate.
        let now = dt(2026, 1, 20, 12, 0, 0); // → window [2026-01-20T00Z, 2026-01-21T00Z)
        let window_end = dt(2026, 1, 21, 0, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", window_end, 10, Decimal::ZERO); // excluded
        repo.record("k-1", now, 5, Decimal::ZERO); // included

        let quota = core_quota(core_calendar(api_key_quota_calendar_duration_unit::DAY));

        let snapshot = service()
            .get_quota(&repo, "k-1", Some(&quota), now)
            .await?
            .ok_or(QuotaError::InvalidPeriod("expected snapshot".into()))?;

        assert_eq!(
            snapshot.usage.tokens, 5,
            "exclusive-end log must be excluded"
        );
        assert_eq!(snapshot.usage.requests, 1);
        Ok(())
    }

    #[tokio::test]
    async fn profile_quota_usages_skips_profiles_without_quota() -> QuotaResult<()> {
        // Go ProfileQuotaUsages: `if p.Quota == nil { continue }`.
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now, 100, Decimal::ZERO);

        let mut q = core_quota(core_all_time());
        q.total_tokens = Some(1_000);

        let profiles: Vec<(String, Option<APIKeyQuota>)> = vec![
            ("no-quota".into(), None),
            ("with-quota".into(), Some(q.clone())),
        ];

        let utc = match FixedOffset::east_opt(0) {
            Some(off) => off,
            None => return Err(QuotaError::InvalidCalendarWindow),
        };
        let usages = service()
            .profile_quota_usages(&repo, "k-1", &profiles, now, utc)
            .await?;

        assert_eq!(
            usages.len(),
            1,
            "profile without quota must be skipped (not error, not zeroed)"
        );
        assert_eq!(usages[0].profile_name, "with-quota");
        assert_eq!(usages[0].usage.tokens, 100);
        assert_eq!(usages[0].quota, q);
        Ok(())
    }

    #[tokio::test]
    async fn profile_quota_usages_preserves_input_order() -> QuotaResult<()> {
        // Go iterates `apiKey.Profiles.Profiles` in slice order and appends in
        // the same order — the dashboard list order is the profile definition
        // order, not sorted by name. Pin that.
        let now = dt(2026, 6, 24, 12, 0, 0);
        let repo = InMemoryQuotaUsageRepo::new();
        repo.record("k-1", now, 10, Decimal::ZERO);

        let q = core_quota(core_all_time());
        let profiles: Vec<(String, Option<APIKeyQuota>)> = vec![
            ("zeta".into(), Some(q.clone())),
            ("alpha".into(), Some(q.clone())),
            ("middle".into(), Some(q.clone())),
        ];

        let utc = match FixedOffset::east_opt(0) {
            Some(off) => off,
            None => return Err(QuotaError::InvalidCalendarWindow),
        };
        let usages = service()
            .profile_quota_usages(&repo, "k-1", &profiles, now, utc)
            .await?;

        let names: Vec<&str> = usages.iter().map(|u| u.profile_name.as_str()).collect();
        assert_eq!(names, vec!["zeta", "alpha", "middle"]);
        Ok(())
    }

    #[tokio::test]
    async fn get_quota_in_offset_aligns_calendar_to_local_midnight() -> QuotaResult<()> {
        // S10 + S07 cross-check: the display path threads `offset` through the
        // same `window_in_offset` as the gate path. A Shanghai (UTC+8) system
        // timezone must align the calendar-day bucket to local midnight, not
        // UTC midnight.
        let shanghai = match FixedOffset::east_opt(8 * 3600) {
            Some(off) => off,
            None => return Err(QuotaError::InvalidCalendarWindow),
        };
        // now = 2026-01-20T01:02:03Z = Shanghai 2026-01-20T09:02:03+08:00.
        // Shanghai-local calendar day → [2026-01-19T16:00:00Z, 2026-01-20T16:00:00Z).
        let now = dt(2026, 1, 20, 1, 2, 3);
        let repo = InMemoryQuotaUsageRepo::new();
        // A log at UTC midnight (2026-01-20T00:00:00Z) is Shanghai
        // 2026-01-20T08:00:00+08:00 — INSIDE the Shanghai-local day bucket
        // (between 2026-01-19T16:00Z and 2026-01-20T16:00Z), must be included.
        repo.record("k-1", dt(2026, 1, 20, 0, 0, 0), 999, Decimal::ZERO);
        // A log just before the bucket start (2026-01-19T15:59:59Z = Shanghai
        // 2026-01-19T23:59:59, previous local day) must be excluded.
        repo.record("k-1", dt(2026, 1, 19, 15, 59, 59), 5, Decimal::ZERO);
        // A log at Shanghai-local midnight (2026-01-19T16:00:00Z) is the bucket
        // start — must be included.
        repo.record("k-1", dt(2026, 1, 19, 16, 0, 0), 1, Decimal::ZERO);

        let quota = core_quota(core_calendar(api_key_quota_calendar_duration_unit::DAY));
        let snapshot = service()
            .get_quota_in_offset(&repo, "k-1", Some(&quota), now, shanghai)
            .await?
            .ok_or(QuotaError::InvalidPeriod("expected snapshot".into()))?;

        assert_eq!(
            snapshot.window.start,
            Some(dt(2026, 1, 19, 16, 0, 0)),
            "calendar bucket must align to local midnight"
        );
        // 999 (UTC midnight = Shanghai 08:00, inside) + 1 (Shanghai midnight,
        // bucket start) = 1000; the 5-token pre-bucket-start log is excluded.
        assert_eq!(
            snapshot.usage.tokens, 1_000,
            "UTC-midnight log (Shanghai 08:00) must be inside the local bucket"
        );
        Ok(())
    }

    // ---- QuotaPeriod::from_core mapping ------------------------------------

    #[test]
    fn from_core_all_time_maps() -> QuotaResult<()> {
        let period = QuotaPeriod::from_core(&core_all_time())?;
        assert_eq!(period, QuotaPeriod::AllTime);
        Ok(())
    }

    #[test]
    fn from_core_past_duration_maps() -> QuotaResult<()> {
        let period = QuotaPeriod::from_core(&core_past_duration(
            24,
            api_key_quota_past_duration_unit::HOUR,
        ))?;
        assert_eq!(
            period,
            QuotaPeriod::PastDuration {
                unit: QuotaDurationUnit::Hour,
                amount: 24,
            }
        );
        Ok(())
    }

    #[test]
    fn from_core_calendar_maps() -> QuotaResult<()> {
        let period =
            QuotaPeriod::from_core(&core_calendar(api_key_quota_calendar_duration_unit::MONTH))?;
        assert_eq!(
            period,
            QuotaPeriod::CalendarDuration {
                unit: QuotaCalendarUnit::Month,
                amount: 1,
            }
        );
        Ok(())
    }

    #[test]
    fn from_core_past_duration_zero_value_is_invalid() {
        let err = QuotaPeriod::from_core(&core_past_duration(
            0,
            api_key_quota_past_duration_unit::MINUTE,
        ));
        assert!(matches!(err, Err(QuotaError::InvalidPeriodAmount)));
    }

    #[test]
    fn from_core_unknown_period_type_is_invalid() {
        let bad = CoreAPIKeyQuotaPeriod {
            r#type: "banana".into(),
            past_duration: None,
            calendar_duration: None,
        };
        assert!(matches!(
            QuotaPeriod::from_core(&bad),
            Err(QuotaError::InvalidPeriod(_))
        ));
    }

    // ---- quota_exhausted ConduitError mapping ---------------------------------

    #[test]
    fn exceeded_quota_maps_to_quota_exhausted_conduit_error() {
        let err = QuotaError::Exceeded {
            policy_id: "monthly-cost".to_string(),
            limit: QuotaLimitKind::Cost,
            limit_value: "1".to_string(),
            attempted_value: "1.01".to_string(),
            period: QuotaPeriod::all_time(),
            window: QuotaWindow::all_time(),
        };

        let conduit_error: ConduitError = err.into();

        assert_eq!(conduit_error.kind, ErrorKind::QuotaExhausted);
        assert_eq!(conduit_error.http_status, 429);
        assert_eq!(conduit_error.error_type(), "quota_exhausted");
    }

    #[test]
    fn invalid_quota_maps_to_invalid_request_conduit_error() {
        let err = QuotaError::InvalidPeriodAmount;
        let conduit_error: ConduitError = err.into();
        assert_eq!(conduit_error.kind, ErrorKind::InvalidRequest);
        assert_eq!(conduit_error.http_status, 400);
    }

    // ---- S11: QuotaChannelStatus::effective_status (Go parity) --------------
    //
    // Direct ports of `conduit/internal/server/biz/quota_channel_status_test.go`
    // golden cases. Each Rust test name maps 1:1 to a Go test name.

    fn limit_status(
        limit_type: QuotaLimitType,
        status: &str,
        usage_ratio: f64,
        ready: bool,
    ) -> QuotaLimitStatus {
        QuotaLimitStatus {
            limit_type,
            status: ProviderQuotaStatusKind::from_str_ci(status),
            usage_ratio,
            ready,
        }
    }

    #[test]
    fn effective_status_no_limits_returns_channel_level_status() {
        // Go: TestQuotaChannelStatus_EffectiveStatus_NoLimits
        let s = QuotaChannelStatus::new(ProviderQuotaStatusKind::Warning, true);

        let (status, ready) = s.effective_status(QuotaLimitType::Token);
        assert_eq!(status, ProviderQuotaStatusKind::Warning);
        assert!(ready);
    }

    #[test]
    fn effective_status_image_exhausted_token_available_independent() {
        // Go: TestQuotaChannelStatus_EffectiveStatus_ImageExhausted_TokenAvailable
        let s = QuotaChannelStatus {
            status: ProviderQuotaStatusKind::Warning,
            ready: true,
            limits: vec![
                limit_status(QuotaLimitType::Image, "exhausted", 1.0, false),
                limit_status(QuotaLimitType::Token, "available", 0.3, true),
            ],
        };

        let (img_status, img_ready) = s.effective_status(QuotaLimitType::Image);
        assert_eq!(img_status, ProviderQuotaStatusKind::Exhausted);
        assert!(!img_ready);

        let (tkn_status, tkn_ready) = s.effective_status(QuotaLimitType::Token);
        assert_eq!(tkn_status, ProviderQuotaStatusKind::Available);
        assert!(tkn_ready);
    }

    #[test]
    fn effective_status_image_warning_does_not_affect_tokens() {
        // Go: TestQuotaChannelStatus_EffectiveStatus_ImageWarning_DoesNotAffectTokens
        let s = QuotaChannelStatus {
            status: ProviderQuotaStatusKind::Warning,
            ready: true,
            limits: vec![
                limit_status(QuotaLimitType::Image, "warning", 0.9, true),
                limit_status(QuotaLimitType::Token, "available", 0.3, true),
            ],
        };

        let (img_status, _) = s.effective_status(QuotaLimitType::Image);
        assert_eq!(img_status, ProviderQuotaStatusKind::Warning);

        let (tkn_status, _) = s.effective_status(QuotaLimitType::Token);
        assert_eq!(tkn_status, ProviderQuotaStatusKind::Available);
    }

    #[test]
    fn effective_status_multiple_token_limits_worst_wins() {
        // Go: TestQuotaChannelStatus_EffectiveStatus_MultipleTokenLimits_WorstWins
        let s = QuotaChannelStatus {
            status: ProviderQuotaStatusKind::Warning,
            ready: true,
            limits: vec![
                limit_status(QuotaLimitType::Token, "available", 0.3, true),
                limit_status(QuotaLimitType::Token, "warning", 0.85, true),
            ],
        };

        let (status, ready) = s.effective_status(QuotaLimitType::Token);
        assert_eq!(status, ProviderQuotaStatusKind::Warning);
        assert!(ready);
    }

    #[test]
    fn effective_status_no_matching_limit_falls_back_to_channel_status() {
        // Go: TestQuotaChannelStatus_EffectiveStatus_NoMatchingLimit_Fallback.
        // NOTE: Go returns (Unknown, true) here — "missing data should not
        // block routing" — even though channel-level status is Available.
        let s = QuotaChannelStatus {
            status: ProviderQuotaStatusKind::Available,
            ready: true,
            limits: vec![limit_status(QuotaLimitType::Image, "exhausted", 1.0, false)],
        };

        let (status, ready) = s.effective_status(QuotaLimitType::Token);
        assert_eq!(status, ProviderQuotaStatusKind::Unknown);
        assert!(ready);
    }

    #[test]
    fn effective_status_both_exhausted() {
        // Go: TestQuotaChannelStatus_EffectiveStatus_BothExhausted
        let s = QuotaChannelStatus {
            status: ProviderQuotaStatusKind::Exhausted,
            ready: false,
            limits: vec![
                limit_status(QuotaLimitType::Image, "exhausted", 1.0, false),
                limit_status(QuotaLimitType::Token, "exhausted", 1.0, false),
            ],
        };

        let (img_status, img_ready) = s.effective_status(QuotaLimitType::Image);
        assert_eq!(img_status, ProviderQuotaStatusKind::Exhausted);
        assert!(!img_ready);

        let (tkn_status, tkn_ready) = s.effective_status(QuotaLimitType::Token);
        assert_eq!(tkn_status, ProviderQuotaStatusKind::Exhausted);
        assert!(!tkn_ready);
    }

    #[test]
    fn effective_status_all_limits_unknown() {
        // Go: TestQuotaChannelStatus_EffectiveStatus_AllLimitsUnknown
        let s = QuotaChannelStatus {
            status: ProviderQuotaStatusKind::Available,
            ready: true,
            limits: vec![
                limit_status(QuotaLimitType::Token, "unknown", 0.0, false),
                limit_status(QuotaLimitType::Image, "unknown", 0.0, false),
            ],
        };

        let (tkn_status, tkn_ready) = s.effective_status(QuotaLimitType::Token);
        assert_eq!(
            tkn_status,
            ProviderQuotaStatusKind::Unknown,
            "all-unknown limits should return unknown status"
        );
        assert!(!tkn_ready, "all-unknown limits should not be ready");

        let (img_status, img_ready) = s.effective_status(QuotaLimitType::Image);
        assert_eq!(img_status, ProviderQuotaStatusKind::Unknown);
        assert!(!img_ready);
    }

    #[test]
    fn effective_status_channel_exhausted_overrides_per_limit_available() {
        // Go: TestEffectiveStatus_ChannelExhaustedOverridesPerLimitAvailable
        let s = QuotaChannelStatus {
            status: ProviderQuotaStatusKind::Exhausted,
            ready: false,
            limits: vec![limit_status(QuotaLimitType::Token, "available", 0.3, true)],
        };

        let (status, ready) = s.effective_status(QuotaLimitType::Token);
        assert_eq!(status, ProviderQuotaStatusKind::Exhausted);
        assert!(!ready);
    }

    #[test]
    fn effective_status_unknown_fallback_when_no_matching_limit_type() {
        // Go: TestEffectiveStatus_UnknownFallbackWhenNoMatchingLimitType
        let s = QuotaChannelStatus {
            status: ProviderQuotaStatusKind::Warning,
            ready: true,
            limits: vec![limit_status(QuotaLimitType::Image, "exhausted", 1.0, false)],
        };

        let (status, ready) = s.effective_status(QuotaLimitType::Token);
        assert_eq!(status, ProviderQuotaStatusKind::Unknown);
        assert!(ready);
    }

    #[test]
    fn effective_status_equal_rank_ready_aggregation() {
        // Go: TestEffectiveStatus_EqualRankReadyAggregation.
        // Two warning limits: one ready, one not ready. Equal rank tie →
        // worstReady = ready1 && ready2 = false.
        let s = QuotaChannelStatus {
            status: ProviderQuotaStatusKind::Warning,
            ready: true,
            limits: vec![
                limit_status(QuotaLimitType::Token, "warning", 0.85, true),
                limit_status(QuotaLimitType::Token, "warning", 0.90, false),
            ],
        };

        let (status, ready) = s.effective_status(QuotaLimitType::Token);
        assert_eq!(status, ProviderQuotaStatusKind::Warning);
        assert!(!ready);
    }

    // ---- S11: enforce_channel_quota (error-code separation) -----------------

    #[test]
    fn enforce_channel_quota_available_channel_passes() {
        let svc = service();
        let s = QuotaChannelStatus::new(ProviderQuotaStatusKind::Available, true);
        assert!(svc.enforce_channel_quota(&s, QuotaLimitType::Token).is_ok());
    }

    #[test]
    fn enforce_channel_quota_warning_channel_passes() {
        // Go selector keeps warning channels in the candidate pool.
        let svc = service();
        let s = QuotaChannelStatus::new(ProviderQuotaStatusKind::Warning, true);
        assert!(svc.enforce_channel_quota(&s, QuotaLimitType::Token).is_ok());
    }

    #[test]
    fn enforce_channel_quota_unknown_channel_passes() {
        // Go selector keeps unknown channels (missing data should not block).
        let svc = service();
        let s = QuotaChannelStatus::new(ProviderQuotaStatusKind::Unknown, true);
        assert!(svc.enforce_channel_quota(&s, QuotaLimitType::Token).is_ok());
    }

    #[test]
    fn enforce_channel_quota_exhausted_channel_blocks_with_503() {
        // S11 error-code separation: channel-quota exhausted → HTTP 503
        // (distinct from api-key quota's 429).
        let svc = service();
        let s = QuotaChannelStatus::new(ProviderQuotaStatusKind::Exhausted, false);
        let err = svc.enforce_channel_quota(&s, QuotaLimitType::Token).err();
        let err = match err {
            Some(value) => value,
            None => panic!("expected channel quota rejection"),
        };

        assert_eq!(err.kind, ErrorKind::QuotaExhausted);
        assert_eq!(
            err.http_status, CHANNEL_QUOTA_EXHAUSTED_HTTP_STATUS,
            "channel quota exhausted must map to 503, not 429"
        );
        assert_eq!(err.code.as_deref(), Some(CHANNEL_QUOTA_EXHAUSTED_CODE));
        assert_eq!(err.error_type(), "quota_exhausted");
    }

    #[test]
    fn enforce_channel_quota_per_limit_exhausted_blocks_for_matching_type() {
        // Per-limit exhausted for the request's limit type blocks; the other
        // limit type passes.
        let svc = service();
        let s = QuotaChannelStatus {
            status: ProviderQuotaStatusKind::Warning,
            ready: true,
            limits: vec![
                limit_status(QuotaLimitType::Image, "exhausted", 1.0, false),
                limit_status(QuotaLimitType::Token, "available", 0.3, true),
            ],
        };

        assert!(svc.enforce_channel_quota(&s, QuotaLimitType::Token).is_ok());
        let err = svc.enforce_channel_quota(&s, QuotaLimitType::Image).err();
        let err = match err {
            Some(value) => value,
            None => panic!("expected image-limit channel quota rejection"),
        };
        assert_eq!(err.http_status, CHANNEL_QUOTA_EXHAUSTED_HTTP_STATUS);
    }

    #[test]
    fn enforce_channel_quota_image_request_maps_to_image_limit_type() {
        // Go: provider_quota.RequestModality(isImageRequest).
        assert_eq!(
            QuotaLimitType::from_request_modality(false),
            QuotaLimitType::Token
        );
        assert_eq!(
            QuotaLimitType::from_request_modality(true),
            QuotaLimitType::Image
        );
    }

    // ---- S12: enforce_provider_quota (exhausted_only vs disabled) -----------

    #[test]
    fn enforce_provider_quota_disabled_never_blocks() {
        // disabled setting (enabled=false) → never block, regardless of status.
        let svc = service();
        for status in [
            ProviderQuotaStatusKind::Available,
            ProviderQuotaStatusKind::Warning,
            ProviderQuotaStatusKind::Exhausted,
            ProviderQuotaStatusKind::Unknown,
        ] {
            assert_eq!(
                svc.enforce_provider_quota(false, status),
                ProviderQuotaDecision::Allow,
                "disabled enforcement should allow even exhausted status"
            );
        }
    }

    #[test]
    fn enforce_provider_quota_exhausted_only_blocks_only_on_exhausted() {
        // exhausted_only mode (enabled=true) → block only on Exhausted.
        let svc = service();
        assert_eq!(
            svc.enforce_provider_quota(true, ProviderQuotaStatusKind::Exhausted),
            ProviderQuotaDecision::Block
        );
        assert_eq!(
            svc.enforce_provider_quota(true, ProviderQuotaStatusKind::Available),
            ProviderQuotaDecision::Allow
        );
        assert_eq!(
            svc.enforce_provider_quota(true, ProviderQuotaStatusKind::Warning),
            ProviderQuotaDecision::Allow
        );
        assert_eq!(
            svc.enforce_provider_quota(true, ProviderQuotaStatusKind::Unknown),
            ProviderQuotaDecision::Allow
        );
    }

    #[test]
    fn enforce_provider_quota_mirrors_go_selector_filter_switch() {
        // Mirrors the Go `ProviderQuotaSelector.Select` switch arms:
        //   case Available, Warning, Unknown: return true (keep)
        //   case Exhausted: return false (filter)
        let svc = service();
        let keep = |status: ProviderQuotaStatusKind| {
            svc.enforce_provider_quota(true, status) == ProviderQuotaDecision::Allow
        };
        assert!(keep(ProviderQuotaStatusKind::Available));
        assert!(keep(ProviderQuotaStatusKind::Warning));
        assert!(keep(ProviderQuotaStatusKind::Unknown));
        assert!(!keep(ProviderQuotaStatusKind::Exhausted));
    }

    // ---- S13: plan_quota_decrement (concurrency-safe decrement plan) --------

    #[test]
    fn plan_quota_decrement_api_key_profile_uses_transactional_append() {
        // S13: API-key profile quota is decremented by appending a usage row
        // inside the request DB transaction; the pre-request gate
        // re-aggregates rows. Race-safe via SQL COUNT/SUM snapshot.
        let svc = service();
        let plan = svc.plan_quota_decrement(QuotaDecrementKind::ApiKeyProfile, 1);
        assert_eq!(plan.kind, QuotaDecrementKind::ApiKeyProfile);
        assert_eq!(plan.amount, 1);
        assert_eq!(plan.strategy, DecrementStrategy::TransactionalAppend);
    }

    #[test]
    fn plan_quota_decrement_channel_uses_atomic_cache_snapshot() {
        // S13: channel/provider quota is a read-only cached snapshot —
        // per-request decrement does NOT happen. The scheduler overwrites the
        // snapshot periodically (Go's `runQuotaCheck` → `updateQuotaCache`).
        let svc = service();
        let plan = svc.plan_quota_decrement(QuotaDecrementKind::Channel, 1);
        assert_eq!(plan.kind, QuotaDecrementKind::Channel);
        assert_eq!(plan.amount, 1);
        assert_eq!(plan.strategy, DecrementStrategy::AtomicCacheSnapshot);
    }

    #[test]
    fn plan_quota_decrement_carries_requested_amount_for_multi_unit_decrements() {
        // Token-batch decrement (e.g. 1500 tokens for one request).
        let svc = service();
        let plan = svc.plan_quota_decrement(QuotaDecrementKind::ApiKeyProfile, 1_500);
        assert_eq!(plan.amount, 1_500);
    }

    #[test]
    fn plan_quota_decrement_strategy_is_determined_only_by_kind() {
        // Pin the contract: strategy depends only on kind, never on amount.
        let svc = service();
        let small = svc.plan_quota_decrement(QuotaDecrementKind::Channel, 1);
        let large = svc.plan_quota_decrement(QuotaDecrementKind::Channel, 1_000_000);
        assert_eq!(small.strategy, large.strategy);
    }
}
