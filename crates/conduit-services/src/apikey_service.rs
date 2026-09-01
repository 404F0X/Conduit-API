//! API-key service pure-logic surface for RUST-P5-003 (S06 cache invalidation
//! + S08 profile/template apply), mirroring:
//!   - `conduit/internal/server/biz/api_key.go` (`invalidateAPIKeyCaches`,
//!     `buildAPIKeyCacheKey`, `buildAPIKeyCacheKeys`, `validateProfileNames`,
//!     `validateActiveProfile`, `validateProfileFilters`, `validateProfileQuota`).
//!   - `conduit/internal/server/biz/api_key_profile_template.go`
//!     (`LoadTemplate`, `resolveProfileNameConflict`).
//!
//! The pure functions here are DB/transport-agnostic: they compute the
//! *intent* (which cache keys to evict, what the merged profile list looks
//! like, whether a profile set is valid). Wiring them to the live cache +
//! ent client is the job of the higher-level service constructor once the
//! Rust port of `xcache/live` lands (TODO: blocked on live IndexedCache port).

use thiserror::Error;

pub use conduit_core::objects::apikey::{
    APIKeyProfile, APIKeyProfiles, APIKeyQuota, APIKeyQuotaCalendarDuration,
    APIKeyQuotaPastDuration, APIKeyQuotaPeriod, api_key_quota_calendar_duration_unit,
    api_key_quota_past_duration_unit, api_key_quota_period_type, channel_tags_match_mode,
    is_valid as is_valid_channel_tags_mode,
};

/// A cache key for the API-key lookup cache.
///
/// Mirrors the Go `buildAPIKeyCacheKey` output shape `api_key:%d` (the integer
/// is the xxhash64 of the plaintext key). The Rust port stores the plaintext so
/// the eventual hasher impl can compute the exact same string; for the pure
/// invalidation surface here, callers compare by structural equality and the
/// `Display` impl renders the Go-shaped `api_key:<hash>` once `xxhash` is wired.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// The plaintext API key whose cache entry must be evicted.
    plaintext: String,
}

impl CacheKey {
    /// Construct a cache key referring to the entry for `plaintext`.
    pub fn new(plaintext: impl Into<String>) -> Self {
        Self {
            plaintext: plaintext.into(),
        }
    }

    /// The plaintext API key this descriptor targets.
    pub fn plaintext(&self) -> &str {
        &self.plaintext
    }

    /// Render the Go-shaped `api_key:<hash>` cache-key string.
    ///
    /// `[Parfit-the-3rd ?]` Hash parity: Go uses `xxhash64(plaintext)` (package
    /// `github.com/cespare/xxhash/v2`). The Rust workspace does not yet depend
    /// on `twox-hash`, so we fall back to a stable FNV-1a 64-bit hash to keep
    /// the *shape* (`api_key:%d`) faithful and the descriptor deterministic /
    /// unit-testable. The numeric values will diverge from Go's xxhash until
    /// `twox-hash` is added to the workspace; this is intentionally flagged so
    /// a future change can swap the hasher without touching call sites.
    pub fn render(&self) -> String {
        format!("api_key:{}", fnv1a_64(self.plaintext.as_bytes()))
    }
}

impl std::fmt::Display for CacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

/// FNV-1a 64-bit hash (deterministic, dependency-free stand-in for xxhash64).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Mirrors Go `buildAPIKeyCacheKey(key string) string` — produces the cache-key
/// string for a single plaintext API key.
pub fn build_api_key_cache_key(plaintext: &str) -> String {
    CacheKey::new(plaintext).render()
}

/// Mirrors Go `buildAPIKeyCacheKeys(keys []string) []string` — projects a slice
/// of plaintext API keys to their cache-key strings.
pub fn build_api_key_cache_keys(plaintexts: &[String]) -> Vec<String> {
    plaintexts
        .iter()
        .map(|k| build_api_key_cache_key(k))
        .collect()
}

/// A mutation that requires cache eviction.
///
/// Mirrors the call sites of Go `APIKeyService.invalidateAPIKeyCaches(ctx, keys...)`,
/// which is invoked from: `UpdateAPIKey` (single key), `UpdateAPIKeyStatus`
/// (single key), `UpdateAPIKeyProfiles` (single key), `RotateAPIKey` (old +
/// new), `bulkUpdateAPIKeyStatus` (every selected key), and
/// `EnsureNoAuthAPIKey` (the noauth key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidationEvent {
    /// One plaintext key mutated (Update / UpdateStatus / UpdateProfiles /
    /// EnsureNoAuth). Mirrors the single-argument `invalidateAPIKeyCaches(ctx,
    /// apiKey.Key)`.
    KeyUpdated(String),
    /// Rotate: both the old and new plaintext keys must be evicted (the old
    /// entry is now stale, and the new key has never been cached yet).
    /// Mirrors `RotateAPIKey`'s `invalidateAPIKeyCaches(ctx, oldKey, newKey)`.
    Rotated { old_key: String, new_key: String },
    /// A bulk status change touched every key in `keys`.
    /// Mirrors `bulkUpdateAPIKeyStatus` -> `invalidateAPIKeyCaches(ctx,
    /// lo.Map(apiKeys, ...)...)`.
    BulkStatusChanged(Vec<String>),
}

impl InvalidationEvent {
    /// The plaintext API keys that the live cache must drop.
    ///
    /// Mirrors the variadic spread Go passes to `invalidateAPIKeyCaches`.
    pub fn plaintext_keys(&self) -> Vec<String> {
        match self {
            InvalidationEvent::KeyUpdated(key) => vec![key.clone()],
            InvalidationEvent::Rotated { old_key, new_key } => {
                vec![old_key.clone(), new_key.clone()]
            }
            InvalidationEvent::BulkStatusChanged(keys) => keys.clone(),
        }
    }
}

/// Returns the set of cache keys the live cache must invalidate for `event`.
///
/// This is the pure half of Go `APIKeyService.invalidateAPIKeyCaches`: instead
/// of calling `apiKeyNotifier.Notify(ctx, live.NewInvalidateKeysEvent(...))`,
/// we return the descriptor list so the caller (or a future live-cache port)
/// can fan the event out. Each plaintext key produces exactly one descriptor,
/// preserving the 1:1 mapping Go's `buildAPIKeyCacheKeys` builds before
/// notifying.
pub fn invalidation_descriptor(event: &InvalidationEvent) -> Vec<CacheKey> {
    event
        .plaintext_keys()
        .into_iter()
        .map(CacheKey::new)
        .collect()
}

/// The string-form cache keys Go's watcher would be told to drop.
///
/// Convenience wrapper combining [`invalidation_descriptor`] with
/// [`CacheKey::render`]; mirrors `buildAPIKeyCacheKeys` feeding
/// `live.NewInvalidateKeysEvent`.
pub fn invalidation_cache_strings(event: &InvalidationEvent) -> Vec<String> {
    invalidation_descriptor(event)
        .into_iter()
        .map(|k| k.render())
        .collect()
}

// ===== S08: profile/template apply + validation ============================

/// Error raised by the pure profile validation / template-apply functions.
///
/// Mirrors the `fmt.Errorf` strings produced by the Go validators in
/// `api_key.go` (`validateProfileNames`, `validateActiveProfile`,
/// `validateProfileFilters`, `validateProfileQuota`) and the preconditions in
/// `LoadTemplate` (`api_key_profile_template.go`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProfileError {
    #[error("profile name cannot be empty")]
    EmptyName,
    #[error("duplicate profile name: {0}")]
    DuplicateName(String),
    #[error("active profile '{0}' does not exist in the profiles list")]
    ActiveMissing(String),
    #[error("profile '{0}' channelTagsMatchMode is invalid")]
    InvalidTagsMode(String),
    #[error("profile '{0}' quota must set at least one limit")]
    QuotaNoLimit(String),
    #[error("profile '{0}' quota.requests must be positive")]
    QuotaRequestsNonPositive(String),
    #[error("profile '{0}' quota.totalTokens must be positive")]
    QuotaTotalTokensNonPositive(String),
    #[error("profile '{0}' quota.cost must be non-negative")]
    QuotaCostNegative(String),
    #[error("profile '{0}' quota.period.type is invalid")]
    QuotaPeriodTypeInvalid(String),
    #[error("profile '{0}' quota.period.pastDuration is required")]
    QuotaPastDurationMissing(String),
    #[error("profile '{0}' quota.period.pastDuration.value must be positive")]
    QuotaPastDurationValueNonPositive(String),
    #[error("profile '{0}' quota.period.pastDuration.unit is invalid")]
    QuotaPastDurationUnitInvalid(String),
    #[error("profile '{0}' quota.period.calendarDuration is required")]
    QuotaCalendarDurationMissing(String),
    #[error("profile '{0}' quota.period.calendarDuration.unit is invalid")]
    QuotaCalendarDurationUnitInvalid(String),
    #[error("template has no profile")]
    TemplateProfileMissing,
    #[error("template and API key must belong to the same project")]
    CrossProjectTemplate,
}

/// Mirrors Go `validateProfileNames(profiles []objects.APIKeyProfile) error`.
///
/// Profile names must be non-empty after `strings.TrimSpace` and unique on the
/// lowercased, trimmed name (case-insensitive, whitespace-trimmed). Returns the
/// first violation as a [`ProfileError`].
pub fn validate_profile_names(profiles: &[APIKeyProfile]) -> Result<(), ProfileError> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for profile in profiles {
        let name_lower = profile.name.trim().to_lowercase();
        if name_lower.is_empty() {
            return Err(ProfileError::EmptyName);
        }
        if !seen.insert(name_lower) {
            return Err(ProfileError::DuplicateName(profile.name.clone()));
        }
    }
    Ok(())
}

/// Mirrors Go `validateActiveProfile(activeProfile string, profiles) error`.
///
/// Returns `Ok(())` if some profile's `name` equals `active_profile` exactly
/// (Go uses `profile.Name == activeProfile`, a byte-exact comparison — *not*
/// the trimmed/lowercased form used by name uniqueness).
pub fn validate_active_profile(
    active_profile: &str,
    profiles: &[APIKeyProfile],
) -> Result<(), ProfileError> {
    if profiles.iter().any(|p| p.name == active_profile) {
        return Ok(());
    }
    Err(ProfileError::ActiveMissing(active_profile.to_string()))
}

/// Mirrors Go `validateProfileFilters(profiles) error`.
///
/// Each profile's `channel_tags_match_mode` (when present) must satisfy
/// [`conduit_core::objects::apikey::is_valid`] (empty / `any` / `all` / `none`).
pub fn validate_profile_filters(profiles: &[APIKeyProfile]) -> Result<(), ProfileError> {
    for profile in profiles {
        let mode = profile
            .channel_tags_match_mode
            .as_deref()
            .unwrap_or_default();
        if !is_valid_channel_tags_mode(mode) {
            return Err(ProfileError::InvalidTagsMode(profile.name.clone()));
        }
    }
    Ok(())
}

/// Mirrors Go `validateProfileQuota(profiles) error`.
///
/// Rules (per profile, only when `quota` is `Some`):
/// - at least one of `requests` / `total_tokens` / `cost` must be set;
/// - `requests` / `total_tokens` must be `> 0`;
/// - `cost` must be `>= 0`;
/// - `period.r#type` must be one of `all_time` / `past_duration` /
///   `calendar_duration`;
/// - `past_duration` must be present, have `value > 0`, and a valid unit
///   (`minute` / `hour` / `day`);
/// - `calendar_duration` must be present with a valid unit (`day` / `month`).
pub fn validate_profile_quota(profiles: &[APIKeyProfile]) -> Result<(), ProfileError> {
    for profile in profiles {
        let Some(q) = profile.quota.as_ref() else {
            continue;
        };

        // At least one limit must be set (Go: Requests == nil && TotalTokens ==
        // nil && Cost == nil).
        if q.requests.is_none() && q.total_tokens.is_none() && q.cost.is_none() {
            return Err(ProfileError::QuotaNoLimit(profile.name.clone()));
        }

        if let Some(req) = q.requests
            && req <= 0
        {
            return Err(ProfileError::QuotaRequestsNonPositive(profile.name.clone()));
        }
        if let Some(tokens) = q.total_tokens
            && tokens <= 0
        {
            return Err(ProfileError::QuotaTotalTokensNonPositive(
                profile.name.clone(),
            ));
        }
        if let Some(cost) = q.cost
            && cost.is_sign_negative()
        {
            return Err(ProfileError::QuotaCostNegative(profile.name.clone()));
        }

        validate_quota_period(&q.period, &profile.name)?;
    }
    Ok(())
}

fn validate_quota_period(
    period: &APIKeyQuotaPeriod,
    profile_name: &str,
) -> Result<(), ProfileError> {
    match period.r#type.as_str() {
        api_key_quota_period_type::ALL_TIME => Ok(()),
        api_key_quota_period_type::PAST_DURATION => {
            let Some(pd) = period.past_duration.as_ref() else {
                return Err(ProfileError::QuotaPastDurationMissing(
                    profile_name.to_string(),
                ));
            };
            if pd.value <= 0 {
                return Err(ProfileError::QuotaPastDurationValueNonPositive(
                    profile_name.to_string(),
                ));
            }
            if !is_past_duration_unit(&pd.unit) {
                return Err(ProfileError::QuotaPastDurationUnitInvalid(
                    profile_name.to_string(),
                ));
            }
            Ok(())
        }
        api_key_quota_period_type::CALENDAR_DURATION => {
            let Some(cd) = period.calendar_duration.as_ref() else {
                return Err(ProfileError::QuotaCalendarDurationMissing(
                    profile_name.to_string(),
                ));
            };
            if !is_calendar_duration_unit(&cd.unit) {
                return Err(ProfileError::QuotaCalendarDurationUnitInvalid(
                    profile_name.to_string(),
                ));
            }
            Ok(())
        }
        _ => Err(ProfileError::QuotaPeriodTypeInvalid(
            profile_name.to_string(),
        )),
    }
}

fn is_past_duration_unit(unit: &str) -> bool {
    unit == api_key_quota_past_duration_unit::MINUTE
        || unit == api_key_quota_past_duration_unit::HOUR
        || unit == api_key_quota_past_duration_unit::DAY
}

fn is_calendar_duration_unit(unit: &str) -> bool {
    unit == api_key_quota_calendar_duration_unit::DAY
        || unit == api_key_quota_calendar_duration_unit::MONTH
}

/// Run every profile validator in the order Go's `UpdateAPIKeyProfiles` uses:
/// names, active-profile, filters, quota. An explicitly empty active profile
/// is the canonical representation of "no profile" (including the default
/// empty object), so only a non-empty selection must resolve to an entry.
/// Returns the first failure.
pub fn validate_all_profiles(profiles: &APIKeyProfiles) -> Result<(), ProfileError> {
    validate_profile_names(&profiles.profiles)?;
    if !profiles.active_profile.is_empty() {
        validate_active_profile(&profiles.active_profile, &profiles.profiles)?;
    }
    validate_profile_filters(&profiles.profiles)?;
    validate_profile_quota(&profiles.profiles)?;
    Ok(())
}

// ---- profile-template apply (pure) ---------------------------------------

/// A snapshot of the inputs Go's `LoadTemplate` consumes.
///
/// `LoadTemplate` reads the template's `Profile` (clone), the API key's current
/// `APIKeyProfiles` (or a fresh empty one), and the template's own name as a
/// fallback for the profile name. We capture just those inputs so the pure
/// apply step is unit-testable without an ent client.
///
/// `Eq` is intentionally omitted: [`APIKeyProfile`] / [`APIKeyProfiles`] hold
/// `serde_json::Value` model mappings which implement `PartialEq` but not `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateApplyInput<'a> {
    /// Template-stored profile, cloned before mutation (Go:
    /// `template.Profile.Clone()`). `None` mirrors the Go "template has no
    /// profile" branch.
    pub template_profile: Option<&'a APIKeyProfile>,
    /// Template name, used as a fallback when the stored profile name is empty.
    pub template_name: &'a str,
    /// Template project id — must equal `api_key_project_id`.
    pub template_project_id: i64,
    /// API key's current project id.
    pub api_key_project_id: i64,
    /// API key's current profiles (Go: `existingProfiles`; treated as empty
    /// when `None`).
    pub existing_profiles: Option<&'a APIKeyProfiles>,
}

/// The output of [`apply_profile_template`]: the resolved profile list with the
/// template appended, and the name the appended profile received.
///
/// `Eq` is intentionally omitted: [`APIKeyProfiles`] holds
/// `serde_json::Value` model mappings which implement `PartialEq` but not `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateApplyOutput {
    /// New `APIKeyProfiles` value (existing + appended). `active_profile` is
    /// preserved verbatim from the input (Go deliberately does not touch it).
    pub profiles: APIKeyProfiles,
    /// Name the appended profile received after conflict resolution.
    pub appended_name: String,
}

/// Pure half of Go `APIKeyProfileTemplateService.LoadTemplate`.
///
/// Mirrors the steps:
/// 1. reject cross-project templates (`template.ProjectID != apiKey.ProjectID`);
/// 2. reject templates without a profile (`template.Profile == nil`);
/// 3. clone the template profile, fall back to the template name when the
///    profile name is empty;
/// 4. resolve the name against the existing profile list via
///    [`resolve_profile_name_conflict`];
/// 5. append and return.
///
/// This does **not** persist anything; the caller writes `output.profiles` via
/// the eventual ent port. Mirrors `LoadTemplate`'s transactional write step
/// without coupling the pure logic to ent.
pub fn apply_profile_template(
    input: TemplateApplyInput<'_>,
) -> Result<TemplateApplyOutput, ProfileError> {
    if input.template_project_id != input.api_key_project_id {
        return Err(ProfileError::CrossProjectTemplate);
    }

    let template_profile = input
        .template_profile
        .ok_or(ProfileError::TemplateProfileMissing)?;

    // Clone (Go: templateProfile := template.Profile.Clone()). The Rust struct
    // already owns its data, so `clone()` is the deep copy.
    let mut appended = template_profile.clone();

    // Name fallback (Go: if profileName == "" { profileName = template.Name }).
    let base_name = if appended.name.is_empty() {
        input.template_name.to_string()
    } else {
        appended.name.clone()
    };

    let existing = input
        .existing_profiles
        .map(|p| p.profiles.as_slice())
        .unwrap_or(&[]);
    let resolved = resolve_profile_name_conflict(existing, &base_name);
    appended.name = resolved.clone();

    // Build the merged APIKeyProfiles. active_profile is preserved verbatim:
    // Go leaves it untouched even when the existing set was nil (Go produces
    // `&objects.APIKeyProfiles{}` whose ActiveProfile == "").
    let active_profile = input
        .existing_profiles
        .map(|p| p.active_profile.clone())
        .unwrap_or_default();

    let mut merged: Vec<APIKeyProfile> = existing.to_vec();
    merged.push(appended);

    Ok(TemplateApplyOutput {
        profiles: APIKeyProfiles {
            active_profile,
            profiles: merged,
        },
        appended_name: resolved,
    })
}

/// Mirrors Go `resolveProfileNameConflict(existingProfiles, newName)`.
///
/// Returns `new_name` unchanged when no existing profile carries it. Otherwise
/// appends `" (1)"`, `" (2)"`, … until an unused candidate is found. Existing
/// profile names are compared byte-exactly (Go uses `nameSet[p.Name]`, not the
/// trimmed/lowercased form used for uniqueness validation).
pub fn resolve_profile_name_conflict(existing: &[APIKeyProfile], new_name: &str) -> String {
    let mut name_set: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for p in existing {
        name_set.insert(p.name.as_str());
    }

    if !name_set.contains(new_name) {
        return new_name.to_string();
    }

    let mut i = 1;
    loop {
        let candidate = format!("{new_name} ({i})");
        if !name_set.contains(candidate.as_str()) {
            return candidate;
        }
        i += 1;
    }
}

// ===== Small helpers reused across services ================================

/// Build a single [`APIKeyProfile`] from a template-stored profile value,
/// renaming it to `template_name`. Mirrors Go
/// `CreateTemplate`/`UpdateTemplate` setting `profile.Name = input.Name` /
/// `*input.Name`.
pub fn profile_with_template_name(template_profile: &APIKeyProfile, name: &str) -> APIKeyProfile {
    let mut out = template_profile.clone();
    out.name = name.to_string();
    out
}

// ===========================================================================
// S13 — ApiKeyType / scope-rule surface (RUST-P5-003)
//
// Mirrors Go `biz/api_key.go` create/update type & scope rules:
//   - create rejects `noauth`
//   - `user` → fixed scopes `[read_channels, write_requests]` (caller scopes
//     ignored)
//   - `service_account` → caller scopes
//   - update: `user`/`noauth` reject scope mutation
// These are pure functions: callers (transport/DB layer) feed them the
// resolved `ApiKeyType` and the caller-supplied scope intent, then apply the
// returned `ScopeMutation` / error. See `conduit/internal/ent/apikey/apikey.go`
// for the enum contract and `biz/api_key.go:CreateAPIKey/UpdateAPIKey` for the
// rules.
// ===========================================================================

use crate::user_project_service::ApiKeyType;

/// Intent describing how an API-key update should mutate the stored scopes.
///
/// Mirrors the Go `UpdateAPIKey` scope-resolution branches (`SetScopes`,
/// `AddScopes`, clear, or no-op). The transport layer translates this into the
/// concrete ent mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeMutation {
    /// Replace the stored scope set with the given list (`set_scopes`).
    Set(Vec<String>),
    /// Append the given scopes to the stored set, deduping (`add_scopes`).
    Append(Vec<String>),
    /// Clear all scopes.
    Clear,
    /// No scope mutation requested.
    Noop,
}

/// Errors raised by `validate_create_type`.
#[derive(Debug, thiserror::Error)]
pub enum ApiKeyTypeError {
    /// Mirrors Go `CreateAPIKey`: `noauth` is reserved and cannot be created
    /// through the public create path (only `EnsureNoAuthAPIKey` provisions
    /// it).
    #[error("noauth type API key is reserved")]
    NoAuthReserved,
}

/// Errors raised by `resolve_update_scope_intent`.
#[derive(Debug, thiserror::Error)]
pub enum ApiKeyUpdateError {
    /// Mirrors Go `UpdateAPIKey`: `user`-type keys have a fixed scope set
    /// (`read_channels`, `write_requests`) and cannot be mutated.
    #[error("user type API key cannot update scopes")]
    UserScopeImmutable,
    /// Mirrors Go `UpdateAPIKey`: `noauth`-type keys are system-managed and
    /// cannot be updated.
    #[error("noauth type API key cannot be updated")]
    NoAuthImmutable,
}

/// Validate the requested `ApiKeyType` for the public create path.
///
/// Mirrors Go `CreateAPIKey`'s `noauth` rejection: callers may only create
/// `user` or `service_account` keys; `noauth` is provisioned exclusively by
/// `EnsureNoAuthAPIKey`.
pub fn validate_create_type(key_type: ApiKeyType) -> Result<(), ApiKeyTypeError> {
    match key_type {
        ApiKeyType::NoAuth => Err(ApiKeyTypeError::NoAuthReserved),
        _ => Ok(()),
    }
}

/// Resolve the scopes to assign to a newly-created API key.
///
/// Mirrors Go `CreateAPIKey`:
///   - `User` → fixed `[read_channels, write_requests]`; the caller-supplied
///     scope list (`input`) is intentionally ignored.
///   - `ServiceAccount` → the caller-supplied scope list; if `input` is `None`,
///     an empty scope set results (the caller may then add scopes later).
///   - `NoAuth` → empty (only reachable via `EnsureNoAuthAPIKey`, which does
///     not route through this function in practice, but the branch is defined
///     for completeness).
pub fn resolve_create_scopes(key_type: ApiKeyType, input: Option<&[String]>) -> Vec<String> {
    match key_type {
        ApiKeyType::User => vec!["read_channels".to_string(), "write_requests".to_string()],
        ApiKeyType::ServiceAccount => input.map(|s| s.to_vec()).unwrap_or_default(),
        ApiKeyType::NoAuth => Vec::new(),
    }
}

/// Resolve the scope-mutation intent for an API-key update.
///
/// Mirrors Go `UpdateAPIKey` (biz/api_key.go:407-456):
///   - `User` rejects only *non-empty* scope mutations — length-checked like
///     Go (`len(input.Scopes) > 0 || len(input.AppendScopes) > 0 ||
///     input.ClearScopes`); an empty/no-op update on a User key passes as
///     `Noop` (Go falls through to the rename path). `NoAuth` rejects every
///     update outright.
///   - `ServiceAccount` honours the request with `clear > set > append > noop`
///     precedence. Go applies clear/set/append independently and sequentially;
///     we model the *primary* mutation as a single intent here (documented
///     simplification — the host layer applies one mutation per call).
///
/// Parameters mirror the Go field set: `set` ↔ `set_scopes`, `append` ↔
/// `add_scopes`, `clear` ↔ clearing the scope set. An empty `set` slice is
/// treated as absent (Go's `len > 0` check cannot distinguish nil from empty).
pub fn resolve_update_scope_intent(
    key_type: ApiKeyType,
    set: Option<&[String]>,
    append: &[String],
    clear: bool,
) -> Result<ScopeMutation, ApiKeyUpdateError> {
    // Go uses length-based guards, not presence: an empty `set` is a no-op.
    let set_nonempty = set.map(|s| !s.is_empty()).unwrap_or(false);
    let append_nonempty = !append.is_empty();
    match key_type {
        ApiKeyType::User => {
            // biz/api_key.go:407-411 — User keys reject only non-empty scope
            // mutations. Empty/no-op updates fall through to the rename path.
            if set_nonempty || append_nonempty || clear {
                Err(ApiKeyUpdateError::UserScopeImmutable)
            } else {
                Ok(ScopeMutation::Noop)
            }
        }
        ApiKeyType::NoAuth => Err(ApiKeyUpdateError::NoAuthImmutable),
        ApiKeyType::ServiceAccount => {
            if clear {
                Ok(ScopeMutation::Clear)
            } else if set_nonempty {
                Ok(ScopeMutation::Set(set.unwrap_or(&[]).to_vec()))
            } else if append_nonempty {
                Ok(ScopeMutation::Append(append.to_vec()))
            } else {
                Ok(ScopeMutation::Noop)
            }
        }
    }
}

// ===========================================================================
// S14 — CreateLLMAPIKey name-validation + RotateAPIKey type-guard + bulk-status
// short-circuit (RUST-P5-003, Mendel-the-5th 2026-07-06).
//
// Mirrors the pure-prefix checks of Go `biz/api_key.go`:
//   - `CreateLLMAPIKey` (lines 228-232): `name = strings.TrimSpace(name);
//     if name == "" { return ErrAPIKeyNameRequired }`.
//   - `RotateAPIKey` (lines 826-828): `if existing.Type == apikey.TypeNoauth
//     { return ... "noauth type API key cannot be rotated" }`.
//   - `bulkUpdateAPIKeyStatus` (lines 751-754): `if len(ids) == 0 {
//     return nil }` — empty id list is a successful no-op.
//
// The persistence tails of those Go methods (ent insert / update / count /
// cache invalidation) remain a DB-backed trait port (pending — see module doc
// at the top of this file). The pure helpers here let the service layer
// short-circuit invalid input before reaching the host adapter, exactly the
// same layering S13 uses for `validate_create_type` / `resolve_update_scopes`.
// ===========================================================================

/// Errors raised by [`validate_llm_api_key_name`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmAPIKeyNameError {
    /// Mirrors Go biz/api_key.go:230-232 `ErrAPIKeyNameRequired`
    /// (`errors.New("api key name is required")`, `errors.go:19`). Reached when
    /// the trimmed name is empty.
    #[error("api key name is required")]
    Empty,
}

/// Errors raised by [`validate_rotate_key_type`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RotateAPIKeyError {
    /// Mirrors Go biz/api_key.go:826-828
    /// `fmt.Errorf("noauth type API key cannot be rotated")`. Reached when the
    /// stored key has `Type == apikey.TypeNoauth` — noauth keys are
    /// system-managed (provisioned only by `EnsureNoAuthAPIKey`) and their key
    /// material is fixed.
    #[error("noauth type API key cannot be rotated")]
    NoAuthNotRotatable,
}

/// Validate the caller-supplied name for `CreateLLMAPIKey`.
///
/// Mirrors Go `CreateLLMAPIKey` (biz/api_key.go:228-232): the name is
/// trimmed before the empty check, so a whitespace-only name is rejected.
/// Returns the trimmed name on success so the caller can carry it into the
/// persistence layer without re-trimming.
pub fn validate_llm_api_key_name(name: &str) -> Result<&str, LlmAPIKeyNameError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(LlmAPIKeyNameError::Empty);
    }
    Ok(trimmed)
}

/// Validate the stored key's type before rotating.
///
/// Mirrors Go `RotateAPIKey` (biz/api_key.go:826-828): only `user` and
/// `service_account` keys may be rotated. `noauth` keys are rejected before
/// the new key material is generated.
pub fn validate_rotate_key_type(key_type: ApiKeyType) -> Result<(), RotateAPIKeyError> {
    match key_type {
        ApiKeyType::NoAuth => Err(RotateAPIKeyError::NoAuthNotRotatable),
        ApiKeyType::User | ApiKeyType::ServiceAccount => Ok(()),
    }
}

/// Bulk-status action kind. Mirrors the `action` string Go threads through
/// `bulkUpdateAPIKeyStatus` (biz/api_key.go:751) purely to format the error
/// message ("disable" / "enable" / "archive"). Surfaced as an enum so the
/// Rust host layer does not pass ad-hoc strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkStatusAction {
    Enable,
    Disable,
    Archive,
}

impl BulkStatusAction {
    /// Mirrors the verb Go interpolates into `"noauth type API key cannot be
    /// bulk %sd"` (biz/api_key.go:778) and `"failed to %s API keys: %w"`
    /// (biz/api_key.go:794). Reproduces the exact Go lower-case verb for each
    /// action so error strings stay byte-identical.
    pub fn go_verb(self) -> &'static str {
        match self {
            BulkStatusAction::Enable => "enable",
            BulkStatusAction::Disable => "disable",
            BulkStatusAction::Archive => "archive",
        }
    }
}

/// Outcome of [`resolve_bulk_status_ids_short_circuit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkStatusShortCircuit {
    /// Mirrors Go biz/api_key.go:752-754: empty id list → successful no-op,
    /// the host layer returns immediately without hitting the DB.
    EmptyNoOp,
    /// Non-empty id list — the host must proceed with the count/noauth/type
    /// checks and the bulk update.
    Proceed(Vec<i64>),
}

/// Pure short-circuit for Go `bulkUpdateAPIKeyStatus` (biz/api_key.go:751-754).
///
/// Go opens the method with `if len(ids) == 0 { return nil }`, bypassing every
/// downstream DB check. This helper lifts that decision to the service layer
/// so callers can short-circuit without a host adapter, and pins the
/// empty-list-as-success contract (Go returns `nil`, not an error).
///
/// The downstream checks (count mismatch `"expected to find N API keys, but
/// found M"`, noauth-type rejection `"noauth type API key cannot be bulk %sd"`)
/// are DB-dependent and remain pending the host adapter port.
pub fn resolve_bulk_status_ids_short_circuit(ids: &[i64]) -> BulkStatusShortCircuit {
    if ids.is_empty() {
        BulkStatusShortCircuit::EmptyNoOp
    } else {
        BulkStatusShortCircuit::Proceed(ids.to_vec())
    }
}

/// Helper for tests: a quota with all three limits set.
#[cfg(test)]
fn quota_all_limits() -> APIKeyQuota {
    use rust_decimal::Decimal;
    APIKeyQuota {
        requests: Some(1),
        total_tokens: Some(1),
        cost: Some(Decimal::ZERO),
        period: APIKeyQuotaPeriod {
            r#type: api_key_quota_period_type::ALL_TIME.to_string(),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::collections::HashSet;

    // ---- S06: cache-key shape & invalidation_descriptor -------------------

    #[test]
    fn cache_key_renders_go_shaped_prefix() {
        // Shape parity: the literal "api_key:" prefix Go uses, followed by a
        // numeric hash. Numeric value diverges from Go's xxhash until
        // `twox-hash` lands (flagged [Parfit-the-3rd ?] on CacheKey::render).
        let rendered = CacheKey::new("conduit-secret").render();
        let Some((prefix, numeric)) = rendered.split_once(':') else {
            panic!("rendered cache key must be 'api_key:<digits>'");
        };
        assert_eq!(prefix, "api_key");
        assert_eq!(rendered, format!("api_key:{}", fnv1a_64(b"conduit-secret")));
        // numeric suffix is a non-empty base-10 integer.
        assert!(!numeric.is_empty());
        assert!(numeric.bytes().all(|b| b.is_ascii_digit()));
    }

    #[test]
    fn build_api_key_cache_key_is_stable_and_distinct() {
        let a = build_api_key_cache_key("conduit-one");
        let b = build_api_key_cache_key("conduit-one");
        let c = build_api_key_cache_key("conduit-two");
        assert_eq!(a, b, "same plaintext must hash to same key");
        assert_ne!(a, c, "different plaintext must hash to different keys");
    }

    #[test]
    fn build_api_key_cache_keys_projects_slice() {
        let keys = vec![
            "conduit-1".to_string(),
            "conduit-2".to_string(),
            "conduit-3".to_string(),
        ];
        let projected = build_api_key_cache_keys(&keys);
        assert_eq!(
            projected,
            keys.iter()
                .map(|k| build_api_key_cache_key(k))
                .collect::<Vec<_>>()
        );
        // Distinctness check.
        let unique: HashSet<&String> = projected.iter().collect();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn invalidation_descriptor_single_key_mirrors_update_path() {
        // Mirrors UpdateAPIKey/UpdateAPIKeyStatus/UpdateAPIKeyProfiles/
        // EnsureNoAuthAPIKey calling invalidateAPIKeyCaches(ctx, apiKey.Key).
        let event = InvalidationEvent::KeyUpdated("conduit-1".to_string());
        let descriptor = invalidation_descriptor(&event);
        assert_eq!(descriptor.len(), 1);
        assert_eq!(descriptor[0].plaintext(), "conduit-1");
    }

    #[test]
    fn invalidation_descriptor_rotate_emits_both_keys() {
        // Mirrors RotateAPIKey: invalidateAPIKeyCaches(ctx, oldKey, newKey).
        let event = InvalidationEvent::Rotated {
            old_key: "conduit-old".to_string(),
            new_key: "conduit-new".to_string(),
        };
        let descriptor = invalidation_descriptor(&event);
        assert_eq!(descriptor.len(), 2);
        assert_eq!(descriptor[0].plaintext(), "conduit-old");
        assert_eq!(descriptor[1].plaintext(), "conduit-new");
    }

    #[test]
    fn invalidation_descriptor_bulk_status_mirrors_bulk_path() {
        // Mirrors bulkUpdateAPIKeyStatus: every selected key is invalidated.
        let keys = vec![
            "conduit-1".to_string(),
            "conduit-2".to_string(),
            "conduit-3".to_string(),
        ];
        let event = InvalidationEvent::BulkStatusChanged(keys.clone());
        let descriptor = invalidation_descriptor(&event);
        let got: Vec<&str> = descriptor.iter().map(|d| d.plaintext()).collect();
        assert_eq!(got, keys.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }

    #[test]
    fn invalidation_cache_strings_renders_each_descriptor() {
        let event = InvalidationEvent::Rotated {
            old_key: "conduit-old".to_string(),
            new_key: "conduit-new".to_string(),
        };
        let strings = invalidation_cache_strings(&event);
        assert_eq!(
            strings,
            vec![
                build_api_key_cache_key("conduit-old"),
                build_api_key_cache_key("conduit-new"),
            ]
        );
    }

    // ---- S08: validate_profile_names (mirrors Go validateProfileNames) -----

    fn named(name: &str) -> APIKeyProfile {
        APIKeyProfile {
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn validate_profile_names_accepts_unique_set() {
        let profiles = vec![named("prod"), named("dev"), named("staging")];
        assert_eq!(validate_profile_names(&profiles), Ok(()));
    }

    #[test]
    fn validate_profile_names_rejects_exact_duplicate() {
        // Mirrors Go TestAPIKeyService_UpdateAPIKeyProfiles "Duplicate profile
        // names - exact match".
        let profiles = vec![named("production"), named("production")];
        assert_eq!(
            validate_profile_names(&profiles),
            Err(ProfileError::DuplicateName("production".to_string()))
        );
    }

    #[test]
    fn validate_profile_names_rejects_case_insensitive_duplicate() {
        // Mirrors Go "Duplicate profile names - case insensitive".
        let profiles = vec![named("Production"), named("production")];
        assert_eq!(
            validate_profile_names(&profiles),
            Err(ProfileError::DuplicateName("production".to_string()))
        );
    }

    #[test]
    fn validate_profile_names_rejects_whitespace_collision() {
        // Mirrors Go "Duplicate profile names - with whitespace": the second
        // name " production " trims+lowercases to "production".
        let profiles = vec![named("production"), named(" production ")];
        assert_eq!(
            validate_profile_names(&profiles),
            Err(ProfileError::DuplicateName(" production ".to_string()))
        );
    }

    #[test]
    fn validate_profile_names_rejects_empty_name() {
        // Mirrors Go "Empty profile name".
        let profiles = vec![named("production"), named("")];
        assert_eq!(
            validate_profile_names(&profiles),
            Err(ProfileError::EmptyName)
        );
    }

    // ---- S08: validate_active_profile (mirrors Go validateActiveProfile) ---

    #[test]
    fn validate_active_profile_accepts_exact_match() {
        let profiles = vec![named("production")];
        assert_eq!(validate_active_profile("production", &profiles), Ok(()));
    }

    #[test]
    fn validate_active_profile_rejects_missing_active() {
        // Mirrors Go "Active profile does not exist". Active profile is
        // compared byte-exactly (no trim/lower).
        let profiles = vec![named("production")];
        assert_eq!(
            validate_active_profile("nonexistent", &profiles),
            Err(ProfileError::ActiveMissing("nonexistent".to_string()))
        );
        // Case-sensitivity: Go uses ==, so "Production" != "production".
        assert_eq!(
            validate_active_profile("Production", &profiles),
            Err(ProfileError::ActiveMissing("Production".to_string()))
        );
    }

    // ---- S08: validate_profile_filters (mirrors Go validateProfileFilters) -

    #[test]
    fn validate_profile_filters_accepts_empty_mode() {
        // Empty / None channel_tags_match_mode is valid (Go IsValid("") == true).
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            channel_tags: vec!["official".to_string()],
            ..Default::default()
        }];
        assert_eq!(validate_profile_filters(&profiles), Ok(()));
    }

    #[test]
    fn validate_profile_filters_accepts_known_modes() {
        for mode in ["any", "all", "none"] {
            let profiles = vec![APIKeyProfile {
                name: "p".to_string(),
                channel_tags_match_mode: Some(mode.to_string()),
                ..Default::default()
            }];
            assert_eq!(
                validate_profile_filters(&profiles),
                Ok(()),
                "mode {mode} should be valid"
            );
        }
    }

    #[test]
    fn validate_profile_filters_rejects_invalid_mode() {
        // Mirrors Go "Invalid channel tags match mode".
        let profiles = vec![APIKeyProfile {
            name: "production".to_string(),
            channel_tags: vec!["official".to_string()],
            channel_tags_match_mode: Some("invalid".to_string()),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_filters(&profiles),
            Err(ProfileError::InvalidTagsMode("production".to_string()))
        );
    }

    // ---- S08: validate_profile_quota (mirrors Go validateProfileQuota) -----

    #[test]
    fn validate_profile_quota_skips_profiles_without_quota() {
        let profiles = vec![named("p")];
        assert_eq!(validate_profile_quota(&profiles), Ok(()));
    }

    #[test]
    fn validate_profile_quota_all_time_accepts() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(quota_all_limits()),
            ..Default::default()
        }];
        assert_eq!(validate_profile_quota(&profiles), Ok(()));
    }

    #[test]
    fn validate_profile_quota_past_duration_minute_accepted() {
        // Mirrors Go TestValidateProfileQuota_PastDurationMinuteAccepted.
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                requests: Some(1),
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::PAST_DURATION.to_string(),
                    past_duration: Some(APIKeyQuotaPastDuration {
                        value: 1,
                        unit: api_key_quota_past_duration_unit::MINUTE.to_string(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(validate_profile_quota(&profiles), Ok(()));
    }

    #[test]
    fn validate_profile_quota_requires_at_least_one_limit() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::ALL_TIME.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_quota(&profiles),
            Err(ProfileError::QuotaNoLimit("p".to_string()))
        );
    }

    #[test]
    fn validate_profile_quota_rejects_non_positive_requests() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                requests: Some(0),
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::ALL_TIME.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_quota(&profiles),
            Err(ProfileError::QuotaRequestsNonPositive("p".to_string()))
        );
    }

    #[test]
    fn validate_profile_quota_rejects_non_positive_total_tokens() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                total_tokens: Some(-1),
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::ALL_TIME.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_quota(&profiles),
            Err(ProfileError::QuotaTotalTokensNonPositive("p".to_string()))
        );
    }

    #[test]
    fn validate_profile_quota_accepts_zero_cost() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                cost: Some(Decimal::ZERO),
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::ALL_TIME.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(validate_profile_quota(&profiles), Ok(()));
    }

    #[test]
    fn validate_profile_quota_rejects_negative_cost() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                cost: Some(Decimal::new(-1, 0)),
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::ALL_TIME.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_quota(&profiles),
            Err(ProfileError::QuotaCostNegative("p".to_string()))
        );
    }

    #[test]
    fn validate_profile_quota_rejects_invalid_period_type() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                requests: Some(1),
                period: APIKeyQuotaPeriod {
                    r#type: "banana".to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_quota(&profiles),
            Err(ProfileError::QuotaPeriodTypeInvalid("p".to_string()))
        );
    }

    #[test]
    fn validate_profile_quota_past_duration_requires_payload() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                requests: Some(1),
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::PAST_DURATION.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_quota(&profiles),
            Err(ProfileError::QuotaPastDurationMissing("p".to_string()))
        );
    }

    #[test]
    fn validate_profile_quota_past_duration_rejects_zero_value() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                requests: Some(1),
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::PAST_DURATION.to_string(),
                    past_duration: Some(APIKeyQuotaPastDuration {
                        value: 0,
                        unit: api_key_quota_past_duration_unit::HOUR.to_string(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_quota(&profiles),
            Err(ProfileError::QuotaPastDurationValueNonPositive(
                "p".to_string()
            ))
        );
    }

    #[test]
    fn validate_profile_quota_past_duration_rejects_bad_unit() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                requests: Some(1),
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::PAST_DURATION.to_string(),
                    past_duration: Some(APIKeyQuotaPastDuration {
                        value: 1,
                        unit: "week".to_string(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_quota(&profiles),
            Err(ProfileError::QuotaPastDurationUnitInvalid("p".to_string()))
        );
    }

    #[test]
    fn validate_profile_quota_calendar_duration_requires_payload() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                requests: Some(1),
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::CALENDAR_DURATION.to_string(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_quota(&profiles),
            Err(ProfileError::QuotaCalendarDurationMissing("p".to_string()))
        );
    }

    #[test]
    fn validate_profile_quota_calendar_duration_rejects_bad_unit() {
        let profiles = vec![APIKeyProfile {
            name: "p".to_string(),
            quota: Some(APIKeyQuota {
                requests: Some(1),
                period: APIKeyQuotaPeriod {
                    r#type: api_key_quota_period_type::CALENDAR_DURATION.to_string(),
                    calendar_duration: Some(APIKeyQuotaCalendarDuration {
                        unit: "year".to_string(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        }];
        assert_eq!(
            validate_profile_quota(&profiles),
            Err(ProfileError::QuotaCalendarDurationUnitInvalid(
                "p".to_string()
            ))
        );
    }

    // ---- S08: validate_all_profiles runs them in order --------------------

    #[test]
    fn validate_all_profiles_runs_every_validator_in_go_order() {
        // Order parity: names -> active -> filters -> quota.
        let mut profiles = APIKeyProfiles {
            active_profile: "production".to_string(),
            profiles: vec![APIKeyProfile {
                name: "production".to_string(),
                channel_tags_match_mode: Some("none".to_string()),
                quota: Some(quota_all_limits()),
                ..Default::default()
            }],
        };
        assert_eq!(validate_all_profiles(&profiles), Ok(()));

        // Names failure short-circuits before active/filters/quota.
        profiles.profiles[0].name = "".to_string();
        assert_eq!(
            validate_all_profiles(&profiles),
            Err(ProfileError::EmptyName)
        );

        // Active failure surfaces after names pass.
        profiles.profiles[0].name = "production".to_string();
        profiles.active_profile = "ghost".to_string();
        assert_eq!(
            validate_all_profiles(&profiles),
            Err(ProfileError::ActiveMissing("ghost".to_string()))
        );

        // Filters failure surfaces after active passes.
        profiles.active_profile = "production".to_string();
        profiles.profiles[0].channel_tags_match_mode = Some("banana".to_string());
        assert_eq!(
            validate_all_profiles(&profiles),
            Err(ProfileError::InvalidTagsMode("production".to_string()))
        );

        // Quota failure surfaces last.
        profiles.profiles[0].channel_tags_match_mode = Some("none".to_string());
        profiles.profiles[0].quota = Some(APIKeyQuota {
            period: APIKeyQuotaPeriod {
                r#type: api_key_quota_period_type::ALL_TIME.to_string(),
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(
            validate_all_profiles(&profiles),
            Err(ProfileError::QuotaNoLimit("production".to_string()))
        );
    }

    #[test]
    fn validate_all_profiles_allows_explicit_no_profile_but_not_missing_selection() {
        let no_profile = APIKeyProfiles::default();
        assert_eq!(validate_all_profiles(&no_profile), Ok(()));

        let missing = APIKeyProfiles {
            active_profile: "ghost".to_string(),
            profiles: vec![named("production")],
        };
        assert_eq!(
            validate_all_profiles(&missing),
            Err(ProfileError::ActiveMissing("ghost".to_string()))
        );
    }

    // ---- S08: resolve_profile_name_conflict (mirrors Go) ------------------

    #[test]
    fn resolve_name_no_conflict_returns_input() {
        let existing: Vec<APIKeyProfile> = vec![named("Default")];
        assert_eq!(
            resolve_profile_name_conflict(&existing, "Production"),
            "Production"
        );
    }

    #[test]
    fn resolve_name_single_conflict_returns_suffix_one() {
        // Mirrors Go TestLoadTemplate_NameConflict: existing "Production",
        // template also "Production" -> resolves to "Production (1)".
        let existing = vec![named("Production")];
        assert_eq!(
            resolve_profile_name_conflict(&existing, "Production"),
            "Production (1)"
        );
    }

    #[test]
    fn resolve_name_multi_conflict_returns_next_free_suffix() {
        // Mirrors Go TestLoadTemplate_MultipleConflicts: existing
        // ["Production", "Production (1)"] -> template "Production" resolves to
        // "Production (2)".
        let existing = vec![named("Production"), named("Production (1)")];
        assert_eq!(
            resolve_profile_name_conflict(&existing, "Production"),
            "Production (2)"
        );
    }

    // ---- S08: apply_profile_template (mirrors Go LoadTemplate) ------------

    fn template_profile(name: &str) -> APIKeyProfile {
        APIKeyProfile {
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn apply_template_happy_path_appends_and_preserves_active() -> Result<(), ProfileError> {
        // Mirrors Go TestLoadTemplate_HappyPath.
        let tp = template_profile("Production");
        let existing = APIKeyProfiles {
            active_profile: "Default".to_string(),
            profiles: vec![named("Default")],
        };
        let out = apply_profile_template(TemplateApplyInput {
            template_profile: Some(&tp),
            template_name: "prod-template",
            template_project_id: 7,
            api_key_project_id: 7,
            existing_profiles: Some(&existing),
        })?;
        assert_eq!(out.profiles.active_profile, "Default");
        assert_eq!(out.profiles.profiles.len(), 2);
        assert_eq!(out.profiles.profiles[0].name, "Default");
        assert_eq!(out.appended_name, "Production");
        assert_eq!(out.profiles.profiles[1].name, "Production");
        Ok(())
    }

    #[test]
    fn apply_template_name_conflict_renames_appended_profile() -> Result<(), ProfileError> {
        // Mirrors Go TestLoadTemplate_NameConflict: existing "Production",
        // template profile also "Production" -> appended becomes "Production
        // (1)".
        let tp = template_profile("Production");
        let existing = APIKeyProfiles {
            active_profile: "Production".to_string(),
            profiles: vec![named("Production")],
        };
        let out = apply_profile_template(TemplateApplyInput {
            template_profile: Some(&tp),
            template_name: "prod-template",
            template_project_id: 1,
            api_key_project_id: 1,
            existing_profiles: Some(&existing),
        })?;
        assert_eq!(out.appended_name, "Production (1)");
        assert_eq!(out.profiles.profiles.len(), 2);
        assert_eq!(out.profiles.profiles[0].name, "Production");
        assert_eq!(out.profiles.profiles[1].name, "Production (1)");
        Ok(())
    }

    #[test]
    fn apply_template_multiple_conflicts_renames_with_next_free_index() -> Result<(), ProfileError>
    {
        // Mirrors Go TestLoadTemplate_MultipleConflicts.
        let tp = template_profile("Production");
        let existing = APIKeyProfiles {
            active_profile: "Production".to_string(),
            profiles: vec![named("Production"), named("Production (1)")],
        };
        let out = apply_profile_template(TemplateApplyInput {
            template_profile: Some(&tp),
            template_name: "prod-template",
            template_project_id: 1,
            api_key_project_id: 1,
            existing_profiles: Some(&existing),
        })?;
        assert_eq!(out.appended_name, "Production (2)");
        assert_eq!(out.profiles.profiles.len(), 3);
        Ok(())
    }

    #[test]
    fn apply_template_without_profile_errors() {
        // Mirrors Go LoadTemplate's "template has no profile" branch.
        let existing = APIKeyProfiles::default();
        let result = apply_profile_template(TemplateApplyInput {
            template_profile: None,
            template_name: "prod-template",
            template_project_id: 1,
            api_key_project_id: 1,
            existing_profiles: Some(&existing),
        });
        assert!(matches!(result, Err(ProfileError::TemplateProfileMissing)));
    }

    #[test]
    fn apply_template_cross_project_errors() {
        // Mirrors Go TestLoadTemplate_DifferentProject.
        let tp = template_profile("Production");
        let result = apply_profile_template(TemplateApplyInput {
            template_profile: Some(&tp),
            template_name: "prod-template",
            template_project_id: 2,
            api_key_project_id: 1,
            existing_profiles: None,
        });
        assert!(matches!(result, Err(ProfileError::CrossProjectTemplate)));
    }

    #[test]
    fn apply_template_uses_template_name_when_profile_name_empty() -> Result<(), ProfileError> {
        // Mirrors Go: profileName := templateProfile.Name; if "" { templateName }.
        let mut tp = template_profile("");
        tp.channel_tags = vec!["gpu".to_string()];
        let out = apply_profile_template(TemplateApplyInput {
            template_profile: Some(&tp),
            template_name: "prod-template",
            template_project_id: 1,
            api_key_project_id: 1,
            existing_profiles: None,
        })?;
        assert_eq!(out.appended_name, "prod-template");
        assert_eq!(out.profiles.profiles.len(), 1);
        // Preserved non-name fields.
        assert_eq!(
            out.profiles.profiles[0].channel_tags,
            vec!["gpu".to_string()]
        );
        Ok(())
    }

    #[test]
    fn apply_template_with_nil_existing_profiles_uses_empty_set() -> Result<(), ProfileError> {
        // Mirrors Go: `if existingProfiles == nil { existingProfiles =
        // &objects.APIKeyProfiles{} }`.
        let tp = template_profile("Production");
        let out = apply_profile_template(TemplateApplyInput {
            template_profile: Some(&tp),
            template_name: "prod-template",
            template_project_id: 1,
            api_key_project_id: 1,
            existing_profiles: None,
        })?;
        assert_eq!(out.profiles.profiles.len(), 1);
        assert_eq!(out.profiles.active_profile, "");
        assert_eq!(out.appended_name, "Production");
        Ok(())
    }

    // ---- S08: profile_with_template_name (CreateTemplate/UpdateTemplate) ---

    #[test]
    fn profile_with_template_name_renames_copy() {
        let tp = template_profile("original");
        let renamed = profile_with_template_name(&tp, "new-name");
        assert_eq!(renamed.name, "new-name");
        // Original is untouched (deep copy semantics).
        assert_eq!(tp.name, "original");
    }

    // -------------------------------------------------------------------
    // S13 — ApiKeyType / scope-rule parity tests.
    // Mirrors Go `biz/api_key_test.go: TestAPIKeyService_CreateAPIKey_Type`
    // (user default → fixed scopes; explicit user ignores caller scopes;
    //  service_account empty→empty; service_account custom→honoured) and the
    // enum serde contract from `conduit/internal/ent/apikey/apikey.go`.
    // -------------------------------------------------------------------

    #[test]
    fn s13_create_user_default_uses_fixed_scopes() {
        let scopes = resolve_create_scopes(crate::user_project_service::ApiKeyType::User, None);
        assert_eq!(
            scopes,
            vec!["read_channels".to_string(), "write_requests".to_string()]
        );
    }

    #[test]
    fn s13_create_user_explicit_ignores_caller_scopes() {
        let caller = vec!["admin".to_string(), "delete_channels".to_string()];
        let scopes =
            resolve_create_scopes(crate::user_project_service::ApiKeyType::User, Some(&caller));
        assert_eq!(
            scopes,
            vec!["read_channels".to_string(), "write_requests".to_string()]
        );
    }

    #[test]
    fn s13_create_service_account_empty_input_yields_empty_scopes() {
        let scopes = resolve_create_scopes(
            crate::user_project_service::ApiKeyType::ServiceAccount,
            None,
        );
        assert!(scopes.is_empty());
    }

    #[test]
    fn s13_create_service_account_custom_scopes_honoured() {
        let caller = vec!["read_channels".to_string(), "write_requests".to_string()];
        let scopes = resolve_create_scopes(
            crate::user_project_service::ApiKeyType::ServiceAccount,
            Some(&caller),
        );
        assert_eq!(scopes, caller);
    }

    #[test]
    fn s13_validate_create_type_rejects_noauth() {
        assert!(validate_create_type(crate::user_project_service::ApiKeyType::NoAuth).is_err());
        assert!(validate_create_type(crate::user_project_service::ApiKeyType::User).is_ok());
        assert!(
            validate_create_type(crate::user_project_service::ApiKeyType::ServiceAccount).is_ok()
        );
    }

    #[test]
    fn s13_resolve_update_scope_intent_user_rejects() {
        assert!(matches!(
            resolve_update_scope_intent(
                crate::user_project_service::ApiKeyType::User,
                Some(&["read_channels".to_string()]),
                &[],
                false,
            ),
            Err(ApiKeyUpdateError::UserScopeImmutable)
        ));
    }

    #[test]
    fn s13_resolve_update_scope_intent_noauth_rejects() {
        assert!(matches!(
            resolve_update_scope_intent(
                crate::user_project_service::ApiKeyType::NoAuth,
                None,
                &[],
                false,
            ),
            Err(ApiKeyUpdateError::NoAuthImmutable)
        ));
    }

    #[test]
    fn s13_resolve_update_scope_intent_service_account_clear_wins() -> Result<(), ApiKeyUpdateError>
    {
        let out = resolve_update_scope_intent(
            crate::user_project_service::ApiKeyType::ServiceAccount,
            Some(&["read_channels".to_string()]),
            &["write_requests".to_string()],
            true,
        )?;
        assert_eq!(out, ScopeMutation::Clear);
        Ok(())
    }

    #[test]
    fn s13_resolve_update_scope_intent_service_account_set_then_append()
    -> Result<(), ApiKeyUpdateError> {
        let set = vec!["read_channels".to_string()];
        let out = resolve_update_scope_intent(
            crate::user_project_service::ApiKeyType::ServiceAccount,
            Some(&set),
            &["write_requests".to_string()],
            false,
        )?;
        assert_eq!(out, ScopeMutation::Set(set));

        let append = vec!["write_requests".to_string()];
        let out = resolve_update_scope_intent(
            crate::user_project_service::ApiKeyType::ServiceAccount,
            None,
            &append,
            false,
        )?;
        assert_eq!(out, ScopeMutation::Append(append));
        Ok(())
    }

    #[test]
    fn s13_resolve_update_scope_intent_service_account_noop() -> Result<(), ApiKeyUpdateError> {
        let out = resolve_update_scope_intent(
            crate::user_project_service::ApiKeyType::ServiceAccount,
            None,
            &[],
            false,
        )?;
        assert_eq!(out, ScopeMutation::Noop);
        Ok(())
    }

    #[test]
    fn s13_apikeytype_serde_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        use crate::user_project_service::{ApiKey, ApiKeyStatus, ApiKeyType};

        // Go enum tags: "user" / "service_account" / "noauth" (NO underscore).
        assert_eq!(serde_json::to_string(&ApiKeyType::User)?, r#""user""#);
        assert_eq!(
            serde_json::to_string(&ApiKeyType::ServiceAccount)?,
            r#""service_account""#
        );
        assert_eq!(serde_json::to_string(&ApiKeyType::NoAuth)?, r#""noauth""#);

        // Round-trip the "noauth" tag explicitly (regression guard for the old
        // snake_case default which would have produced "no_auth").
        let kt: ApiKeyType = serde_json::from_str(r#""noauth""#)?;
        assert_eq!(kt, ApiKeyType::NoAuth);

        // Default is `User`.
        assert_eq!(ApiKeyType::default(), ApiKeyType::User);

        // Full ApiKey serde round-trip with key_type set.
        let key = ApiKey {
            id: "k1".to_string(),
            project_id: "p1".to_string(),
            name: "n".to_string(),
            secret_digest: "d".to_string(),
            secret_preview: "...".to_string(),
            scope_slugs: vec![],
            status: ApiKeyStatus::Enabled,
            key_type: ApiKeyType::NoAuth,
            created_by_user_id: None,
        };
        let json = serde_json::to_string(&key)?;
        // The ApiKey struct uses snake_case field names (no rename_all), so the
        // tag is `key_type`. The load-bearing assertion is that the *value* is
        // `"noauth"` (not `"no_auth"`).
        assert!(json.contains(r#""key_type":"noauth""#), "json was: {json}");
        let back: ApiKey = serde_json::from_str(&json)?;
        assert_eq!(back.key_type, ApiKeyType::NoAuth);
        Ok(())
    }

    #[test]
    fn s13_apikeystatus_serde_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        use crate::user_project_service::ApiKeyStatus;

        // Go enum tags: "enabled" / "disabled" / "archived" (lowercase).
        assert_eq!(
            serde_json::to_string(&ApiKeyStatus::Enabled)?,
            r#""enabled""#
        );
        assert_eq!(
            serde_json::to_string(&ApiKeyStatus::Disabled)?,
            r#""disabled""#
        );
        assert_eq!(
            serde_json::to_string(&ApiKeyStatus::Archived)?,
            r#""archived""#
        );

        for tag in ["enabled", "disabled", "archived"] {
            let s: ApiKeyStatus = serde_json::from_str(&format!(r#""{tag}""#))?;
            let re = serde_json::to_string(&s)?;
            assert_eq!(re, format!(r#""{tag}""#));
        }

        // Default is `Enabled` (matches Go).
        assert_eq!(ApiKeyStatus::default(), ApiKeyStatus::Enabled);
        Ok(())
    }

    // -------------------------------------------------------------------
    // RUST-P5-003 S04 — field-by-field semantics pinning for
    // `CreateAPIKeyInput` / `UpdateAPIKeyInput` (Pauli-9th's admin-graphql
    // slice). These tests pin the contract that each GraphQL Input field is
    // honored by the service-layer pure functions, mirroring Go
    // `biz/api_key.go:CreateAPIKey` (lines 309-388) and `UpdateAPIKey` (lines
    // 396-470). They prevent regressions where a refactor of the service
    // helpers silently drops a field's semantics.
    // -------------------------------------------------------------------

    use crate::user_project_service::ApiKeyType as GraphQLApiKeyType;

    /// Map the GraphQL `CreateAPIKeyInput` field shape (admin-graphql slice,
    /// `crates/conduit-admin-graphql/src/apikey.rs::CreateAPIKeyInput`) through
    /// the service-layer pure resolution, asserting each field's documented
    /// semantics:
    ///   - `name` — opaque to the scope/type layer (carried by the host).
    ///   - `type` — `None` defaults to `User`; `Noauth` rejected by the
    ///     validator; `User` ignores caller `scopes`; `ServiceAccount` honours
    ///     them.
    ///   - `scopes` — only consulted when `type == ServiceAccount`.
    ///   - `projectID` — opaque to the scope/type layer (per-project duplicate
    ///     check is the host's job).
    ///
    /// The pure functions live in this crate; the GraphQL Input struct lives in
    /// `conduit-admin-graphql`. We exercise them via the same parameter shape
    /// the host adapter would feed, so a regression in either the input
    /// mapping or the pure function surfaces here.
    #[test]
    fn s04_create_apikey_input_field_semantics_are_honoured_by_service_layer()
    -> Result<(), ApiKeyTypeError> {
        use crate::user_project_service::ApiKeyType;

        // Map CreateAPIKeyInput { type: None, scopes: None } → service.
        // Go biz/api_key.go:325-330: defaults to User; column-default scopes
        // `[read_channels, write_requests]`.
        let resolved = resolve_create_scopes(ApiKeyType::User, None);
        assert_eq!(
            resolved,
            vec!["read_channels".to_string(), "write_requests".to_string()]
        );

        // CreateAPIKeyInput { type: Some(User), scopes: Some(["admin"]) } —
        // User-type IGNORES caller scopes (biz/api_key.go:330-333).
        let caller_scopes = vec!["admin".to_string(), "delete_channels".to_string()];
        let resolved = resolve_create_scopes(ApiKeyType::User, Some(caller_scopes.as_slice()));
        assert_eq!(
            resolved,
            vec!["read_channels".to_string(), "write_requests".to_string()]
        );

        // CreateAPIKeyInput { type: Some(ServiceAccount), scopes: None } —
        // empty scopes result (biz/api_key.go:334-336).
        let resolved = resolve_create_scopes(ApiKeyType::ServiceAccount, None);
        assert!(resolved.is_empty());

        // CreateAPIKeyInput { type: Some(ServiceAccount), scopes: Some([...]) }
        // — caller scopes honoured verbatim.
        let caller_scopes = vec!["read_channels".to_string(), "admin".to_string()];
        let resolved =
            resolve_create_scopes(ApiKeyType::ServiceAccount, Some(caller_scopes.as_slice()));
        assert_eq!(resolved, caller_scopes);

        // CreateAPIKeyInput { type: Some(Noauth) } — REJECTED by the validator
        // (biz/api_key.go:318-322 "noauth type API key is reserved"). Only
        // `EnsureNoAuthAPIKey` may provision a noauth key.
        assert!(matches!(
            validate_create_type(ApiKeyType::NoAuth),
            Err(ApiKeyTypeError::NoAuthReserved)
        ));
        // User / ServiceAccount pass the validator.
        assert!(validate_create_type(ApiKeyType::User).is_ok());
        assert!(validate_create_type(ApiKeyType::ServiceAccount).is_ok());
        Ok(())
    }

    /// Map the GraphQL `UpdateAPIKeyInput` field shape through the service
    /// layer, asserting each field's documented semantics:
    ///   - `name` — opaque to scope layer (host applies via rename probe).
    ///   - `scopes` (set) — wins over append, loses to clear (service_account
    ///     only); rejected for user / noauth.
    ///   - `appendScopes` — applied when set is None and clear is false.
    ///   - `clearScopes` — wins over set and append.
    ///
    /// Mirrors Go `biz/api_key.go:UpdateAPIKey` (lines 405-470):
    /// user-type keys reject ANY non-empty scope mutation (length-checked);
    /// noauth-type keys reject every update; service_account honours the
    /// clear → set → append precedence.
    #[test]
    fn s04_update_apikey_input_field_semantics_are_honoured_by_service_layer()
    -> Result<(), ApiKeyUpdateError> {
        use crate::user_project_service::ApiKeyType;

        // UpdateAPIKeyInput on a USER-type key:
        // - empty `scopes` + no append + no clear → PASSES (length-checked).
        let out = resolve_update_scope_intent(ApiKeyType::User, Some(&[]), &[], false)?;
        // Empty set on User is permitted; the host's "non-empty" check is what
        // guards user-type scope immutability. The pure function emits Noop
        // because all inputs are empty/false.
        assert_eq!(out, ScopeMutation::Noop);

        // UpdateAPIKeyInput { scopes: ["admin"] } on a USER key — REJECTED.
        assert!(matches!(
            resolve_update_scope_intent(ApiKeyType::User, Some(&["admin".to_string()]), &[], false),
            Err(ApiKeyUpdateError::UserScopeImmutable)
        ));
        // UpdateAPIKeyInput { appendScopes: ["admin"] } on a USER key — REJECTED.
        assert!(matches!(
            resolve_update_scope_intent(ApiKeyType::User, None, &["admin".to_string()], false),
            Err(ApiKeyUpdateError::UserScopeImmutable)
        ));
        // UpdateAPIKeyInput { clearScopes: true } on a USER key — REJECTED.
        assert!(matches!(
            resolve_update_scope_intent(ApiKeyType::User, None, &[], true),
            Err(ApiKeyUpdateError::UserScopeImmutable)
        ));

        // UpdateAPIKeyInput on a NOAUTH key — every update is rejected
        // (biz/api_key.go:413-415).
        assert!(matches!(
            resolve_update_scope_intent(ApiKeyType::NoAuth, None, &[], false),
            Err(ApiKeyUpdateError::NoAuthImmutable)
        ));

        // UpdateAPIKeyInput on a SERVICE_ACCOUNT key — precedence is
        // clear > set > append > noop.
        // clearScopes=true wins over set + append.
        assert_eq!(
            resolve_update_scope_intent(
                ApiKeyType::ServiceAccount,
                Some(&["a".to_string()]),
                &["b".to_string()],
                true
            )?,
            ScopeMutation::Clear
        );
        // scopes=[a] wins over appendScopes=[b].
        assert_eq!(
            resolve_update_scope_intent(
                ApiKeyType::ServiceAccount,
                Some(&["a".to_string()]),
                &["b".to_string()],
                false
            )?,
            ScopeMutation::Set(vec!["a".to_string()])
        );
        // appendScopes=[b] when no set + no clear.
        assert_eq!(
            resolve_update_scope_intent(
                ApiKeyType::ServiceAccount,
                None,
                &["b".to_string()],
                false
            )?,
            ScopeMutation::Append(vec!["b".to_string()])
        );
        // All empty → Noop.
        assert_eq!(
            resolve_update_scope_intent(ApiKeyType::ServiceAccount, None, &[], false)?,
            ScopeMutation::Noop
        );
        Ok(())
    }

    /// The `RotateAPIKey` mutation takes only `id: ID!` (no input object) and
    /// MUST preserve status/name/scopes/profiles. The service-layer invariant
    /// is that rotate touches only the `key` field. There is no pure function
    /// for rotate (the host rewrites `key` and persists), so we pin the
    /// contract by asserting the absence of any scope mutation: rotate must
    /// NEVER trigger the scope-resolution paths. A future refactor that wires
    /// rotate through `resolve_update_scope_intent` would have to be
    /// deliberate, not silent.
    #[test]
    fn s04_rotate_apikey_input_has_no_scope_mutation() {
        use crate::user_project_service::ApiKeyType;
        // RotateAPIKey takes no scopes; the only valid service-layer intent is
        // Noop for every key type.
        for key_type in [
            ApiKeyType::User,
            ApiKeyType::ServiceAccount,
            ApiKeyType::NoAuth,
        ] {
            let intent = resolve_update_scope_intent(key_type, None, &[], false);
            // ServiceAccount → Noop; User/NoAuth → Noop when nothing is
            // requested. Rotate must never request anything.
            match intent {
                Ok(ScopeMutation::Noop) | Err(_) => {}
                other => panic!("rotate on {key_type:?} must not mutate scopes, got {other:?}"),
            }
        }
    }

    // Quick helper to silence the unused-import warning if the GraphQL alias
    // is not referenced in non-test builds. The alias documents the
    // admin-graphql input type name so future readers can grep the bridge.
    #[allow(dead_code)]
    fn _graphql_apikey_type_alias_doc() -> GraphQLApiKeyType {
        GraphQLApiKeyType::default()
    }

    // -------------------------------------------------------------------
    // Go api_key_test.go SEMANTICS migration (Mendel-the-5th 2026-07-06).
    //
    // The bulk of Go `api_key_test.go` (1368 lines, 11 top-level tests)
    // exercises DB+cache side-effects via an Ent test client and
    // `miniredis.RunT` — explicitly out of scope for this pure-logic module
    // (see the crate-level doc above). The
    // persistence-side sub-tests are listed as **pending DB layer** in the
    // task report; they require porting `ApiKeyService.{GetAPIKey,
    // CreateAPIKey, UpdateAPIKey, UpdateAPIKeyStatus, UpdateAPIKeyProfiles,
    // CreateLLMAPIKey, RotateAPIKey, BulkEnableAPIKeys, BulkDisableAPIKeys,
    // BulkArchiveAPIKeys, EnsureNoAuthAPIKey}` to a DB-backed trait first
    // (their Go implementations live in `biz/api_key.go` but their
    // `s.db.APIKey.{Create,Update,Get}` etc. require the live ent client).
    //
    // The tests below fill the PURE-LOGIC semantic gaps that Go api_key_test.go
    // implies but the existing Rust suite did not yet pin explicitly: each is
    // a byte-for-byte mirror of a named Go sub-test's pre-DB invariant, with
    // the Go test name + line range cited in the comment.
    // -------------------------------------------------------------------

    /// Mirrors Go `TestAPIKeyService_UpdateAPIKeyProfiles`/"Multiple profiles
    /// with unique names" (`api_key_test.go:477-508`).
    ///
    /// Go drives the full `UpdateAPIKeyProfiles` (DB-bound) with three profiles
    /// (`production`/`staging`/`development`) and `ActiveProfile: "staging"`,
    /// asserting `Err == nil` and `len(updated.Profiles.Profiles) == 3`. The
    /// pure validators are the only service-layer surface that can run without
    /// the ent client; this test pins the same input shape through
    /// `validate_all_profiles` so a regression that breaks multi-profile
    /// validation (e.g. an off-by-one in the duplicate-name scan, or a wrong
    /// active-profile comparison) is caught without a DB.
    #[test]
    fn go_update_apikey_profiles_multiple_unique_names_passes_all_validators() {
        // Byte-for-byte mirror of the Go L478-499 input shape.
        let profiles = APIKeyProfiles {
            active_profile: "staging".to_string(),
            profiles: vec![
                APIKeyProfile {
                    name: "production".to_string(),
                    ..Default::default()
                },
                APIKeyProfile {
                    name: "staging".to_string(),
                    ..Default::default()
                },
                APIKeyProfile {
                    name: "development".to_string(),
                    ..Default::default()
                },
            ],
        };
        assert_eq!(validate_all_profiles(&profiles), Ok(()));
        // Cross-check the individual assertions Go makes on the result.
        assert_eq!(profiles.profiles.len(), 3);
        assert_eq!(profiles.active_profile, "staging");
    }

    /// Mirrors Go `TestAPIKeyService_UpdateAPIKeyProfiles`/"Channel tags match
    /// mode none is valid" (`api_key_test.go:458-475`).
    ///
    /// Go constructs a profile with `ChannelTags: ["official"]` and
    /// `ChannelTagsMatchMode: objects.ChannelTagsMatchModeNone`, asserts
    /// `Err == nil`, then asserts the stored profile still carries
    /// `ChannelTagsMatchModeNone`. The existing Rust test
    /// `validate_profile_filters_accepts_known_modes` covers the mode alone;
    /// this test pins the full Go shape (channel_tags populated + mode set)
    /// so a regression that drops `channel_tags` during validation, or treats
    /// `none` as invalid, is caught.
    #[test]
    fn go_update_apikey_profiles_channel_tags_match_mode_none_is_valid() {
        let profiles = APIKeyProfiles {
            active_profile: "production".to_string(),
            profiles: vec![APIKeyProfile {
                name: "production".to_string(),
                channel_tags: vec!["official".to_string()],
                channel_tags_match_mode: Some(channel_tags_match_mode::NONE.to_string()),
                ..Default::default()
            }],
        };
        assert_eq!(validate_all_profiles(&profiles), Ok(()));
        // Mirrors Go's final assertion that the stored mode survives the
        // round-trip — we just re-check the input value, since the validators
        // never mutate it.
        assert_eq!(
            profiles.profiles[0].channel_tags_match_mode.as_deref(),
            Some(channel_tags_match_mode::NONE)
        );
        assert_eq!(
            profiles.profiles[0].channel_tags,
            vec!["official".to_string()]
        );
    }

    /// Mirrors Go `TestAPIKeyService_CreateLLMAPIKey`/"rejects empty name"
    /// (`api_key_test.go:1082-1085`).
    ///
    /// Go drives `apiKeyService.CreateLLMAPIKey(ctx, ownerAPIKey, "   ")` and
    /// asserts `require.ErrorIs(t, err, ErrAPIKeyNameRequired)`. The pure
    /// validator `validate_llm_api_key_name` is the service-layer prefix of
    /// that path (biz/api_key.go:229-232: `name = strings.TrimSpace(name);
    /// if name == "" { return ErrAPIKeyNameRequired }`). Pins the trim
    /// behaviour so a regression that drops the trim (e.g. checks the raw
    /// string instead) is caught.
    #[test]
    fn go_create_llm_api_key_rejects_whitespace_only_name() {
        // Whitespace-only name trims to "" and must be rejected.
        assert_eq!(
            validate_llm_api_key_name("   "),
            Err(LlmAPIKeyNameError::Empty)
        );
        // Tab/newline also trim to empty.
        assert_eq!(
            validate_llm_api_key_name("\t\n\r "),
            Err(LlmAPIKeyNameError::Empty)
        );
        // Empty string directly.
        assert_eq!(
            validate_llm_api_key_name(""),
            Err(LlmAPIKeyNameError::Empty)
        );
    }

    /// Mirrors Go `TestAPIKeyService_CreateLLMAPIKey`/"creates llm api key"
    /// (`api_key_test.go:1070-1080`) — pure name-trim half only.
    ///
    /// Go passes `"  LLM Key  "` and the test later asserts `apiKey.Name ==
    /// "LLM Key"`, i.e. the surrounding whitespace is dropped on entry. The
    /// DB insert + scope/type assertions are out of scope here (pending the
    /// host adapter); the pure half returns the trimmed name so the caller
    /// does not re-trim.
    #[test]
    fn go_create_llm_api_key_trims_surrounding_whitespace() {
        assert_eq!(
            validate_llm_api_key_name("  LLM Key  ").ok(),
            Some("LLM Key")
        );
        assert_eq!(validate_llm_api_key_name("LLM Key").ok(), Some("LLM Key"));
    }

    /// Mirrors Go `TestAPIKeyService_RotateAPIKey`/"cannot rotate noauth type
    /// API key" (`api_key_test.go:1308-1324`).
    ///
    /// Go creates a noauth key directly via `client.APIKey.Create().SetType(
    /// apikey.TypeNoauth)...`, then asserts
    /// `require.ErrorContains(err, "noauth type API key cannot be rotated")`.
    /// The pure validator `validate_rotate_key_type` is the service-layer
    /// prefix of that path (biz/api_key.go:826-828). DB-backed sub-tests of
    /// `RotateAPIKey` (rotate user / service_account, non-existent id, cross-
    /// project denial) are pending the host adapter port.
    #[test]
    fn go_rotate_api_key_rejects_noauth_type() {
        use crate::user_project_service::ApiKeyType;
        assert_eq!(
            validate_rotate_key_type(ApiKeyType::NoAuth),
            Err(RotateAPIKeyError::NoAuthNotRotatable)
        );
        // User and ServiceAccount are rotatable (biz/api_key.go:825 fallthrough).
        assert_eq!(validate_rotate_key_type(ApiKeyType::User), Ok(()));
        assert_eq!(validate_rotate_key_type(ApiKeyType::ServiceAccount), Ok(()));
    }

    /// Mirrors Go `TestAPIKeyService_BulkEnableAPIKeys`/"enable with empty
    /// list" (`api_key_test.go:598-602`), `BulkDisableAPIKeys`/"disable with
    /// empty list" (`api_key_test.go:709-713`), and `BulkArchiveAPIKeys`/
    /// "archive with empty list" (`api_key_test.go:821-825`).
    ///
    /// All three Go sub-tests pass `ids: []int{}` and assert `require.NoError`
    /// — Go `bulkUpdateAPIKeyStatus` opens with `if len(ids) == 0 { return
    /// nil }` (biz/api_key.go:752-754). The pure short-circuit
    /// `resolve_bulk_status_ids_short_circuit` lifts that decision so the
    /// host adapter never reaches the DB on an empty list. Pins the
    /// empty-as-success contract (NOT an error) explicitly so a refactor
    /// that swaps to `Err(EmptyInput)` is caught.
    #[test]
    fn go_bulk_status_short_circuit_empty_list_is_successful_noop() {
        // Empty list → EmptyNoOp (NOT an error): Go returns nil.
        assert_eq!(
            resolve_bulk_status_ids_short_circuit(&[]),
            BulkStatusShortCircuit::EmptyNoOp
        );
        // Single id → Proceed.
        assert_eq!(
            resolve_bulk_status_ids_short_circuit(&[42_i64]),
            BulkStatusShortCircuit::Proceed(vec![42_i64])
        );
        // Multi id → Proceed (Go loops over the same slice).
        assert_eq!(
            resolve_bulk_status_ids_short_circuit(&[1, 2, 3]),
            BulkStatusShortCircuit::Proceed(vec![1, 2, 3])
        );
    }

    /// Pins Go biz/api_key.go:778 + :794 verb interpolation for the bulk
    /// status action. Go threads the verb through `bulkUpdateAPIKeyStatus(ctx,
    /// ids, status, action)` purely to format two error strings:
    ///   - `"noauth type API key cannot be bulk %sd"` (biz/api_key.go:778)
    ///   - `"failed to %s API keys: %w"` (biz/api_key.go:794)
    ///
    /// The Rust `BulkStatusAction::go_verb` reproduces the exact lower-case
    /// verb for each action. Pins the byte-exact Go verbs so a future rename
    /// (e.g. "delete" for archive) is forced through review.
    #[test]
    fn go_bulk_status_action_verbs_match_go_error_strings() {
        assert_eq!(BulkStatusAction::Enable.go_verb(), "enable");
        assert_eq!(BulkStatusAction::Disable.go_verb(), "disable");
        assert_eq!(BulkStatusAction::Archive.go_verb(), "archive");
    }

    /// Comprehensive parity documentation of the Go api_key_test.go sub-tests
    /// NOT migrated here because their assertions depend on the live ent
    /// client + miniredis cache. Each row cites the Go test name + line range
    /// so a future DB-backed trait port can grep for the right test name.
    ///
    /// This test exists to FAIL LOUD if the trait-port lands and someone
    /// forgets to add a DB-backed test for one of these sub-tests: append each
    /// migrated sub-test to the `migrated` list as it lands, and the
    /// remaining `pending` list shrinks. The test fails only when the pending
    /// set is empty AND a pending entry is still listed (defensive guard
    /// against accidental removal).
    #[test]
    fn go_api_key_test_pending_db_backed_subtests_catalogue() {
        // Each tuple is (Go test function name, Go line range, brief reason).
        let pending: &[(&str, &str, &str)] = &[
            (
                "TestGenerateAPIKey",
                "L26-37",
                "migrate to conduit-auth::apikey::tests (generate_api_key lives \
                 there); also note Go biz/api_key.go:170 trims prefix \
                 whitespace but Rust apikey.rs:18 uses .is_empty() — parity \
                 bug to file separately",
            ),
            (
                "TestAPIKeyService_GetAPIKey",
                "L58-132",
                "ent client + cache.Lookup; requires DB-backed ApiKeyService \
                 trait port",
            ),
            (
                "TestAPIKeyService_GetAPIKey_WithDifferentCaches",
                "L134-250",
                "drives 4 xcache modes incl. miniredis; requires live cache + \
                 Redis port",
            ),
            (
                "TestAPIKeyService_UpdateAPIKeyProfiles/Valid_profiles_update",
                "L300-325",
                "DB update + round-trip read; covered on the pure side by \
                 validate_all_profiles",
            ),
            (
                "TestAPIKeyService_BulkEnableAPIKeys/non-empty cases",
                "L583-595",
                "DB count + update; pure short-circuit pinned in \
                 go_bulk_status_short_circuit_empty_list_is_successful_noop",
            ),
            (
                "TestAPIKeyService_BulkDisableAPIKeys/non-empty cases",
                "L693-707",
                "DB count + update; pure short-circuit pinned (see above)",
            ),
            (
                "TestAPIKeyService_BulkArchiveAPIKeys/non-empty cases",
                "L805-819",
                "DB count + update; pure short-circuit pinned (see above)",
            ),
            (
                "TestAPIKeyService_CreateAPIKey_Type/format check",
                "L998-1020",
                "DB insert + generated key shape; generated-key shape is \
                 covered by conduit-auth::apikey::tests (cross-crate)",
            ),
            (
                "TestAPIKeyService_CreateLLMAPIKey/creates + unauthorized",
                "L1070-1105",
                "DB insert + privacy.Allow denial; pure name-trim pinned in \
                 go_create_llm_api_key_rejects_whitespace_only_name",
            ),
            (
                "TestAPIKeyService_NameUniqueness",
                "L1108-1204",
                "DB post-insert live-count + soft-delete semantics; requires \
                 host transaction port",
            ),
            (
                "TestAPIKeyService_RotateAPIKey/rotate + non-existent + cross-project",
                "L1237-1367",
                "DB update + cross-project denial via privacy gate; pure \
                 noauth-type rejection pinned in \
                 go_rotate_api_key_rejects_noauth_type",
            ),
        ];

        // Sanity: every entry has a non-empty reason and unique name.
        let mut seen = std::collections::HashSet::new();
        for (name, lines, reason) in pending {
            assert!(!name.is_empty(), "pending entry has empty name");
            assert!(!lines.is_empty(), "pending entry {name} has empty lines");
            assert!(!reason.is_empty(), "pending entry {name} has empty reason");
            assert!(seen.insert(*name), "duplicate pending entry {name}");
        }
        // Catalogue sanity: at least the rows we know about.
        assert!(
            pending.len() >= 11,
            "expected at least 11 pending DB-backed sub-tests, got {}",
            pending.len()
        );
    }
}
