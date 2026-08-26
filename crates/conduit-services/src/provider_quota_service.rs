use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use conduit_db::RequestContext;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

pub const PROVIDER_QUOTA_STATUS_READY: &str = "ready";
pub const PROVIDER_QUOTA_STATUS_EXHAUSTED: &str = "exhausted";
pub const PROVIDER_QUOTA_STATUS_WARNING: &str = "warning";
pub const PROVIDER_QUOTA_STATUS_UNKNOWN: &str = "unknown";
pub const PROVIDER_QUOTA_CACHE_INVALIDATION_SCOPE: &str = "candidate_selector";

/// Default quota check interval mirroring Go `ProviderQuotaService.getCheckInterval()`
/// fallback (`5 * time.Minute`).
pub const PROVIDER_QUOTA_DEFAULT_CHECK_INTERVAL_SECS: u64 = 5 * 60;

/// Default warning-check interval ratio mirroring Go
/// `ProviderQuotaService.warningCheckIntervalRatio` fallback (`4`).
pub const PROVIDER_QUOTA_DEFAULT_WARNING_CHECK_INTERVAL_RATIO: u32 = 4;

/// S05 — Warning threshold ratio at which a channel's usage flips its quota
/// status to `"warning"`. Mirrors Go
/// `provider_quota.WarningThresholdRatio` (`provider_quota/types.go:48`).
///
/// Most Go checkers compare `usage_ratio >= WarningThresholdRatio`
/// (e.g. `nanogpt_checker.go:258` `>= 0.8`, `codex` 80% utilization); a few use
/// strict `>` (`apertis_checker.go:239`). The pure helper
/// [`status_for_usage_ratio`] uses `>=` because that is the dominant Go
/// convention and matches `WarningThresholdRatio`'s docstring intent.
pub const PROVIDER_QUOTA_WARNING_THRESHOLD_RATIO: f64 = 0.8;

// ───────────────────────────────────────────────────────────────────────────
// S12 — Independent quota-checker HTTP timeout
// ───────────────────────────────────────────────────────────────────────────
//
// Go currently routes the checker's HTTP call through the shared
// `httpclient.HttpClient` (see `provider_quota/claudecode_checker.go:75-80`),
// so it inherits the LLM request budget. RUST-P13-005 S12 requires the quota
// checker to use its OWN timeout, distinct from the normal LLM request
// timeout, so a slow quota endpoint cannot eat the LLM budget. Because the Go
// source has no separate const to mirror, we expose a clearly-named,
// conservative default here. The intent is that callers wiring the future
// HTTP-bound checker pass this duration into the per-request context (e.g.
// `tokio::time::timeout`), NOT the shared LLM client timeout.
/// Default HTTP timeout for a single quota-checker outbound call. Picked to be
/// short relative to a typical LLM streaming budget so a stalled quota
/// endpoint fails fast without blocking user-facing requests.
pub const PROVIDER_QUOTA_CHECKER_HTTP_TIMEOUT_SECS: u64 = 10;

/// Convenience `Duration` form of [`PROVIDER_QUOTA_CHECKER_HTTP_TIMEOUT_SECS`].
pub fn provider_quota_checker_http_timeout() -> Duration {
    Duration::from_secs(PROVIDER_QUOTA_CHECKER_HTTP_TIMEOUT_SECS)
}

pub type ProviderQuotaServiceResult<T> = Result<T, ProviderQuotaServiceError>;

const PROVIDER_QUOTA_SETTING_URL_KEYS: &[&str] = &[
    "provider_quota_check_url",
    "provider_quota_url",
    "quota_check_url",
    "quota_url",
];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProviderQuotaServiceError {
    #[error("provider quota persistence lock poisoned")]
    LockPoisoned,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderQuotaStatus {
    pub channel_id: String,
    pub provider_type: String,
    pub status: String,
    pub quota_data: Value,
    pub next_reset_at: Option<DateTime<Utc>>,
    pub ready: bool,
    pub next_check_at: Option<DateTime<Utc>>,
}

impl ProviderQuotaStatus {
    pub fn new(channel_id: impl Into<String>, provider_type: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            provider_type: provider_type.into(),
            status: PROVIDER_QUOTA_STATUS_READY.to_string(),
            quota_data: Value::Object(Default::default()),
            next_reset_at: None,
            ready: true,
            next_check_at: None,
        }
    }

    pub fn exhausted(channel_id: impl Into<String>, provider_type: impl Into<String>) -> Self {
        Self {
            status: PROVIDER_QUOTA_STATUS_EXHAUSTED.to_string(),
            ready: false,
            ..Self::new(channel_id, provider_type)
        }
    }

    pub fn is_exhausted(&self) -> bool {
        self.status
            .eq_ignore_ascii_case(PROVIDER_QUOTA_STATUS_EXHAUSTED)
    }

    pub fn reset(mut self) -> Self {
        self.status = PROVIDER_QUOTA_STATUS_READY.to_string();
        self.ready = true;
        self.quota_data = Value::Object(Default::default());
        self.next_reset_at = None;
        self.next_check_at = None;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderQuotaCacheInvalidation {
    pub scope: String,
    pub channel_id: String,
    pub provider_type: String,
    pub key: String,
}

impl ProviderQuotaCacheInvalidation {
    pub fn new(channel_id: impl Into<String>, provider_type: impl AsRef<str>) -> Self {
        let channel_id = channel_id.into();
        let provider_type = normalize_provider_type(provider_type.as_ref());
        let key = provider_quota_cache_invalidation_key(&channel_id, &provider_type);

        Self {
            scope: PROVIDER_QUOTA_CACHE_INVALIDATION_SCOPE.to_string(),
            channel_id,
            provider_type,
            key,
        }
    }

    pub fn for_status(status: &ProviderQuotaStatus) -> Self {
        Self::new(status.channel_id.clone(), &status.provider_type)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderQuotaEnforcementMode {
    ExhaustedOnly,
    DePrioritize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQuotaCheckTarget {
    pub provider_type: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy)]
struct ProviderQuotaCheckTargetTemplate {
    provider_type: &'static str,
    default_base_url: &'static str,
    default_path: &'static str,
}

pub fn detect_provider_quota_check_target(
    provider_type: impl AsRef<str>,
    base_url: Option<&str>,
    settings: Option<&Value>,
) -> Option<ProviderQuotaCheckTarget> {
    let template = provider_quota_check_target_template(provider_type.as_ref())?;
    let configured_url = settings.and_then(provider_quota_url_from_settings);
    let url = match configured_url {
        Some(configured_url) if is_absolute_url(configured_url) => configured_url.to_string(),
        Some(configured_path) => join_url(
            base_url.unwrap_or(template.default_base_url),
            configured_path,
        ),
        None => join_url(
            base_url.unwrap_or(template.default_base_url),
            template.default_path,
        ),
    };

    Some(ProviderQuotaCheckTarget {
        provider_type: template.provider_type.to_string(),
        url,
    })
}

fn provider_quota_check_target_template(
    provider_type: &str,
) -> Option<ProviderQuotaCheckTargetTemplate> {
    match normalize_provider_type(provider_type).as_str() {
        "apertis" => Some(ProviderQuotaCheckTargetTemplate {
            provider_type: "apertis",
            default_base_url: "https://api.apertis.ai",
            default_path: "/quota",
        }),
        "codex" => Some(ProviderQuotaCheckTargetTemplate {
            provider_type: "codex",
            default_base_url: "https://chatgpt.com",
            default_path: "/backend-api/codex/quota",
        }),
        "nanogpt" | "nanogpt_responses" => Some(ProviderQuotaCheckTargetTemplate {
            provider_type: "nanogpt",
            default_base_url: "https://nano-gpt.com",
            default_path: "/api/subscription/v1/usage",
        }),
        "synthetic" => Some(ProviderQuotaCheckTargetTemplate {
            provider_type: "synthetic",
            default_base_url: "http://127.0.0.1",
            default_path: "/quota",
        }),
        _ => None,
    }
}

fn provider_quota_url_from_settings(settings: &Value) -> Option<&str> {
    for key in PROVIDER_QUOTA_SETTING_URL_KEYS {
        if let Some(url) = settings.get(*key).and_then(Value::as_str).map(str::trim)
            && !url.is_empty()
        {
            return Some(url);
        }
    }

    let provider_quota = settings.get("provider_quota")?;
    for key in PROVIDER_QUOTA_SETTING_URL_KEYS {
        if let Some(url) = provider_quota
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            && !url.is_empty()
        {
            return Some(url);
        }
    }

    None
}

fn normalize_provider_type(provider_type: &str) -> String {
    provider_type.trim().to_ascii_lowercase().replace('-', "_")
}

fn provider_quota_cache_invalidation_key(channel_id: &str, provider_type: &str) -> String {
    format!(
        "candidate_selector:provider_quota:{}:{}",
        channel_id.trim_matches(':'),
        normalize_provider_type(provider_type)
    )
}

fn join_url(base_url: &str, path: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    let path = path.trim();
    if path.starts_with('/') {
        format!("{base_url}{path}")
    } else {
        format!("{base_url}/{path}")
    }
}

fn is_absolute_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

// ───────────────────────────────────────────────────────────────────────────
// S09 — Provider checker registry (pure)
// ───────────────────────────────────────────────────────────────────────────

/// Kinds of provider quota checkers registered with the service.
///
/// Mirrors the Go checker registry populated by
/// `ProviderQuotaService.NewProviderQuotaService` via the
/// `register<Provider>Support()` methods in
/// `conduit/internal/server/biz/provider_quota.go` (lines 280-287, 304-334).
/// Pattern A (dedicated channel type): `ClaudeCode`, `Codex`, `GithubCopilot`,
/// `NanoGpt`, `Apertis`. Pattern B (URL-detected on the OpenAI channel type):
/// `Wafer`, `Synthetic`, `NeuralWatt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCheckerKind {
    Apertis,
    Codex,
    NanoGpt,
    NeuralWatt,
    Wafer,
    ClaudeCode,
    GithubCopilot,
    Synthetic,
}

impl ProviderCheckerKind {
    /// Stable provider_type string used as the Go `svc.checkers[providerType]` key.
    /// Mirrors the literals passed to the `register<Provider>Support()` methods
    /// (`provider_quota.go:304-334`).
    pub fn as_provider_type(self) -> &'static str {
        match self {
            ProviderCheckerKind::Apertis => "apertis",
            ProviderCheckerKind::Codex => "codex",
            ProviderCheckerKind::NanoGpt => "nanogpt",
            ProviderCheckerKind::NeuralWatt => "neuralwatt",
            ProviderCheckerKind::Wafer => "wafer",
            ProviderCheckerKind::ClaudeCode => "claudecode",
            ProviderCheckerKind::GithubCopilot => "github_copilot",
            ProviderCheckerKind::Synthetic => "synthetic",
        }
    }

    /// Iterate over every registered checker kind in registration order
    /// (mirrors `NewProviderQuotaService` order: claudecode, codex,
    /// github_copilot, nanogpt, wafer, synthetic, neuralwatt, apertis).
    pub const fn all() -> &'static [ProviderCheckerKind] {
        &[
            ProviderCheckerKind::ClaudeCode,
            ProviderCheckerKind::Codex,
            ProviderCheckerKind::GithubCopilot,
            ProviderCheckerKind::NanoGpt,
            ProviderCheckerKind::Wafer,
            ProviderCheckerKind::Synthetic,
            ProviderCheckerKind::NeuralWatt,
            ProviderCheckerKind::Apertis,
        ]
    }
}

/// Returns the checker kind registered for a given channel type token, or
/// `None` if the channel type has no quota checker.
///
/// Mirrors Go `ProviderQuotaService.getProviderType(ch)` for the dedicated
/// (Pattern A) channel types (`provider_quota.go:690-705`):
///   - `claudecode`              -> claudecode
///   - `codex`                   -> codex
///   - `github_copilot`          -> github_copilot
///   - `nanogpt`/`nanogpt_responses` -> nanogpt
/// OpenAI-compatible channel types (`openai`/`openai_responses`) are resolved
/// via URL detection — see [`detect_quota_check_url`].
///
/// `channel_type` is matched case-insensitively after normalizing `-` to `_`,
/// consistent with [`normalize_provider_type`].
pub fn checker_for(channel_type: impl AsRef<str>) -> Option<ProviderCheckerKind> {
    match normalize_provider_type(channel_type.as_ref()).as_str() {
        "claudecode" => Some(ProviderCheckerKind::ClaudeCode),
        "codex" => Some(ProviderCheckerKind::Codex),
        "github_copilot" => Some(ProviderCheckerKind::GithubCopilot),
        "nanogpt" | "nanogpt_responses" => Some(ProviderCheckerKind::NanoGpt),
        // Pattern B providers resolve purely from the channel's base URL — they
        // have no dedicated channel type. Returning the kind here lets callers
        // that already know the provider (e.g. from URL detection) look up the
        // checker by kind without re-running detection.
        "apertis" => Some(ProviderCheckerKind::Apertis),
        "neuralwatt" => Some(ProviderCheckerKind::NeuralWatt),
        "wafer" => Some(ProviderCheckerKind::Wafer),
        "synthetic" => Some(ProviderCheckerKind::Synthetic),
        _ => None,
    }
}

/// Returns the checker kind registered for a URL-detected provider type
/// (Pattern B). Mirrors the inverse of Go
/// `provider_quota.URLDetectedProviders()` (`url_detection.go:21-28`).
pub fn checker_for_url_detected_provider(
    provider_type: impl AsRef<str>,
) -> Option<ProviderCheckerKind> {
    match normalize_provider_type(provider_type.as_ref()).as_str() {
        "wafer" => Some(ProviderCheckerKind::Wafer),
        "synthetic" => Some(ProviderCheckerKind::Synthetic),
        "neuralwatt" => Some(ProviderCheckerKind::NeuralWatt),
        "apertis" => Some(ProviderCheckerKind::Apertis),
        _ => None,
    }
}

/// Builder that resolves the URL a quota check should hit for a channel.
///
/// This is the pure-logic equivalent of the Go checker dispatch in
/// `ProviderQuotaService.checkChannelQuota` (`provider_quota.go:547-588`) —
/// given the channel type, optional base URL and optional settings JSON, it
/// returns the provider_type + target URL pair. The actual HTTP call is out of
/// scope for the pure slice and is left to the future HTTP-bound checker.
///
/// Settings-based URL overrides (the `provider_quota_check_url` /
/// `quota_check_url` / `provider_quota_url` / `quota_url` keys) are honored for
/// dedicated channel types that have a settings URL configured; otherwise the
/// per-provider default URL builder (mirroring Go's `build<Provider>QuotaURL`)
/// is used.
pub fn check_target_url(
    channel_type: impl AsRef<str>,
    base_url: Option<&str>,
    settings: Option<&Value>,
) -> Option<ProviderQuotaCheckTarget> {
    let normalized = normalize_provider_type(channel_type.as_ref());

    // Pattern B: OpenAI-compatible channel types resolve via URL detection,
    // then route through the per-provider URL builder (Go's
    // `build<Provider>QuotaURL` family) — exactly like
    // [`detect_quota_check_url`].
    if matches!(normalized.as_str(), "openai" | "openai_responses") {
        let base = base_url.unwrap_or_default();
        let detected = detect_provider_from_url(base)?;
        let url =
            detect_quota_check_url(&normalized, base_url, settings_url_as_endpoint(settings))?;
        return Some(ProviderQuotaCheckTarget {
            provider_type: detected,
            url,
        });
    }

    // Pattern A: dedicated channel types resolve directly.
    if let Some(kind) = checker_for(&normalized)
        && checker_kind_has_check_target(kind)
    {
        // Settings overrides win over the default builder for Pattern A
        // providers (mirrors the existing
        // `detect_provider_quota_check_target` precedence).
        return detect_provider_quota_check_target(kind.as_provider_type(), base_url, settings);
    }

    None
}

/// Extracts the configured endpoint override from a settings JSON blob, if any.
/// Used to bridge the S06/S07-style settings JSON into the
/// [`detect_quota_check_url`] `endpoint` parameter for Pattern B providers.
fn settings_url_as_endpoint(settings: Option<&Value>) -> Option<&str> {
    let settings = settings?;
    provider_quota_url_from_settings(settings)
}

/// Whether a checker kind exposes a static check-target template
/// (see [`provider_quota_check_target_template`]). The HTTP-only checkers
/// (`claudecode`, `github_copilot`) intentionally have no template here —
/// their URLs are not part of the pure registry slice.
const fn checker_kind_has_check_target(kind: ProviderCheckerKind) -> bool {
    matches!(
        kind,
        ProviderCheckerKind::Apertis
            | ProviderCheckerKind::Codex
            | ProviderCheckerKind::NanoGpt
            | ProviderCheckerKind::Synthetic
            | ProviderCheckerKind::NeuralWatt
            | ProviderCheckerKind::Wafer
    )
}

// ───────────────────────────────────────────────────────────────────────────
// S11 — URL detection (pure)
// ───────────────────────────────────────────────────────────────────────────

/// Host-suffix -> provider_type table. Mirrors Go `urlProviderMap`
/// (`url_detection.go:14-19`); more specific patterns first.
const URL_PROVIDER_MAP: &[(&str, &str)] = &[
    ("wafer.ai", "wafer"),
    ("api.synthetic.new", "synthetic"),
    ("api.neuralwatt.com", "neuralwatt"),
    ("api.apertis.ai", "apertis"),
];

/// Detects a Pattern-B provider from its base URL host.
///
/// Mirrors Go `provider_quota.DetectProviderFromURL` (`url_detection.go:31-55`)
/// including the empty/whitespace, malformed-URL and false-positive cases
/// exercised by `url_detection_test.go`. Returns `None` for unrecognized
/// hosts, matching Go's empty-string return.
pub fn detect_provider_from_url(base_url: &str) -> Option<String> {
    let base_url = base_url.trim();
    if base_url.is_empty() {
        return None;
    }

    let host = parse_url_host(base_url)?;

    for (pattern, provider_type) in URL_PROVIDER_MAP {
        let pattern = pattern.to_ascii_lowercase();
        if host == pattern || host.ends_with(&format!(".{pattern}")) {
            return Some((*provider_type).to_string());
        }
    }

    None
}

/// Returns the provider types that may be URL-detected on an OpenAI-compatible
/// channel. Mirrors Go `provider_quota.URLDetectedProviders()`.
pub fn url_detected_providers() -> &'static [&'static str] {
    URL_PROVIDER_MAP
        .iter()
        .map(|(_, p)| *p)
        .collect::<Vec<_>>()
        .leak()
}

/// Per-provider URL builder table. Mirrors Go's `build<Provider>QuotaURL`
/// functions — each entry captures the same three behaviors the Go builders
/// share:
///   - default base URL (used when `base_url` is empty/missing)
///   - default path (appended to the host)
///   - whether `http://` is upgraded to `https://`
///
/// Sources:
///   - wafer: `provider_quota/wafer_checker.go:193-210`
///     (`buildWaferQuotaURL`, default `https://pass.wafer.ai/v1/inference/quota`)
///   - synthetic: `provider_quota/synthetic_checker.go:165-182`
///     (`buildSyntheticQuotaURL`, default `https://api.synthetic.new/v2/quotas`,
///      upgrades http -> https)
///   - neuralwatt: `provider_quota/neuralwatt_checker.go:163-180`
///     (`buildNeuralWattQuotaURL`, default `https://api.neuralwatt.com/v1/quota`,
///      upgrades http -> https)
///   - apertis: `provider_quota/apertis_checker.go:162-180`
///     (`buildApertisQuotaURL`, default
///      `https://api.apertis.ai/v1/dashboard/billing/credits`)
///   - codex / nanogpt: derived from the existing
///     [`provider_quota_check_target_template`] table (codex hits
///     ChatGPT usage, nanogpt hits its subscription usage endpoint).
struct QuotaCheckUrlBuilder {
    default_base_url: &'static str,
    default_path: &'static str,
    upgrade_to_https: bool,
}

fn quota_check_url_builder(provider_type: &str) -> Option<QuotaCheckUrlBuilder> {
    match provider_type {
        "wafer" => Some(QuotaCheckUrlBuilder {
            default_base_url: "https://pass.wafer.ai",
            default_path: "/v1/inference/quota",
            upgrade_to_https: true,
        }),
        "synthetic" => Some(QuotaCheckUrlBuilder {
            default_base_url: "https://api.synthetic.new",
            default_path: "/v2/quotas",
            upgrade_to_https: true,
        }),
        "neuralwatt" => Some(QuotaCheckUrlBuilder {
            default_base_url: "https://api.neuralwatt.com",
            default_path: "/v1/quota",
            upgrade_to_https: true,
        }),
        "apertis" => Some(QuotaCheckUrlBuilder {
            default_base_url: "https://api.apertis.ai",
            default_path: "/v1/dashboard/billing/credits",
            upgrade_to_https: false,
        }),
        "codex" => Some(QuotaCheckUrlBuilder {
            default_base_url: "https://chatgpt.com",
            default_path: "/backend-api/codex/quota",
            upgrade_to_https: false,
        }),
        "nanogpt" => Some(QuotaCheckUrlBuilder {
            default_base_url: "https://nano-gpt.com",
            default_path: "/api/subscription/v1/usage",
            upgrade_to_https: true,
        }),
        _ => None,
    }
}

/// Detects the quota check target URL for a channel, mirroring the
/// URL-detection half of Go `ProviderQuotaService.checkChannelQuota`.
///
/// For dedicated channel types (`codex`, `nanogpt`, `nanogpt_responses`) the
/// channel type alone identifies the provider; for OpenAI-compatible types
/// (`openai`, `openai_responses`) the `base_url` host is consulted via
/// [`detect_provider_from_url`]. `endpoint` overrides the provider's default
/// check path (Go equivalent: a per-checker `provider_quota_check_url`
/// setting).
///
/// `claudecode` and `github_copilot` intentionally have no static URL builder
/// here — their endpoints are baked into the HTTP-bound checker
/// (`provider_quota/claudecode_checker.go`,
/// `provider_quota/github_copilot_checker.go`) and cannot be resolved from the
/// channel type alone. The pure slice returns `None` for them.
///
/// Returns `None` when the channel has no registered checker (Go equivalent:
/// `getProviderType` returns `""`).
pub fn detect_quota_check_url(
    channel_type: impl AsRef<str>,
    base_url: Option<&str>,
    endpoint: Option<&str>,
) -> Option<String> {
    let normalized = normalize_provider_type(channel_type.as_ref());

    let provider_type = if matches!(normalized.as_str(), "openai" | "openai_responses") {
        detect_provider_from_url(base_url.unwrap_or_default())?
    } else {
        checker_for(&normalized)?.as_provider_type().to_string()
    };

    let builder = quota_check_url_builder(&provider_type)?;
    let base = base_url.map(str::trim).unwrap_or_default();
    let effective_base: String = if base.is_empty() {
        builder.default_base_url.to_string()
    } else if builder.upgrade_to_https {
        upgrade_http_to_https(base)
    } else {
        base.to_string()
    };
    let path = endpoint
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(builder.default_path);

    Some(join_url(&effective_base, path))
}

/// Mirrors the Go builders' `if scheme == "http" { scheme = "https" }` step
/// (`synthetic_checker.go:176-179`, `neuralwatt_checker.go:174-177`,
/// `wafer_checker.go:205-207`). For inputs without a scheme this is a no-op —
/// the Go builders leave such inputs untouched and the caller is expected to
/// supply a base URL with a scheme.
fn upgrade_http_to_https(base: &str) -> String {
    base.strip_prefix("http://")
        .map(|rest| format!("https://{rest}"))
        .unwrap_or_else(|| base.to_string())
}

/// Best-effort URL host extraction mirroring Go `url.Parse(baseURL).Hostname()`.
///
/// Only the parts this module needs are implemented: scheme strip, userinfo
/// strip, port strip, lower-casing. Malformed inputs return `None`, matching
/// Go's `url.Parse` error / empty-host behavior exercised by
/// `url_detection_test.go::TestDetectProviderFromURL_Malformed`.
fn parse_url_host(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    // Strip scheme. Go's `url.Parse` requires a scheme for `Hostname()` to
    // resolve: inputs like `wafer.ai` (no scheme) are parsed as a Path, not a
    // Host, and `Hostname()` returns "". Mirror that.
    let after_scheme = raw
        .strip_prefix("http://")
        .or_else(|| raw.strip_prefix("https://"))?;

    // Strip path/query/fragment.
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];

    // Strip userinfo (`user:pass@host`).
    let host_with_port = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    if host_with_port.is_empty() {
        return None;
    }

    // Strip port. Go's `Hostname` strips a trailing `:port`. It does NOT
    // validate that what remains is a valid host — it just removes the last
    // `:N..` segment. `Hostname()` also leaves bracketed IPv6 hosts as-is
    // (without the brackets).
    let host = strip_port(host_with_port);
    if host.is_empty() {
        return None;
    }

    Some(host.to_ascii_lowercase())
}

/// Mirrors Go `url.URL.Hostname()` port-stripping (handles bracketed IPv6).
fn strip_port(host_with_port: &str) -> &str {
    if let Some(stripped) = host_with_port.strip_prefix('[')
        && let Some(end) = stripped.find(']')
    {
        return &stripped[..end];
    }
    match host_with_port.rfind(':') {
        Some(idx)
            if host_with_port[idx..]
                .chars()
                .nth(1)
                .is_none_or(|c| c != ':') =>
        {
            &host_with_port[..idx]
        }
        _ => host_with_port,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// S10 — Quota status row shape + due-for-check predicate (pure)
// ───────────────────────────────────────────────────────────────────────────

/// Provider-quota status row mirror, covering every field the Go
/// `provider_quota_status` schema reads/writes through
/// `ProviderQuotaService.saveQuotaStatus` (`provider_quota.go:590-629`):
/// `next_check_at`, `next_reset_at`, `ready`, `status`, `quota_data`.
///
/// This is intentionally a separate type from [`ProviderQuotaStatus`] (the
/// cache DTO): the row shape is what the scheduler consults when deciding
/// whether to poll a channel, whereas [`ProviderQuotaStatus`] is the
/// cache/invalidation descriptor already used by S06/S07.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderQuotaStatusRow {
    pub channel_id: String,
    pub provider_type: String,
    pub status: String,
    pub ready: bool,
    pub quota_data: Value,
    pub next_check_at: Option<DateTime<Utc>>,
    pub next_reset_at: Option<DateTime<Utc>>,
}

impl ProviderQuotaStatusRow {
    pub fn new(channel_id: impl Into<String>, provider_type: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            provider_type: provider_type.into(),
            status: PROVIDER_QUOTA_STATUS_READY.to_string(),
            ready: true,
            quota_data: Value::Object(Default::default()),
            next_check_at: None,
            next_reset_at: None,
        }
    }

    /// Mirrors Go's `providerquotastatus.Status` equality check used throughout
    /// `ProviderQuotaService` (e.g. `EffectiveStatus`, `nextCheckIntervalForStatus`).
    pub fn has_status(&self, status: &str) -> bool {
        self.status.eq_ignore_ascii_case(status)
    }

    pub fn is_exhausted(&self) -> bool {
        self.has_status(PROVIDER_QUOTA_STATUS_EXHAUSTED)
    }
}

/// Pure predicate: should a quota row be polled at `now`?
///
/// Mirrors the Go channel-query filter in
/// `ProviderQuotaService.runQuotaCheck` (`provider_quota.go:504-513`):
///
/// ```text
/// channel.Or(
///   channel.Not(channel.HasProviderQuotaStatus()),         // never checked -> due
///   channel.HasProviderQuotaStatusWith(
///     channel.NextCheckAtLTE(now)))                        // next_check_at <= now -> due
/// ```
///
/// `check_interval` is included so callers that only store `next_reset_at`
/// (and a row-level `last_seen`) can still derive the next-check deadline the
/// same way Go's `saveQuotaStatus` does (`nextCheck := now.Add(interval)`).
pub fn is_due_for_check(
    row: Option<&ProviderQuotaStatusRow>,
    now: DateTime<Utc>,
    check_interval: Duration,
) -> bool {
    let Some(row) = row else {
        // No existing row: Go's `Not(HasProviderQuotaStatus())` branch.
        return true;
    };

    // If `next_check_at` is populated, honor it directly — that's what the Go
    // query filter does.
    if let Some(next_check_at) = row.next_check_at {
        return now >= next_check_at;
    }

    // Row exists without `next_check_at` (e.g. legacy row, or set via a path
    // that didn't compute it). Fall back to the interval-based schedule Go
    // uses when writing rows: derive an effective next-check deadline from
    // the row's creation time encoded in `quota_data.last_seen` if present,
    // otherwise treat as due so the scheduler picks it up.
    if let Some(last_seen) = row
        .quota_data
        .get("last_seen")
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
    {
        return now >= last_seen + check_interval;
    }

    // No schedule signal at all — poll now so the row is refreshed.
    true
}

#[async_trait]
pub trait ProviderQuotaStatusRepo: Send + Sync {
    async fn get_provider_quota_status(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> ProviderQuotaServiceResult<Option<ProviderQuotaStatus>>;

    async fn set_provider_quota_status(
        &self,
        ctx: &RequestContext,
        status: ProviderQuotaStatus,
    ) -> ProviderQuotaServiceResult<ProviderQuotaStatus>;

    async fn reset_provider_quota_status(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> ProviderQuotaServiceResult<Option<ProviderQuotaStatus>>;
}

pub struct ProviderQuotaService {
    repo: Arc<dyn ProviderQuotaStatusRepo>,
}

impl ProviderQuotaService {
    pub fn new(repo: Arc<dyn ProviderQuotaStatusRepo>) -> Self {
        Self { repo }
    }

    pub async fn get_status(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> ProviderQuotaServiceResult<Option<ProviderQuotaStatus>> {
        self.repo.get_provider_quota_status(ctx, channel_id).await
    }

    pub async fn set_status(
        &self,
        ctx: &RequestContext,
        status: ProviderQuotaStatus,
    ) -> ProviderQuotaServiceResult<ProviderQuotaStatus> {
        self.repo.set_provider_quota_status(ctx, status).await
    }

    pub async fn set_status_with_invalidation(
        &self,
        ctx: &RequestContext,
        status: ProviderQuotaStatus,
    ) -> ProviderQuotaServiceResult<(ProviderQuotaStatus, ProviderQuotaCacheInvalidation)> {
        let status = self.set_status(ctx, status).await?;
        let invalidation = ProviderQuotaCacheInvalidation::for_status(&status);
        Ok((status, invalidation))
    }

    pub async fn reset_status(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> ProviderQuotaServiceResult<Option<ProviderQuotaStatus>> {
        self.repo.reset_provider_quota_status(ctx, channel_id).await
    }

    pub async fn reset_status_with_invalidation(
        &self,
        ctx: &RequestContext,
        channel_id: &str,
    ) -> ProviderQuotaServiceResult<Option<(ProviderQuotaStatus, ProviderQuotaCacheInvalidation)>>
    {
        let Some(status) = self.reset_status(ctx, channel_id).await? else {
            return Ok(None);
        };
        let invalidation = ProviderQuotaCacheInvalidation::for_status(&status);
        Ok(Some((status, invalidation)))
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryProviderQuotaStatusRepo {
    inner: Arc<Mutex<BTreeMap<String, ProviderQuotaStatus>>>,
}

impl InMemoryProviderQuotaStatusRepo {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(
        &self,
    ) -> ProviderQuotaServiceResult<std::sync::MutexGuard<'_, BTreeMap<String, ProviderQuotaStatus>>>
    {
        self.inner
            .lock()
            .map_err(|_| ProviderQuotaServiceError::LockPoisoned)
    }
}

#[async_trait]
impl ProviderQuotaStatusRepo for InMemoryProviderQuotaStatusRepo {
    async fn get_provider_quota_status(
        &self,
        _ctx: &RequestContext,
        channel_id: &str,
    ) -> ProviderQuotaServiceResult<Option<ProviderQuotaStatus>> {
        Ok(self.lock()?.get(channel_id).cloned())
    }

    async fn set_provider_quota_status(
        &self,
        _ctx: &RequestContext,
        status: ProviderQuotaStatus,
    ) -> ProviderQuotaServiceResult<ProviderQuotaStatus> {
        self.lock()?
            .insert(status.channel_id.clone(), status.clone());
        Ok(status)
    }

    async fn reset_provider_quota_status(
        &self,
        _ctx: &RequestContext,
        channel_id: &str,
    ) -> ProviderQuotaServiceResult<Option<ProviderQuotaStatus>> {
        let mut inner = self.lock()?;
        let Some(status) = inner.get_mut(channel_id) else {
            return Ok(None);
        };

        *status = status.clone().reset();
        Ok(Some(status.clone()))
    }
}

// ───────────────────────────────────────────────────────────────────────────
// S05 — check_interval + warning-ratio scheduling (pure)
// ───────────────────────────────────────────────────────────────────────────
//
// Mirrors the Go scheduling helpers in `provider_quota.go:336-394`:
//   - `intervalToCronExpr(interval)`            -> cron expression
//   - `getWarningCheckInterval()`               -> interval * ratio
//   - `nextCheckIntervalForStatus(status)`      -> warning ? warning-interval : interval
//   - `getCheckInterval()`                      -> configured || 5m
// In Rust these are pure free functions over `Duration` so they can be unit
// tested without constructing a full service. The Go semantics are replicated
// exactly, including the `ratio <= 0 -> 4` fallback, the hourly vs sub-hourly
// cron branches, and the warning-status short-circuit.

/// Resolves the effective quota check interval, mirroring Go
/// `ProviderQuotaService.getCheckInterval()` (`provider_quota.go:388-394`):
/// the configured interval wins when > 0, otherwise the 5-minute default
/// (`PROVIDER_QUOTA_DEFAULT_CHECK_INTERVAL_SECS`) is used.
pub fn effective_check_interval(configured: Duration) -> Duration {
    if configured > Duration::ZERO {
        configured
    } else {
        Duration::from_secs(PROVIDER_QUOTA_DEFAULT_CHECK_INTERVAL_SECS)
    }
}

/// Resolves the effective warning-check interval ratio, mirroring Go
/// `ProviderQuotaService.getWarningCheckInterval()` (`provider_quota.go:372-379`):
/// a non-positive ratio falls back to 4.
pub fn effective_warning_check_interval_ratio(configured: u32) -> u32 {
    if configured == 0 {
        PROVIDER_QUOTA_DEFAULT_WARNING_CHECK_INTERVAL_RATIO
    } else {
        configured
    }
}

/// Mirrors Go `ProviderQuotaService.getWarningCheckInterval()` — the interval
/// at which near-exhausted (`warning`) channels are re-polled. It is the
/// normal check interval multiplied by the warning ratio (default 4x).
///
/// Returns `Duration::MAX` on overflow so a misconfigured huge ratio cannot
/// panic; callers should treat an absurd interval as "effectively never".
pub fn warning_check_interval(
    configured_interval: Duration,
    configured_warning_ratio: u32,
) -> Duration {
    let interval = effective_check_interval(configured_interval);
    let ratio = effective_warning_check_interval_ratio(configured_warning_ratio);
    interval.checked_mul(ratio).unwrap_or(Duration::MAX)
}

/// Mirrors Go `ProviderQuotaService.nextCheckIntervalForStatus()`
/// (`provider_quota.go:381-386`): `warning` status uses the (longer) warning
/// interval; every other status uses the normal check interval.
pub fn next_check_interval_for_status(
    status: &str,
    configured_interval: Duration,
    configured_warning_ratio: u32,
) -> Duration {
    if status.eq_ignore_ascii_case(PROVIDER_QUOTA_STATUS_WARNING) {
        warning_check_interval(configured_interval, configured_warning_ratio)
    } else {
        effective_check_interval(configured_interval)
    }
}

/// Mirrors Go `ProviderQuotaService.intervalToCronExpr()`
/// (`provider_quota.go:336-370`): converts a `Duration` into a UTC cron
/// expression understood by the scheduler.
///
/// Branch order matches Go exactly:
///   1. Whole-hour intervals that divide evenly -> `0 * * * *` or `0 */N * * *`.
///   2. Sub-hour intervals that divide evenly into 60 -> `*/N * * * *`.
///   3. Otherwise: round down to the largest supported divisor of 60
///      `{1,2,3,4,5,6,10,12,15,20,30,60}` and emit `*/N * * * *`.
///
/// Returns `None` for zero-length intervals (no valid cron maps to "never").
pub fn interval_to_cron_expr(interval: Duration) -> Option<String> {
    let total_secs = interval.as_secs();
    if total_secs == 0 {
        return None;
    }
    let minutes = (total_secs / 60) as i64;
    let hours = minutes / 60;

    // Whole-hour intervals.
    if hours >= 1 && minutes % 60 == 0 {
        if hours == 1 {
            return Some("0 * * * *".to_string());
        }
        return Some(format!("0 */{hours} * * *"));
    }

    // Sub-hour intervals that divide 60 evenly.
    if minutes > 0 && 60 % minutes == 0 {
        return Some(format!("*/{minutes} * * * *"));
    }

    // Round down to the largest supported divisor of 60 not exceeding `minutes`.
    // Mirrors Go's `supportedIntervals` + `lo.Filter` + `lo.Max`.
    const SUPPORTED_INTERVALS: &[i64] = &[1, 2, 3, 4, 5, 6, 10, 12, 15, 20, 30, 60];
    let rounded = SUPPORTED_INTERVALS
        .iter()
        .copied()
        .filter(|si| *si <= minutes)
        .max()
        .unwrap_or(60);

    Some(format!("*/{rounded} * * * *"))
}

/// S05 — Pure warning-ratio decision logic. Given a channel's per-limit
/// `usage_ratio` (used / limit, in `[0.0, +inf]`), produce the effective
/// normalized status string mirroring the Go checker convention
/// (`WarningThresholdRatio = 0.8`):
///
///   - `usage_ratio >= 1.0`                       -> `"exhausted"`
///   - `usage_ratio >= WarningThresholdRatio(0.8)`-> `"warning"`
///   - otherwise                                  -> `"available"`
///
/// This centralizes the threshold comparison so the future HTTP-bound checker
/// and the S10 status-row scheduler can share one source of truth. The Go
/// checkers each apply the same threshold inline (e.g.
/// `nanogpt_checker.go:258 if usageRatio >= WarningThresholdRatio`); we mirror
/// the `>=` form which is the dominant Go convention.
pub fn status_for_usage_ratio(usage_ratio: f64) -> &'static str {
    if usage_ratio >= 1.0 {
        PROVIDER_QUOTA_STATUS_EXHAUSTED
    } else if usage_ratio >= PROVIDER_QUOTA_WARNING_THRESHOLD_RATIO {
        PROVIDER_QUOTA_STATUS_WARNING
    } else {
        PROVIDER_QUOTA_STATUS_READY
    }
}

/// S05 — `Ready` flag for a normalized status, mirroring Go
/// `provider_quota.IsReadyStatus` (`provider_quota/types.go:57-59`):
/// `available` and `warning` are ready, everything else is not.
pub fn is_ready_status(status: &str) -> bool {
    status.eq_ignore_ascii_case(PROVIDER_QUOTA_STATUS_READY)
        || status.eq_ignore_ascii_case(PROVIDER_QUOTA_STATUS_WARNING)
}

/// Mirrors Go `quotaStatusRank` (`provider_quota.go:83-96`): a strict total
/// ordering of statuses from least severe (`available` = 0) to most severe
/// (`exhausted` = 2). `unknown` ranks lowest so it never overrides a known
/// status when aggregating per-limit results. Used by
/// [`effective_limit_status`].
pub fn quota_status_rank(status: &str) -> i8 {
    if status.eq_ignore_ascii_case(PROVIDER_QUOTA_STATUS_READY) {
        0
    } else if status.eq_ignore_ascii_case(PROVIDER_QUOTA_STATUS_WARNING) {
        1
    } else if status.eq_ignore_ascii_case(PROVIDER_QUOTA_STATUS_EXHAUSTED) {
        2
    } else {
        // unknown and anything unrecognized.
        -1
    }
}

/// S05/S07 — Aggregates a slice of per-limit statuses into the single worst
/// status, mirroring Go `QuotaChannelStatus.EffectiveStatus`
/// (`provider_quota.go:39-81`) for the limit-matching loop (the
/// channel-level short-circuit is the caller's responsibility). Returns
/// `None` when `limits` is empty (Go: "No matching limit type -> Unknown with
/// ready=true").
pub fn worst_limit_status<'a, S: AsRef<str> + 'a>(
    limits: impl IntoIterator<Item = &'a S>,
) -> Option<&'a str> {
    limits
        .into_iter()
        .map(|s| s.as_ref())
        .max_by_key(|s| quota_status_rank(s))
}

/// S16 — Mirrors Go `QuotaChannelStatus.EffectiveStatus(limitType)`
/// (`provider_quota.go:39-81`): resolves the effective `(status, ready)` pair
/// for a given limit dimension, accounting for channel-level short-circuits and
/// per-limit aggregation.
///
/// Branch order matches Go exactly:
/// 1. Channel-level `exhausted` short-circuits to `(exhausted, false)` — a
///    channel marked exhausted at the top level is treated as fully unavailable
///    regardless of per-limit data.
/// 2. No limits → return the channel-level `(status, ready)` pair.
/// 3. Filter limits by `limit_type`; among matching limits, pick the worst
///    status (highest [`quota_status_rank`]). When ranks tie, `ready` is the
///    AND of all matching limits' ready flags (Go:
///    `worstReady = worstReady && l.Ready`). The first matching limit seeds
///    both `worstStatus` and `worstReady`.
/// 4. No matching limit type → `(unknown, true)` so missing data does not
///    block routing (Go: "This differs from a per-limit 'unknown' status where
///    ready=false").
///
/// Returns a `(&'a str, bool)` where the `&str` borrows from `channel_status`
/// or `limits` (or a `&'static str` constant for the short-circuit/fallback
/// arms).
pub fn effective_limit_status<'a>(
    channel_status: &'a str,
    channel_ready: bool,
    limits: &'a [CacheQuotaLimitStatus],
    limit_type: &CacheQuotaLimitType,
) -> (&'a str, bool) {
    // 1. Channel-level exhausted short-circuit.
    if channel_status.eq_ignore_ascii_case(PROVIDER_QUOTA_STATUS_EXHAUSTED) {
        return (PROVIDER_QUOTA_STATUS_EXHAUSTED, false);
    }

    // 2. No limits → return channel-level status.
    if limits.is_empty() {
        return (channel_status, channel_ready);
    }

    // 3. Filter by limit type and aggregate worst.
    let mut worst_status: Option<&str> = None;
    let mut worst_ready = true;

    for limit in limits {
        if &limit.limit_type != limit_type {
            continue;
        }

        match worst_status {
            None => {
                // First matching limit seeds both status and ready.
                worst_status = Some(limit.status.as_str());
                worst_ready = limit.ready;
            }
            Some(current) => {
                let new_rank = quota_status_rank(&limit.status);
                let current_rank = quota_status_rank(current);
                if new_rank > current_rank {
                    // Higher severity: replace both status and ready.
                    worst_status = Some(limit.status.as_str());
                    worst_ready = limit.ready;
                } else if new_rank == current_rank {
                    // Equal severity: AND the ready flag (Go:
                    // `worstReady = worstReady && l.Ready`).
                    worst_ready = worst_ready && limit.ready;
                }
                // Lower severity: do nothing.
            }
        }
    }

    // 4. No matching limit type → unknown, true (missing data should not
    //    block routing).
    match worst_status {
        Some(status) => (status, worst_ready),
        None => (PROVIDER_QUOTA_STATUS_UNKNOWN, true),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// S13 — Cache-invalidation → candidate-selector recompute signal
// ───────────────────────────────────────────────────────────────────────────
//
// Go wiring: when `ProviderQuotaService.updateQuotaCache` writes a fresh
// status (`provider_quota.go:428-434`), the in-memory cache is updated; the
// candidate selector (`orchestrator/candidates_quota.go`) reads
// `GetQuotaStatus` on every `Select()` call, so it naturally observes the new
// status. There is no explicit invalidation event in Go — the sync.Map IS the
// signal.
//
// In Rust, the pure service slice already produces a
// `ProviderQuotaCacheInvalidation` descriptor from S06/S07 (`set_status_with_
// invalidation` / `reset_status_with_invalidation`). S13 closes the loop by
// defining the subscriber boundary the candidate selector (which lives in
// `conduit-orchestrator`, a different crate) implements to receive
// invalidations. Per task instructions we model the trait here and document
// the orchestrator-side wiring as a follow-up; we do NOT edit
// `conduit-orchestrator`.

/// S13 — Subscriber trait the candidate selector (or any other consumer)
/// implements to be notified when a provider-quota status change invalidates
/// cached candidate-filtering decisions.
///
/// The orchestrator's `ProviderQuotaSelector`
/// (`orchestrator/candidates_quota.go`) is the canonical Go consumer; its
/// Rust counterpart in `conduit-orchestrator` will implement this trait so
/// that `conduit-services` can fan out invalidations without a circular
/// dependency. **Orchestrator-side wiring is a documented follow-up** — this
/// crate only defines the contract and a minimal in-service notifier.
pub trait QuotaCacheInvalidationSubscriber: Send + Sync {
    /// Called once per status change. Implementations MUST be non-blocking;
    /// heavy recomputation should be deferred (e.g. flagged as stale so the
    /// next `Select()` call re-reads fresh quota status).
    fn on_quota_cache_invalidated(&self, invalidation: &ProviderQuotaCacheInvalidation);
}

/// S13 — Minimal in-service notifier that fans a single invalidation out to
/// every registered subscriber. The service holds one of these behind its
/// `Arc`; the orchestrator wires its selector in at construction time.
///
/// This is intentionally a thin broadcast — no queuing, no async — because the
/// Go path is also synchronous (the cache write IS the signal, and the next
/// `Select()` observes it). Subscribers are expected to mark their own state
/// stale and let the next request recompute.
#[derive(Default)]
pub struct QuotaCacheInvalidationNotifier {
    subscribers: Mutex<Vec<Arc<dyn QuotaCacheInvalidationSubscriber>>>,
}

impl std::fmt::Debug for QuotaCacheInvalidationNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.subscribers.lock().map(|s| s.len()).unwrap_or(0);
        f.debug_struct("QuotaCacheInvalidationNotifier")
            .field("subscriber_count", &count)
            .finish()
    }
}

impl QuotaCacheInvalidationNotifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a subscriber. Idempotent w.r.t. pointer-equal `Arc`s.
    pub fn subscribe(&self, subscriber: Arc<dyn QuotaCacheInvalidationSubscriber>) {
        let mut subs = self
            .subscribers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !subs.iter().any(|s| Arc::ptr_eq(s, &subscriber)) {
            subs.push(subscriber);
        }
    }

    /// Broadcasts an invalidation to every subscriber. A panicking subscriber
    /// cannot poison the Mutex (we recover via `into_inner`), but subscribers
    /// are expected to be non-fallible and non-blocking per the trait contract.
    pub fn notify(&self, invalidation: &ProviderQuotaCacheInvalidation) {
        let subs = self
            .subscribers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for sub in subs.iter() {
            sub.on_quota_cache_invalidated(invalidation);
        }
    }

    /// Number of registered subscribers (useful for assertions in tests).
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().map(|s| s.len()).unwrap_or(0)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// S14 — Limits ↔ quota_data JSON roundtrip (DB persistence helper)
// ───────────────────────────────────────────────────────────────────────────
//
// Go's `ProviderQuotaService` stores the per-limit snapshots *inside* the
// `quota_data` JSON column of the `provider_quota_status` row (under the
// `_limits` key) rather than in a separate column. Two helpers serialize and
// deserialize that shape:
//
//   - `mergeLimitsIntoQuotaData` (`provider_quota.go:723-744`) writes the
//     channel's RawData + a `_limits` array into the column.
//   - `extractLimitsFromQuotaData` (`provider_quota.go:746-798`) reads the
//     `_limits` array back, tolerating either `[]map[string]any` (Go-native)
//     or `[]any` (post-JSON-roundtrip) shapes.
//
// The pure helpers below mirror both behaviors exactly so the future DB-backed
// `ProviderQuotaStatusRepo` implementation can persist + reload per-limit data
// through the same wire shape the Go code uses (which the React frontend also
// reads directly from the JSON column).

/// Limit dimension a provider exposes, mirroring Go
/// `provider_quota.QuotaLimitType` (`provider_quota/types.go:18-24`).
///
/// Kept as a `String`-backed enum (rather than the typed enum in
/// [`crate::QuotaLimitType`]) because the cache/persistence layer must
/// round-trip arbitrary provider-supplied limit-type strings without
/// collapsing unknown values — Go persists the raw string token via
/// `QuotaLimitType(s)`. The [`CacheQuotaLimitType::Other`] arm preserves the
/// original token so unknown values are lossless across DB writes.
///
/// Serde uses a custom string representation (`"token"` / `"image"` /
/// `"subscription_cycle"` / raw unknown token) so the wire shape matches
/// Go's `string(QuotaLimitType)` cast exactly — every variant serializes to
/// and deserializes from a single JSON string. This means a `CacheQuotaLimitStatus`
/// struct serialized directly via serde also produces the Go-faithful
/// `"type": "token"` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheQuotaLimitType {
    Token,
    Image,
    SubscriptionCycle,
    /// Catch-all for provider-specific limit types outside the canonical
    /// `token` / `image` / `subscription_cycle` set. Serialized as the raw
    /// string token (mirrors Go's `string(QuotaLimitType)` cast), so the wire
    /// shape is preserved even though the value isn't one of the named arms.
    /// Use [`CacheQuotaLimitType::from_str_ci`] to construct, and
    /// [`CacheQuotaLimitType::as_str`] to read.
    Other(String),
}

impl CacheQuotaLimitType {
    /// Parse a raw limit-type token into the enum, returning
    /// [`CacheQuotaLimitType::Other`] for any unrecognized provider-specific
    /// value (mirrors Go's tolerant `QuotaLimitType(s)` cast, which never
    /// discards data). The original token is preserved on the `Other` variant
    /// so the roundtrip is lossless.
    pub fn from_str_ci(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "token" => Self::Token,
            "image" => Self::Image,
            "subscription_cycle" => Self::SubscriptionCycle,
            _ => Self::Other(raw.to_string()),
        }
    }

    /// Stable string token persisted in the `_limits` JSON array; mirrors Go's
    /// `string(QuotaLimitType)` cast.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Token => "token",
            Self::Image => "image",
            Self::SubscriptionCycle => "subscription_cycle",
            Self::Other(raw) => raw,
        }
    }

    /// Constructor for the `Other` arm; provided so callers can build the
    /// typed value from an unknown token without naming the inner struct.
    pub fn other(raw: impl Into<String>) -> Self {
        Self::Other(raw.into())
    }
}

impl Serialize for CacheQuotaLimitType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for CacheQuotaLimitType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_str_ci(&raw))
    }
}

/// One per-limit snapshot as persisted in the `quota_data._limits` JSON array.
/// Mirrors Go `provider_quota.QuotaLimitStatus` (`types.go:29-35`): every
/// field is preserved so DB ↔ cache roundtrips are lossless (the
/// [`crate::QuotaLimitStatus`] type in `quota_service` drops `next_reset_at`
/// because the pure enforcement decision never consults it; here we keep it
/// because the scheduler + UI do).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CacheQuotaLimitStatus {
    /// JSON tag is `type` (Go field tag), not `limit_type`.
    #[serde(rename = "type")]
    pub limit_type: CacheQuotaLimitType,
    pub status: String,
    #[serde(default)]
    pub usage_ratio: f64,
    #[serde(default)]
    pub ready: bool,
    /// Mirrors Go's `*time.Time` — absent on the wire when `None` (Go nil).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_reset_at: Option<DateTime<Utc>>,
}

impl CacheQuotaLimitStatus {
    pub fn new(
        limit_type: CacheQuotaLimitType,
        status: impl Into<String>,
        usage_ratio: f64,
        ready: bool,
        next_reset_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            limit_type,
            status: status.into(),
            usage_ratio,
            ready,
            next_reset_at,
        }
    }
}

/// Key under which the per-limit array is nested inside `quota_data`.
/// Mirrors Go's literal `"data[\"_limits\"] = limitMaps"` write
/// (`provider_quota.go:740`).
pub const QUOTA_DATA_LIMITS_KEY: &str = "_limits";

/// Merge per-limit snapshots into a JSON `quota_data` object, mirroring Go
/// `ProviderQuotaService.mergeLimitsIntoQuotaData`
/// (`provider_quota.go:723-744`).
///
/// Behavior:
///   - Start from `raw_data` (an arbitrary JSON object, possibly empty).
///   - When `limits` is non-empty, serialize each limit as a JSON object
///     `{type, status, usageRatio, ready, nextResetAt?}` and store the array
///     under [`QUOTA_DATA_LIMITS_KEY`] (`"_limits"`). `nextResetAt` is written
///     only when `Some` (Go: `if l.NextResetAt != nil`).
///   - When `limits` is empty, do NOT write the `_limits` key at all (Go
///     `if len(quotaData.Limits) > 0` guard).
///   - Existing entries in `raw_data` are preserved (mirrors Go
///     `lo.Assign(map[string]any{}, quotaData.RawData)`).
///
/// Returns the merged object. If `raw_data` is not a JSON object, it is
/// discarded (matching Go's `map[string]any` initialization — a non-object
/// raw payload would be a contract violation).
pub fn merge_limits_into_quota_data(raw_data: &Value, limits: &[CacheQuotaLimitStatus]) -> Value {
    // Start from a fresh object so we never mutate the caller's `raw_data`.
    // Mirrors Go's `lo.Assign(map[string]any{}, quotaData.RawData)`.
    let mut merged = match raw_data {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };

    if limits.is_empty() {
        return Value::Object(merged);
    }

    let limit_maps: Vec<Value> = limits
        .iter()
        .map(|limit| {
            let mut m = Map::new();
            m.insert(
                "type".to_string(),
                Value::String(limit.limit_type.as_str().to_string()),
            );
            m.insert("status".to_string(), Value::String(limit.status.clone()));
            m.insert("usageRatio".to_string(), Value::from(limit.usage_ratio));
            m.insert("ready".to_string(), Value::Bool(limit.ready));
            if let Some(reset_at) = limit.next_reset_at {
                // Go uses `time.RFC3339` (`nextResetAt.Format(time.RFC3339)`).
                m.insert(
                    "nextResetAt".to_string(),
                    Value::String(reset_at.to_rfc3339()),
                );
            }
            Value::Object(m)
        })
        .collect();

    merged.insert(QUOTA_DATA_LIMITS_KEY.to_string(), Value::Array(limit_maps));
    Value::Object(merged)
}

/// Read back the per-limit array written by [`merge_limits_into_quota_data`].
/// Mirrors Go `extractLimitsFromQuotaData` (`provider_quota.go:746-798`):
///
///   - Returns an empty `Vec` when `quota_data` is missing the `_limits` key
///     (Go: `return nil`).
///   - Tolerates both the typed array shape (objects directly) and the
///     post-JSON-roundtrip shape (objects wrapped in `Value::Object`); Go
///     matches both `[]map[string]any` and `[]any`.
///   - Each entry's fields are read best-effort: missing/incorrectly-typed
///     fields fall back to zero values, mirroring Go's per-field type-assert
///     chain.
pub fn extract_limits_from_quota_data(data: &Value) -> Vec<CacheQuotaLimitStatus> {
    let Some(raw_limits) = data.get(QUOTA_DATA_LIMITS_KEY) else {
        return Vec::new();
    };
    let Value::Array(arr) = raw_limits else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let Value::Object(m) = entry else {
            continue;
        };

        let limit_type = m
            .get("type")
            .and_then(Value::as_str)
            .map(CacheQuotaLimitType::from_str_ci)
            .unwrap_or_else(|| CacheQuotaLimitType::other(""));

        let status = m
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let usage_ratio = m
            .get("usageRatio")
            .and_then(|v| {
                // Go unmarshal produces `float64` for numeric JSON values; but
                // integer JSON (`json.Number` off) also lands as `float64` in
                // `map[string]any`. Accept both `f64` and `i64` defensively.
                v.as_f64()
            })
            .unwrap_or_default();

        let ready = m.get("ready").and_then(Value::as_bool).unwrap_or(false);

        let next_reset_at = m
            .get("nextResetAt")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        out.push(CacheQuotaLimitStatus {
            limit_type,
            status,
            usage_ratio,
            ready,
            next_reset_at,
        });
    }
    out
}

// ───────────────────────────────────────────────────────────────────────────
// S15 — `hasCredentialsForProvider` gate (pure)
// ───────────────────────────────────────────────────────────────────────────
//
// Go `hasCredentialsForProvider(ch)` (`provider_quota.go:707-721`) gates
// whether a channel is even eligible for a quota check. The three branches:
//
//   1. OpenAI-compatible channel types (`openai` / `openai_responses`) whose
//      base URL resolves to a URL-detected Pattern-B provider (`wafer`,
//      `synthetic`, `neuralwatt`, `apertis`) — these providers use simple API
//      keys (NOT OAuth), so only `api_key` / `api_keys` count. OAuth tokens
//      are explicitly ignored.
//   2. `codex` / `claudecode` channels — OAuth-only: a non-empty `oauth`
//      field OR an OAuth-JSON `api_key` blob is required. A plain `api_key`
//      string is rejected.
//   3. Every other channel type — accepts OAuth, OAuth-JSON `api_key`, plain
//      `api_key`, or `api_keys` (any auth flavor is fine).
//
// The pure helper below takes already-extracted inputs (channel type, base
// URL, credentials) so it can be unit-tested without constructing a full
// `Channel` row.

/// Inputs to [`has_credentials_for_provider`], mirroring the fields Go reads
/// off `*ent.Channel` inside `hasCredentialsForProvider`.
#[derive(Debug, Clone)]
pub struct ChannelCredentialView<'a> {
    pub channel_type: &'a str,
    pub base_url: &'a str,
    pub credentials: &'a conduit_core::objects::channel_settings::ChannelCredentials,
}

/// Pure port of Go `hasCredentialsForProvider(ch)` (`provider_quota.go:707-721`).
///
/// Returns `true` when the channel has at least one credential the provider's
/// quota checker is willing to use. See the S15 section doc above for the
/// three branch rules.
pub fn has_credentials_for_provider(view: &ChannelCredentialView<'_>) -> bool {
    let normalized = normalize_provider_type(view.channel_type);
    let creds = view.credentials;

    // Branch 1: OpenAI-compatible + URL-detected Pattern-B provider.
    if matches!(normalized.as_str(), "openai" | "openai_responses")
        && let Some(provider_type) = detect_provider_from_url(view.base_url)
        && url_detected_providers().contains(&provider_type.as_str())
    {
        // Only plain API keys count for these providers.
        return !creds.api_key.trim().is_empty() || !creds.api_keys.is_empty();
    }
    // OpenAI-compatible but NOT a URL-detected provider falls through to
    // branch 3 (any-credential), matching Go's structure.

    // Branch 2: codex / claudecode are OAuth-only.
    if matches!(normalized.as_str(), "codex" | "claudecode") {
        return creds.is_oauth();
    }

    // Branch 3: any auth flavor is accepted.
    creds.is_oauth() || !creds.api_key.trim().is_empty() || !creds.api_keys.is_empty()
}

#[cfg(test)]
mod tests {
    use conduit_db::{PolicyContext, Principal, RequestContext};
    use serde_json::json;

    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::new(PolicyContext::new(Principal::test()))
    }

    #[test]
    fn quota_check_target_detects_known_provider_defaults() {
        let cases = [
            (
                "apertis",
                ProviderQuotaCheckTarget {
                    provider_type: "apertis".to_string(),
                    url: "https://api.apertis.ai/quota".to_string(),
                },
            ),
            (
                "codex",
                ProviderQuotaCheckTarget {
                    provider_type: "codex".to_string(),
                    url: "https://chatgpt.com/backend-api/codex/quota".to_string(),
                },
            ),
            (
                "nanogpt",
                ProviderQuotaCheckTarget {
                    provider_type: "nanogpt".to_string(),
                    url: "https://nano-gpt.com/api/subscription/v1/usage".to_string(),
                },
            ),
            (
                "synthetic",
                ProviderQuotaCheckTarget {
                    provider_type: "synthetic".to_string(),
                    url: "http://127.0.0.1/quota".to_string(),
                },
            ),
        ];

        for (provider_type, expected) in cases {
            assert_eq!(
                detect_provider_quota_check_target(provider_type, None, None),
                Some(expected)
            );
        }
    }

    #[test]
    fn quota_check_target_normalizes_provider_type_aliases() {
        assert_eq!(
            detect_provider_quota_check_target("NanoGPT-Responses", None, None),
            Some(ProviderQuotaCheckTarget {
                provider_type: "nanogpt".to_string(),
                url: "https://nano-gpt.com/api/subscription/v1/usage".to_string(),
            })
        );
    }

    #[test]
    fn quota_check_target_prefers_custom_base_url() {
        assert_eq!(
            detect_provider_quota_check_target("codex", Some("https://proxy.example/"), None),
            Some(ProviderQuotaCheckTarget {
                provider_type: "codex".to_string(),
                url: "https://proxy.example/backend-api/codex/quota".to_string(),
            })
        );
    }

    #[test]
    fn quota_check_target_prefers_settings_url() {
        let settings = json!({
            "provider_quota": {
                "quota_check_url": "/internal/quota/check"
            }
        });

        assert_eq!(
            detect_provider_quota_check_target(
                "apertis",
                Some("https://quota.example/api"),
                Some(&settings),
            ),
            Some(ProviderQuotaCheckTarget {
                provider_type: "apertis".to_string(),
                url: "https://quota.example/api/internal/quota/check".to_string(),
            })
        );
    }

    #[test]
    fn quota_check_target_allows_absolute_settings_url() {
        let settings = json!({
            "provider_quota_check_url": "https://quota.example/check"
        });

        assert_eq!(
            detect_provider_quota_check_target("synthetic", Some("http://unused"), Some(&settings)),
            Some(ProviderQuotaCheckTarget {
                provider_type: "synthetic".to_string(),
                url: "https://quota.example/check".to_string(),
            })
        );
    }

    #[test]
    fn quota_check_target_ignores_unsupported_provider() {
        assert_eq!(
            detect_provider_quota_check_target("unknown-provider", Some("https://example"), None),
            None
        );
    }

    #[tokio::test]
    async fn status_row_get_set_and_reset_round_trip() -> ProviderQuotaServiceResult<()> {
        let service = ProviderQuotaService::new(Arc::new(InMemoryProviderQuotaStatusRepo::new()));
        let ctx = ctx();
        let status = ProviderQuotaStatus {
            status: PROVIDER_QUOTA_STATUS_EXHAUSTED.to_string(),
            ready: false,
            quota_data: json!({"remaining": 0, "limit": 100}),
            ..ProviderQuotaStatus::new("channel-a", "synthetic")
        };

        let saved = service.set_status(&ctx, status.clone()).await?;
        let fetched = service.get_status(&ctx, "channel-a").await?;
        let reset = service.reset_status(&ctx, "channel-a").await?;

        assert_eq!(saved, status);
        assert_eq!(fetched, Some(status));
        assert_eq!(
            reset,
            Some(ProviderQuotaStatus::new("channel-a", "synthetic"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_update_returns_candidate_selector_invalidation()
    -> ProviderQuotaServiceResult<()> {
        let service = ProviderQuotaService::new(Arc::new(InMemoryProviderQuotaStatusRepo::new()));
        let ctx = ctx();
        let status = ProviderQuotaStatus {
            status: PROVIDER_QUOTA_STATUS_EXHAUSTED.to_string(),
            ready: false,
            ..ProviderQuotaStatus::new("channel-a", "claudecode")
        };

        let (_saved, invalidation) = service.set_status_with_invalidation(&ctx, status).await?;

        assert_eq!(invalidation.scope, "candidate_selector");
        assert_eq!(invalidation.channel_id, "channel-a");
        assert_eq!(invalidation.provider_type, "claudecode");
        assert_eq!(
            invalidation.key,
            "candidate_selector:provider_quota:channel-a:claudecode"
        );
        Ok(())
    }

    #[tokio::test]
    async fn status_reset_returns_same_candidate_selector_invalidation_key()
    -> ProviderQuotaServiceResult<()> {
        let service = ProviderQuotaService::new(Arc::new(InMemoryProviderQuotaStatusRepo::new()));
        let ctx = ctx();
        let status = ProviderQuotaStatus {
            status: PROVIDER_QUOTA_STATUS_EXHAUSTED.to_string(),
            ready: false,
            ..ProviderQuotaStatus::new(":channel-a:", "claudecode")
        };

        let (_saved, update_invalidation) =
            service.set_status_with_invalidation(&ctx, status).await?;
        let Some((_reset, reset_invalidation)) = service
            .reset_status_with_invalidation(&ctx, ":channel-a:")
            .await?
        else {
            panic!("expected reset status");
        };

        assert_eq!(reset_invalidation.scope, update_invalidation.scope);
        assert_eq!(reset_invalidation.key, update_invalidation.key);
        assert_eq!(
            reset_invalidation.key,
            "candidate_selector:provider_quota:channel-a:claudecode"
        );
        Ok(())
    }

    #[tokio::test]
    async fn ready_and_status_fields_are_preserved() -> ProviderQuotaServiceResult<()> {
        let service = ProviderQuotaService::new(Arc::new(InMemoryProviderQuotaStatusRepo::new()));
        let ctx = ctx();
        let status = ProviderQuotaStatus {
            status: "warning".to_string(),
            ready: false,
            ..ProviderQuotaStatus::new("channel-a", "synthetic")
        };

        service.set_status(&ctx, status).await?;
        let fetched = service.get_status(&ctx, "channel-a").await?;

        assert_eq!(
            fetched.map(|status| (status.ready, status.status)),
            Some((false, "warning".to_string()))
        );
        Ok(())
    }

    // ─── S09: checker registry ──────────────────────────────────────────────

    /// Mirrors Go `provider_quota_url_test.go::TestGetProviderType_ExistingTypesPreserved`
    /// for the dedicated (Pattern A) channel types registered in
    /// `NewProviderQuotaService` (`provider_quota.go:280-287`).
    #[test]
    fn checker_for_returns_registered_dedicated_channel_types() {
        let cases = [
            ("claudecode", ProviderCheckerKind::ClaudeCode),
            ("codex", ProviderCheckerKind::Codex),
            ("github_copilot", ProviderCheckerKind::GithubCopilot),
            ("nanogpt", ProviderCheckerKind::NanoGpt),
            ("nanogpt_responses", ProviderCheckerKind::NanoGpt),
        ];
        for (channel_type, expected) in cases {
            assert_eq!(
                checker_for(channel_type),
                Some(expected),
                "channel_type = {channel_type}"
            );
        }
    }

    /// Mirrors Go `NewProviderQuotaService` registration: the same set of
    /// provider_types must be reachable from both the dedicated and URL-detected
    /// paths so `svc.checkers[providerType]` always resolves.
    #[test]
    fn checker_for_provider_type_string_round_trips() {
        for kind in ProviderCheckerKind::all() {
            let provider_type = kind.as_provider_type();
            assert_eq!(checker_for(provider_type), Some(*kind));
            if matches!(
                kind,
                ProviderCheckerKind::Wafer
                    | ProviderCheckerKind::Synthetic
                    | ProviderCheckerKind::NeuralWatt
                    | ProviderCheckerKind::Apertis
            ) {
                assert_eq!(
                    checker_for_url_detected_provider(provider_type),
                    Some(*kind)
                );
            }
        }
    }

    #[test]
    fn checker_for_rejects_unsupported_channel_type() {
        assert_eq!(checker_for("unknown"), None);
        assert_eq!(checker_for("openai"), None);
        assert_eq!(checker_for("openai_responses"), None);
    }

    #[test]
    fn checker_for_normalizes_aliases_and_case() {
        assert_eq!(
            checker_for("ClaudeCode"),
            Some(ProviderCheckerKind::ClaudeCode)
        );
        assert_eq!(
            checker_for("Github-Copilot"),
            Some(ProviderCheckerKind::GithubCopilot)
        );
        assert_eq!(
            checker_for("NANOGPT-Responses"),
            Some(ProviderCheckerKind::NanoGpt)
        );
    }

    /// `check_target_url` for a dedicated channel type must produce the
    /// provider's default target URL — same expectation as
    /// `quota_check_target_detects_known_provider_defaults` but routed through
    /// the registry.
    #[test]
    fn check_target_url_resolves_dedicated_channel_type() {
        let target = check_target_url("codex", Some("https://proxy.example/"), None)
            .unwrap_or_else(|| panic!("expected target for codex"));
        assert_eq!(target.provider_type, "codex");
        assert_eq!(target.url, "https://proxy.example/backend-api/codex/quota");
    }

    /// `check_target_url` for an OpenAI-compatible channel must defer to URL
    /// detection (mirrors `provider_quota.go:700-701`).
    #[test]
    fn check_target_url_uses_url_detection_for_openai_channel_types() {
        let target = check_target_url("openai", Some("https://pass.wafer.ai"), None)
            .unwrap_or_else(|| panic!("expected target for wafer"));
        assert_eq!(target.provider_type, "wafer");
    }

    #[test]
    fn check_target_url_returns_none_for_unrecognized_openai_host() {
        assert_eq!(
            check_target_url("openai", Some("https://api.unknown.com"), None),
            None
        );
        assert_eq!(
            check_target_url("openai_responses", Some("https://example.org"), None),
            None
        );
    }

    /// HTTP-only checkers (`claudecode`, `github_copilot`) have no static URL
    /// template in the pure registry — the pure slice cannot fabricate a URL
    /// for them. Mirrors the fact that their checkers ship their own endpoints
    /// internally (see Go checker docs at `provider_quota.go:182-243`).
    #[test]
    fn check_target_url_has_no_template_for_http_only_checkers() {
        assert_eq!(check_target_url("claudecode", None, None), None);
        assert_eq!(check_target_url("github_copilot", None, None), None);
    }

    // ─── S11: URL detection (mirrors url_detection_test.go) ─────────────────

    /// Mirrors Go `url_detection_test.go::TestDetectProviderFromURL_Wafer`.
    #[test]
    fn detect_provider_from_url_wafer_matches_all_go_cases() {
        for base_url in [
            "https://wafer.ai",
            "https://pass.wafer.ai",
            "https://api.wafer.ai/v1/chat",
            "http://wafer.ai",
            "https://pass.wafer.ai:443",
            "https://pass.wafer.ai:8443",
        ] {
            assert_eq!(
                detect_provider_from_url(base_url).as_deref(),
                Some("wafer"),
                "base_url = {base_url}"
            );
        }
    }

    /// Mirrors Go `url_detection_test.go::TestDetectProviderFromURL_Synthetic`.
    #[test]
    fn detect_provider_from_url_synthetic_matches_all_go_cases() {
        for base_url in [
            "https://api.synthetic.new",
            "https://us-east.api.synthetic.new",
            "https://api.synthetic.new/v1/chat/completions",
            "https://api.synthetic.new:443",
        ] {
            assert_eq!(
                detect_provider_from_url(base_url).as_deref(),
                Some("synthetic"),
                "base_url = {base_url}"
            );
        }
    }

    /// Mirrors Go `url_detection_test.go::TestDetectProviderFromURL_NeuralWatt`.
    #[test]
    fn detect_provider_from_url_neuralwatt_matches_all_go_cases() {
        for base_url in [
            "https://api.neuralwatt.com",
            "https://us.api.neuralwatt.com",
            "https://api.neuralwatt.com/v1",
            "https://api.neuralwatt.com:443",
        ] {
            assert_eq!(
                detect_provider_from_url(base_url).as_deref(),
                Some("neuralwatt"),
                "base_url = {base_url}"
            );
        }
    }

    /// Mirrors Go `url_detection_test.go::TestDetectProviderFromURL_Apertis`.
    #[test]
    fn detect_provider_from_url_apertis_matches_all_go_cases() {
        for base_url in [
            "https://api.apertis.ai",
            "https://api.apertis.ai/v1/chat",
            "http://api.apertis.ai",
            "https://us.api.apertis.ai",
            "https://api.apertis.ai:443",
        ] {
            assert_eq!(
                detect_provider_from_url(base_url).as_deref(),
                Some("apertis"),
                "base_url = {base_url}"
            );
        }
    }

    /// Mirrors Go `url_detection_test.go::TestDetectProviderFromURL_Unknown`.
    #[test]
    fn detect_provider_from_url_unknown_returns_none() {
        for base_url in [
            "https://api.unknown-provider.com",
            "https://api.openai.com",
            "https://example.com",
        ] {
            assert_eq!(
                detect_provider_from_url(base_url),
                None,
                "base_url = {base_url}"
            );
        }
    }

    /// Mirrors Go `url_detection_test.go::TestDetectProviderFromURL_Empty`.
    #[test]
    fn detect_provider_from_url_empty_returns_none() {
        assert_eq!(detect_provider_from_url(""), None);
        assert_eq!(detect_provider_from_url("   "), None);
    }

    /// Mirrors Go `url_detection_test.go::TestDetectProviderFromURL_Malformed`.
    /// Inputs without an `http(s)://` scheme produce no host (Go `url.Parse`
    /// treats the whole input as Path/Opaque).
    #[test]
    fn detect_provider_from_url_malformed_returns_none() {
        assert_eq!(detect_provider_from_url("wafer.ai"), None);
        assert_eq!(detect_provider_from_url("api.synthetic.new"), None);
        assert_eq!(detect_provider_from_url("://invalid"), None);
    }

    /// Mirrors Go `url_detection_test.go::TestDetectProviderFromURL_FalsePositives`.
    /// `evilwafer.ai` / `fakeapi.synthetic.new` must NOT match because Go's
    /// rule is `host == pattern || HasSuffix(host, "."+pattern)`.
    #[test]
    fn detect_provider_from_url_rejects_false_positives() {
        assert_eq!(detect_provider_from_url("https://evilwafer.ai"), None);
        assert_eq!(
            detect_provider_from_url("https://fakeapi.synthetic.new"),
            None
        );
    }

    /// `url_detected_providers` mirrors Go
    /// `provider_quota.URLDetectedProviders()` — the set used by
    /// `hasCredentialsForProvider` to gate API-key-only auth for OpenAI
    /// channels.
    #[test]
    fn url_detected_providers_matches_go_set() {
        let providers = url_detected_providers();
        let mut sorted = providers.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, ["apertis", "neuralwatt", "synthetic", "wafer"]);
    }

    /// Mirrors Go `provider_quota_url_test.go::TestGetProviderType_OpenaiWith*URL`
    /// cases: when the OpenAI channel type is paired with a recognized
    /// provider host, `detect_quota_check_url` must select that provider's
    /// default check path.
    #[test]
    fn detect_quota_check_url_openai_resolves_by_host() {
        let cases = [
            (
                "https://pass.wafer.ai",
                "https://pass.wafer.ai/v1/inference/quota",
            ),
            (
                "https://api.synthetic.new",
                "https://api.synthetic.new/v2/quotas",
            ),
            (
                "https://api.neuralwatt.com",
                "https://api.neuralwatt.com/v1/quota",
            ),
        ];

        for (base_url, expected_url) in cases {
            assert_eq!(
                detect_quota_check_url("openai", Some(base_url), None),
                Some(expected_url.to_string()),
                "base_url = {base_url}"
            );
        }
    }

    /// Mirrors Go `provider_quota_url_test.go::TestGetProviderType_OpenaiWithEmptyURL`
    /// and `TestGetProviderType_OpenaiWithUnknownURL`: an OpenAI channel
    /// without a recognized host yields no quota URL.
    #[test]
    fn detect_quota_check_url_openai_returns_none_for_unknown_or_empty_host() {
        assert_eq!(detect_quota_check_url("openai", Some(""), None), None);
        assert_eq!(
            detect_quota_check_url("openai", Some("https://api.unknown.com"), None),
            None
        );
        assert_eq!(detect_quota_check_url("openai_responses", None, None), None);
    }

    /// Mirrors Go `provider_quota_url_test.go::TestGetProviderType_OpenaiWith*URLPort`:
    /// a port suffix must not break host detection.
    #[test]
    fn detect_quota_check_url_openai_handles_port_suffix() {
        assert_eq!(
            detect_quota_check_url("openai", Some("https://pass.wafer.ai:443"), None),
            Some("https://pass.wafer.ai:443/v1/inference/quota".to_string())
        );
        assert_eq!(
            detect_quota_check_url(
                "openai_responses",
                Some("https://api.synthetic.new:443"),
                None
            ),
            Some("https://api.synthetic.new:443/v2/quotas".to_string())
        );
    }

    /// Dedicated channel types ignore the URL and use the channel type alone.
    #[test]
    fn detect_quota_check_url_dedicated_channel_uses_default_template() {
        assert_eq!(
            detect_quota_check_url("codex", None, None),
            Some("https://chatgpt.com/backend-api/codex/quota".to_string())
        );
        assert_eq!(
            detect_quota_check_url("nanogpt", None, None),
            Some("https://nano-gpt.com/api/subscription/v1/usage".to_string())
        );
    }

    /// The `endpoint` override mirrors a per-checker `provider_quota_check_url`
    /// setting, replacing the provider's default check path.
    #[test]
    fn detect_quota_check_url_endpoint_overrides_default_path() {
        assert_eq!(
            detect_quota_check_url("apertis", Some("https://api.apertis.ai"), Some("/v2/quota")),
            Some("https://api.apertis.ai/v2/quota".to_string())
        );
    }

    #[test]
    fn detect_quota_check_url_returns_none_for_unsupported_channel_type() {
        assert_eq!(detect_quota_check_url("claudecode", None, None), None);
        assert_eq!(detect_quota_check_url("github_copilot", None, None), None);
        assert_eq!(detect_quota_check_url("unknown", None, None), None);
    }

    // ─── S10: status row shape + is_due_for_check ───────────────────────────

    #[test]
    fn status_row_new_initializes_ready_with_empty_quota_data() {
        let row = ProviderQuotaStatusRow::new("channel-a", "codex");
        assert_eq!(row.channel_id, "channel-a");
        assert_eq!(row.provider_type, "codex");
        assert_eq!(row.status, PROVIDER_QUOTA_STATUS_READY);
        assert!(row.ready);
        assert_eq!(row.quota_data, Value::Object(Default::default()));
        assert_eq!(row.next_check_at, None);
        assert_eq!(row.next_reset_at, None);
        assert!(!row.is_exhausted());
    }

    #[test]
    fn status_row_has_status_is_case_insensitive() {
        let mut row = ProviderQuotaStatusRow::new("channel-a", "codex");
        row.status = "Exhausted".to_string();
        assert!(row.has_status("exhausted"));
        assert!(row.has_status("EXHAUSTED"));
        assert!(row.is_exhausted());
        assert!(!row.has_status("ready"));
    }

    /// Mirrors Go `ProviderQuotaService.runQuotaCheck` (`provider_quota.go:504-513`):
    /// `Not(HasProviderQuotaStatus())` — no row -> always due.
    #[test]
    fn is_due_for_check_no_row_is_due() {
        let now = Utc::now();
        assert!(is_due_for_check(None, now, Duration::from_secs(60)));
    }

    /// Mirrors Go `NextCheckAtLTE(now)` filter arm.
    #[test]
    fn is_due_for_check_uses_next_check_at_when_present() {
        let now = Utc::now();
        let interval = Duration::from_secs(60);

        let future = now + chrono::Duration::seconds(30);
        let mut row = ProviderQuotaStatusRow::new("channel-a", "codex");
        row.next_check_at = Some(future);
        assert!(!is_due_for_check(Some(&row), now, interval));

        let past = now - chrono::Duration::seconds(30);
        row.next_check_at = Some(past);
        assert!(is_due_for_check(Some(&row), now, interval));

        row.next_check_at = Some(now);
        assert!(is_due_for_check(Some(&row), now, interval));
    }

    /// Mirrors Go `saveQuotaStatus` (`provider_quota.go:597`): rows written
    /// without `next_check_at` should fall back to `last_seen + interval`.
    #[test]
    fn is_due_for_check_falls_back_to_last_seen_plus_interval() {
        let now = DateTime::parse_from_rfc3339("2026-06-28T12:00:00Z")
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| panic!("parse failed"));
        let interval = Duration::from_secs(300);

        let mut row = ProviderQuotaStatusRow::new("channel-a", "codex");
        row.next_check_at = None;
        row.quota_data = json!({
            "last_seen": "2026-06-28T11:55:00Z"
        });
        // 11:55 + 5min = 12:00 == now -> due
        assert!(is_due_for_check(Some(&row), now, interval));

        row.quota_data = json!({
            "last_seen": "2026-06-28T11:58:00Z"
        });
        // 11:58 + 5min = 12:03 > now -> not due
        assert!(!is_due_for_check(Some(&row), now, interval));
    }

    /// A row with no schedule signal at all should be treated as due so the
    /// scheduler refreshes it (defensive default; matches Go's behavior of
    /// always writing `next_check_at` on save, so a missing value indicates a
    /// legacy row that needs a refresh).
    #[test]
    fn is_due_for_check_row_without_schedule_is_due() {
        let now = Utc::now();
        let row = ProviderQuotaStatusRow::new("channel-a", "codex");
        assert!(is_due_for_check(Some(&row), now, Duration::from_secs(60)));
    }

    // ===== S05 / S12 / S13 parity tests (Leader 2026-07-02) =====
    // Lock in the scheduling / threshold / invalidation helpers Curie-the-4th
    // ported from Go `provider_quota.go`; each test mirrors the Go branch order
    // and threshold values documented inline above the function it covers.

    // ---- S05: check_interval + warning-ratio scheduling ----

    #[test]
    fn effective_check_interval_prefers_configured_then_default() {
        // Go `getCheckInterval`: configured > 0 wins, otherwise the 5m default.
        assert_eq!(
            effective_check_interval(std::time::Duration::from_secs(10)),
            std::time::Duration::from_secs(10)
        );
        assert_eq!(
            effective_check_interval(std::time::Duration::ZERO),
            std::time::Duration::from_secs(PROVIDER_QUOTA_DEFAULT_CHECK_INTERVAL_SECS)
        );
    }

    #[test]
    fn effective_warning_check_interval_ratio_defaults_to_four_on_zero() {
        // Go `getWarningCheckInterval`: non-positive ratio falls back to 4.
        assert_eq!(effective_warning_check_interval_ratio(0), 4);
        assert_eq!(effective_warning_check_interval_ratio(7), 7);
    }

    #[test]
    fn warning_check_interval_multiplies_interval_by_ratio() {
        // 5m interval * 4 ratio == 20m.
        assert_eq!(
            warning_check_interval(std::time::Duration::from_secs(300), 4),
            std::time::Duration::from_secs(1200)
        );
        // Overflow saturates to MAX so a misconfigured ratio cannot panic.
        assert_eq!(
            warning_check_interval(std::time::Duration::MAX, 2),
            std::time::Duration::MAX
        );
    }

    #[test]
    fn next_check_interval_for_status_warning_uses_longer_interval() {
        // Go `nextCheckIntervalForStatus`: warning -> warning interval; else normal.
        let normal = std::time::Duration::from_secs(300);
        assert_eq!(
            next_check_interval_for_status("warning", normal, 4),
            std::time::Duration::from_secs(1200)
        );
        // Case-insensitive, matching Go `strings.EqualFold`.
        assert_eq!(
            next_check_interval_for_status("WARNING", normal, 4),
            std::time::Duration::from_secs(1200)
        );
        // Non-warning statuses use the plain check interval.
        assert_eq!(next_check_interval_for_status("ready", normal, 4), normal);
        assert_eq!(
            next_check_interval_for_status("exhausted", normal, 4),
            normal
        );
    }

    #[test]
    fn interval_to_cron_expr_mirrors_go_branches() {
        // Go `intervalToCronExpr` (`provider_quota.go:336-370`).
        let cases: &[(u64, &str)] = &[
            (60, "*/1 * * * *"),   // 1 min, divides 60 evenly
            (120, "*/2 * * * *"),  // 2 min
            (300, "*/5 * * * *"),  // 5 min (default check interval)
            (900, "*/15 * * * *"), // 15 min
            (3600, "0 * * * *"),   // 1 hour -> whole-hour singular
            (7200, "0 */2 * * *"), // 2 hours -> whole-hour plural
            (420, "*/6 * * * *"),  // 7 min: 60%7!=0 -> round down to 6
        ];
        for (secs, expected) in cases {
            assert_eq!(
                interval_to_cron_expr(std::time::Duration::from_secs(*secs)).as_deref(),
                Some(*expected),
                "interval_to_cron_expr({secs}s) mismatch"
            );
        }
        // Zero interval has no cron representation.
        assert_eq!(interval_to_cron_expr(std::time::Duration::ZERO), None);
    }

    #[test]
    fn status_for_usage_ratio_applies_go_thresholds() {
        // WarningThresholdRatio = 0.8; >=1.0 exhausted, >=0.8 warning, else ready.
        assert_eq!(status_for_usage_ratio(1.0), PROVIDER_QUOTA_STATUS_EXHAUSTED);
        assert_eq!(status_for_usage_ratio(1.5), PROVIDER_QUOTA_STATUS_EXHAUSTED);
        assert_eq!(status_for_usage_ratio(0.8), PROVIDER_QUOTA_STATUS_WARNING);
        assert_eq!(status_for_usage_ratio(0.79), PROVIDER_QUOTA_STATUS_READY);
        assert_eq!(status_for_usage_ratio(0.0), PROVIDER_QUOTA_STATUS_READY);
    }

    #[test]
    fn is_ready_status_mirrors_go() {
        assert!(is_ready_status(PROVIDER_QUOTA_STATUS_READY));
        assert!(is_ready_status(PROVIDER_QUOTA_STATUS_WARNING));
        assert!(!is_ready_status(PROVIDER_QUOTA_STATUS_EXHAUSTED));
        assert!(!is_ready_status("unknown"));
    }

    #[test]
    fn quota_status_rank_orders_by_severity() {
        // Go `quotaStatusRank`: ready=0, warning=1, exhausted=2, unknown=-1.
        assert_eq!(quota_status_rank(PROVIDER_QUOTA_STATUS_READY), 0);
        assert_eq!(quota_status_rank(PROVIDER_QUOTA_STATUS_WARNING), 1);
        assert_eq!(quota_status_rank(PROVIDER_QUOTA_STATUS_EXHAUSTED), 2);
        assert_eq!(quota_status_rank("unknown"), -1);
        assert_eq!(quota_status_rank("nonsense"), -1);
    }

    #[test]
    fn worst_limit_status_picks_most_severe_or_none() {
        // Empty -> None (Go: no matching limit type -> unknown).
        let empty: [&str; 0] = [];
        assert_eq!(worst_limit_status(empty.iter()), None);

        let severe = ["ready", "warning", "exhausted"];
        assert_eq!(
            worst_limit_status(severe.iter()),
            Some(PROVIDER_QUOTA_STATUS_EXHAUSTED)
        );
        let mild = ["ready", "warning"];
        assert_eq!(
            worst_limit_status(mild.iter()),
            Some(PROVIDER_QUOTA_STATUS_WARNING)
        );
    }

    // ===== S16: effective_limit_status (mirrors quota_channel_status_test.go) =====
    // Mirror Go `QuotaChannelStatus.EffectiveStatus(limitType)`
    // (`provider_quota.go:39-81`) — the per-limit-type-aware aggregation that
    // `worst_limit_status` does NOT cover (no type filtering, no channel-level
    // short-circuit, no ready-flag AND, no unknown-fallback). Each test below
    // mirrors a Go test case from `quota_channel_status_test.go` (L13-169).

    /// Mirrors Go `TestQuotaChannelStatus_EffectiveStatus_NoLimits`
    /// (`quota_channel_status_test.go:13-23`): when `limits` is empty, the
    /// channel-level `(status, ready)` pair is returned as-is (no per-limit
    /// data to consult).
    #[test]
    fn effective_limit_status_no_limits_returns_channel_status() {
        let (status, ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_WARNING,
            true,
            &[],
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(status, PROVIDER_QUOTA_STATUS_WARNING);
        assert!(ready);
    }

    /// Mirrors Go `TestQuotaChannelStatus_EffectiveStatus_ImageExhausted_TokenAvailable`
    /// (`quota_channel_status_test.go:25-42`): per-limit-type filtering — image
    /// limit is exhausted while token limit is available; querying each type
    /// returns that type's status independently.
    #[test]
    fn effective_limit_status_filters_by_limit_type() {
        let limits = vec![
            cache_limit("image", "exhausted", 1.0, false, None),
            cache_limit("token", "available", 0.3, true, None),
        ];

        let (img_status, img_ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_WARNING,
            true,
            &limits,
            &CacheQuotaLimitType::Image,
        );
        assert_eq!(img_status, PROVIDER_QUOTA_STATUS_EXHAUSTED);
        assert!(!img_ready);

        let (tkn_status, tkn_ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_WARNING,
            true,
            &limits,
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(tkn_status, "available");
        assert!(tkn_ready);
    }

    /// Mirrors Go `TestQuotaChannelStatus_EffectiveStatus_ImageWarning_DoesNotAffectTokens`
    /// (`quota_channel_status_test.go:44-59`): an image warning must not
    /// contaminate the token status query.
    #[test]
    fn effective_limit_status_image_warning_does_not_affect_tokens() {
        let limits = vec![
            cache_limit("image", "warning", 0.9, true, None),
            cache_limit("token", "available", 0.3, true, None),
        ];

        let (img_status, _) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_WARNING,
            true,
            &limits,
            &CacheQuotaLimitType::Image,
        );
        assert_eq!(img_status, PROVIDER_QUOTA_STATUS_WARNING);

        let (tkn_status, _) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_WARNING,
            true,
            &limits,
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(tkn_status, "available");
    }

    /// Mirrors Go `TestQuotaChannelStatus_EffectiveStatus_MultipleTokenLimits_WorstWins`
    /// (`quota_channel_status_test.go:61-74`): when multiple limits share the
    /// queried type, the worst-status one wins.
    #[test]
    fn effective_limit_status_multiple_same_type_worst_wins() {
        let limits = vec![
            cache_limit("token", "available", 0.3, true, None),
            cache_limit("token", "warning", 0.85, true, None),
        ];

        let (status, ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_WARNING,
            true,
            &limits,
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(status, PROVIDER_QUOTA_STATUS_WARNING);
        assert!(ready);
    }

    /// Mirrors Go `TestQuotaChannelStatus_EffectiveStatus_NoMatchingLimit_Fallback`
    /// (`quota_channel_status_test.go:76-88`): when no limit matches the queried
    /// type, return `(unknown, true)` so missing data does not block routing.
    #[test]
    fn effective_limit_status_no_matching_type_returns_unknown_ready() {
        let limits = vec![cache_limit("image", "exhausted", 1.0, false, None)];

        let (status, ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_READY,
            true,
            &limits,
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(status, PROVIDER_QUOTA_STATUS_UNKNOWN);
        assert!(ready);
    }

    /// Mirrors Go `TestQuotaChannelStatus_EffectiveStatus_BothExhausted`
    /// (`quota_channel_status_test.go:90-107`): channel-level exhausted
    /// short-circuit — even though per-limit data also says exhausted, the
    /// short-circuit returns `(exhausted, false)` for every limit type.
    #[test]
    fn effective_limit_status_channel_exhausted_short_circuits_both_types() {
        let limits = vec![
            cache_limit("image", "exhausted", 1.0, false, None),
            cache_limit("token", "exhausted", 1.0, false, None),
        ];

        let (img_status, img_ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_EXHAUSTED,
            false,
            &limits,
            &CacheQuotaLimitType::Image,
        );
        assert_eq!(img_status, PROVIDER_QUOTA_STATUS_EXHAUSTED);
        assert!(!img_ready);

        let (tkn_status, tkn_ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_EXHAUSTED,
            false,
            &limits,
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(tkn_status, PROVIDER_QUOTA_STATUS_EXHAUSTED);
        assert!(!tkn_ready);
    }

    /// Mirrors Go `TestQuotaChannelStatus_EffectiveStatus_AllLimitsUnknown`
    /// (`quota_channel_status_test.go:109-126`): when all matching limits have
    /// `unknown` status (rank=-1), the aggregate is `unknown` with `ready=false`
    /// (AND of all ready flags — all false).
    #[test]
    fn effective_limit_status_all_unknown_returns_unknown_not_ready() {
        let limits = vec![
            cache_limit("token", "unknown", 0.0, false, None),
            cache_limit("image", "unknown", 0.0, false, None),
        ];

        let (tkn_status, tkn_ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_READY,
            true,
            &limits,
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(tkn_status, PROVIDER_QUOTA_STATUS_UNKNOWN);
        assert!(!tkn_ready);

        let (img_status, img_ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_READY,
            true,
            &limits,
            &CacheQuotaLimitType::Image,
        );
        assert_eq!(img_status, PROVIDER_QUOTA_STATUS_UNKNOWN);
        assert!(!img_ready);
    }

    /// Mirrors Go `TestEffectiveStatus_ChannelExhaustedOverridesPerLimitAvailable`
    /// (`quota_channel_status_test.go:128-140`): channel-level exhausted
    /// short-circuits even when per-limit data says `available` — a channel
    /// marked exhausted at the top level is treated as fully unavailable.
    #[test]
    fn effective_limit_status_channel_exhausted_overrides_per_limit_available() {
        let limits = vec![cache_limit("token", "available", 0.3, true, None)];

        let (status, ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_EXHAUSTED,
            false,
            &limits,
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(status, PROVIDER_QUOTA_STATUS_EXHAUSTED);
        assert!(!ready);
    }

    /// Mirrors Go `TestEffectiveStatus_UnknownFallbackWhenNoMatchingLimitType`
    /// (`quota_channel_status_test.go:142-154`): same as
    /// `effective_limit_status_no_matching_type_returns_unknown_ready` but with
    /// a different channel-level status (`warning` + ready=true) to confirm the
    /// fallback is independent of the channel-level pair.
    #[test]
    fn effective_limit_status_unknown_fallback_independent_of_channel_status() {
        let limits = vec![cache_limit("image", "exhausted", 1.0, false, None)];

        let (status, ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_WARNING,
            true,
            &limits,
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(status, PROVIDER_QUOTA_STATUS_UNKNOWN);
        assert!(ready);
    }

    /// Mirrors Go `TestEffectiveStatus_EqualRankReadyAggregation`
    /// (`quota_channel_status_test.go:156-169`): when two matching limits share
    /// the same status rank, `ready` is the AND of both limits' ready flags —
    /// `true && false = false`.
    #[test]
    fn effective_limit_status_equal_rank_ands_ready_flags() {
        let limits = vec![
            cache_limit("token", "warning", 0.85, true, None),
            cache_limit("token", "warning", 0.90, false, None),
        ];

        let (status, ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_WARNING,
            true,
            &limits,
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(status, PROVIDER_QUOTA_STATUS_WARNING);
        assert!(!ready);
    }

    /// Mirrors Go `TestProviderQuotaService_UpdateQuotaCache_WithLimits`
    /// (`provider_quota_cache_test.go:149-184`) EffectiveStatus assertions:
    /// after merging limits into `quota_data` and extracting them back, the
    /// `effective_limit_status` for each limit type must match the per-limit
    /// status that was stored. This combines the S14 merge/extract roundtrip
    /// with the S16 `effective_limit_status` aggregation.
    #[test]
    fn effective_limit_status_after_merge_extract_roundtrip_matches_go() {
        let limits = vec![
            cache_limit("token", "available", 0.3, true, None),
            cache_limit("image", "exhausted", 1.0, false, None),
        ];

        // Merge into quota_data, then extract back — mirrors Go's
        // updateQuotaCache → GetQuotaStatus → EffectiveStatus flow.
        let merged = merge_limits_into_quota_data(&json!({}), &limits);
        let extracted = extract_limits_from_quota_data(&merged);
        assert_eq!(extracted.len(), 2);

        let (img_status, img_ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_WARNING,
            true,
            &extracted,
            &CacheQuotaLimitType::Image,
        );
        assert_eq!(img_status, PROVIDER_QUOTA_STATUS_EXHAUSTED);
        assert!(!img_ready);

        let (tkn_status, tkn_ready) = effective_limit_status(
            PROVIDER_QUOTA_STATUS_WARNING,
            true,
            &extracted,
            &CacheQuotaLimitType::Token,
        );
        assert_eq!(tkn_status, "available");
        assert!(tkn_ready);
    }

    // ---- S12: independent quota-checker HTTP timeout ----

    #[test]
    fn checker_http_timeout_is_independent_ten_seconds() {
        // S12: the quota checker uses its OWN 10s timeout, distinct from the
        // shared LLM request timeout, so a slow quota endpoint can't eat the
        // LLM budget. Lock the constant so it can't drift silently.
        assert_eq!(PROVIDER_QUOTA_CHECKER_HTTP_TIMEOUT_SECS, 10);
        assert_eq!(
            provider_quota_checker_http_timeout(),
            std::time::Duration::from_secs(10)
        );
    }

    // ---- S13: cache-invalidation -> subscriber fan-out ----

    /// Test-only subscriber that records every invalidation it receives.
    struct RecordingSubscriber {
        received: std::sync::Mutex<Vec<ProviderQuotaCacheInvalidation>>,
    }

    impl RecordingSubscriber {
        fn snapshot(&self) -> Vec<ProviderQuotaCacheInvalidation> {
            self.received
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        }
    }

    impl QuotaCacheInvalidationSubscriber for RecordingSubscriber {
        fn on_quota_cache_invalidated(&self, invalidation: &ProviderQuotaCacheInvalidation) {
            let mut guard = self.received.lock().unwrap_or_else(|p| p.into_inner());
            guard.push(invalidation.clone());
        }
    }

    #[test]
    fn invalidation_notifier_fans_out_to_subscribers() {
        let notifier = QuotaCacheInvalidationNotifier::new();
        assert_eq!(notifier.subscriber_count(), 0);

        let sub = std::sync::Arc::new(RecordingSubscriber {
            received: std::sync::Mutex::new(Vec::new()),
        });
        notifier.subscribe(sub.clone());
        assert_eq!(notifier.subscriber_count(), 1);

        let inv = ProviderQuotaCacheInvalidation::new("channel-7", "codex");
        notifier.notify(&inv);

        let got = sub.snapshot();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], inv);
        assert_eq!(got[0].channel_id, "channel-7");
    }

    #[test]
    fn invalidation_notifier_subscribe_is_idempotent_for_same_arc() {
        // Pointer-equal Arc subscribed twice must not duplicate.
        let notifier = QuotaCacheInvalidationNotifier::new();
        let sub = std::sync::Arc::new(RecordingSubscriber {
            received: std::sync::Mutex::new(Vec::new()),
        });
        notifier.subscribe(sub.clone());
        notifier.subscribe(sub.clone());
        assert_eq!(notifier.subscriber_count(), 1);
    }

    // ===== S14: limits <-> quota_data JSON roundtrip =====
    // Mirror Go `provider_quota_cache_test.go::TestMergeAndExtractLimitsRoundTrip`
    // (L186-253) and `TestProviderQuotaService_UpdateQuotaCache_WithLimits`
    // (L149-184). The Go semantics are:
    //   - `mergeLimitsIntoQuotaData` writes raw data + `_limits` array.
    //   - `extractLimitsFromQuotaData` reads `_limits` back, tolerating either
    //     typed-array or `[]any` shapes (Go's `[]map[string]any` vs `[]any`).
    //   - Empty `limits` writes no `_limits` key at all.

    fn cache_limit(
        limit_type: &str,
        status: &str,
        usage_ratio: f64,
        ready: bool,
        next_reset_at: Option<DateTime<Utc>>,
    ) -> CacheQuotaLimitStatus {
        CacheQuotaLimitStatus::new(
            CacheQuotaLimitType::from_str_ci(limit_type),
            status,
            usage_ratio,
            ready,
            next_reset_at,
        )
    }

    /// Mirrors Go `TestMergeAndExtractLimitsRoundTrip` subtest `"basic round trip"`
    /// (`provider_quota_cache_test.go:190-225`): two limits (one without
    /// `next_reset_at`, one with) survive a merge → extract cycle with every
    /// field intact, and the raw-data side channel is preserved.
    #[test]
    fn merge_then_extract_round_trips_token_and_image_limits() {
        let reset_at: DateTime<Utc> = DateTime::parse_from_rfc3339("2025-01-15T10:30:00Z")
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| panic!("parse failed"));
        let raw_data = json!({"key": "value"});

        let limits = vec![
            cache_limit("token", "available", 0.3, true, None),
            cache_limit("image", "exhausted", 1.0, false, Some(reset_at)),
        ];

        let merged = merge_limits_into_quota_data(&raw_data, &limits);
        let extracted = extract_limits_from_quota_data(&merged);

        assert_eq!(extracted.len(), 2);

        let token = extracted
            .iter()
            .find(|l| l.limit_type == CacheQuotaLimitType::Token)
            .unwrap_or_else(|| panic!("missing token limit"));
        assert_eq!(token.status, "available");
        assert!((token.usage_ratio - 0.3).abs() < 1e-3);
        assert!(token.ready);
        assert_eq!(token.next_reset_at, None);

        let image = extracted
            .iter()
            .find(|l| l.limit_type == CacheQuotaLimitType::Image)
            .unwrap_or_else(|| panic!("missing image limit"));
        assert_eq!(image.status, "exhausted");
        assert!((image.usage_ratio - 1.0).abs() < 1e-3);
        assert!(!image.ready);
        assert_eq!(image.next_reset_at, Some(reset_at));

        // Raw-data side channel preserved alongside `_limits`.
        assert_eq!(merged.get("key").and_then(Value::as_str), Some("value"));
    }

    /// Mirrors Go `TestMergeAndExtractLimitsRoundTrip` subtest `"empty limits"`
    /// (`provider_quota_cache_test.go:227-237`): when `limits` is empty, no
    /// `_limits` key is written at all (Go: `if len(quotaData.Limits) > 0`
    /// guard), and extraction returns an empty slice.
    #[test]
    fn merge_with_empty_limits_omits_limits_key_and_extracts_empty() {
        let raw_data = json!({"status": "available"});
        let merged = merge_limits_into_quota_data(&raw_data, &[]);
        assert!(merged.get(QUOTA_DATA_LIMITS_KEY).is_none());
        assert!(extract_limits_from_quota_data(&merged).is_empty());
    }

    /// Mirrors Go `TestMergeAndExtractLimitsRoundTrip` subtest
    /// `"preserves raw data"` (`provider_quota_cache_test.go:239-252`):
    /// existing raw-data entries are kept, and `_limits` is added alongside.
    #[test]
    fn merge_preserves_existing_raw_data_besides_limits() {
        let raw_data = json!({"existing": "data"});
        let limits = vec![cache_limit("token", "available", 0.5, true, None)];
        let merged = merge_limits_into_quota_data(&raw_data, &limits);

        assert_eq!(merged.get("existing").and_then(Value::as_str), Some("data"));
        assert!(merged.get(QUOTA_DATA_LIMITS_KEY).is_some());
    }

    /// Mirrors Go `extractLimitsFromQuotaData` tolerance for the
    /// `[]any`-post-JSON-roundtrip shape (`provider_quota.go:755-765`). When
    /// the `_limits` array is parsed back from JSON it loses Go's
    /// `[]map[string]any` type and becomes `[]any`; the Rust port's "accept
    /// `Value::Array` of `Value::Object`" branch covers the same case.
    #[test]
    fn extract_handles_array_of_objects_built_directly_from_json() {
        let data = json!({
            "_limits": [
                {"type": "token", "status": "warning", "usageRatio": 0.85, "ready": true},
                {"type": "image", "status": "exhausted", "usageRatio": 1.0, "ready": false}
            ]
        });
        let extracted = extract_limits_from_quota_data(&data);
        assert_eq!(extracted.len(), 2);
        assert_eq!(extracted[0].limit_type, CacheQuotaLimitType::Token);
        assert!((extracted[0].usage_ratio - 0.85).abs() < 1e-3);
        assert_eq!(extracted[1].limit_type, CacheQuotaLimitType::Image);
        assert!(!extracted[1].ready);
    }

    /// Mirrors Go `extractLimitsFromQuotaData` missing-key branch
    /// (`provider_quota.go:747-750`): returns empty when `_limits` absent.
    #[test]
    fn extract_returns_empty_when_quota_data_lacks_limits_key() {
        let data = json!({"other": "field"});
        assert!(extract_limits_from_quota_data(&data).is_empty());
        // Also tolerant of a non-array `_limits` value.
        let bad = json!({"_limits": "not an array"});
        assert!(extract_limits_from_quota_data(&bad).is_empty());
    }

    /// Unknown limit-type tokens round-trip losslessly through the
    /// `Other(String)` arm (mirrors Go's tolerant `QuotaLimitType(s)` cast,
    /// which never discards data).
    #[test]
    fn merge_extract_round_trips_unknown_limit_type_token() {
        let limits = vec![cache_limit("custom_limit", "available", 0.1, true, None)];
        let merged = merge_limits_into_quota_data(&json!({}), &limits);
        let extracted = extract_limits_from_quota_data(&merged);

        let l = extracted
            .first()
            .unwrap_or_else(|| panic!("expected one limit"));
        match &l.limit_type {
            CacheQuotaLimitType::Other(raw) => assert_eq!(raw, "custom_limit"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    // ===== S15: `hasCredentialsForProvider` gate =====
    // Mirror Go `provider_quota_url_test.go::TestHasCredentialsForProvider_*`
    // (L104-222). The Go rules (provider_quota.go:707-721) are:
    //   1. OpenAI-compatible channel types whose base URL resolves to a
    //      URL-detected Pattern-B provider (wafer/synthetic/neuralwatt/apertis)
    //      accept ONLY plain API keys — OAuth tokens are ignored.
    //   2. `codex` / `claudecode` accept ONLY OAuth (struct field OR legacy
    //      api_key OAuth-JSON blob); a plain api_key string is rejected.
    //   3. Every other channel type accepts any credential flavor.

    fn creds_with_api_key(
        api_key: &str,
    ) -> conduit_core::objects::channel_settings::ChannelCredentials {
        let mut c = conduit_core::objects::channel_settings::ChannelCredentials::default();
        c.api_key = api_key.to_string();
        c
    }

    fn creds_with_api_keys(
        keys: &[&str],
    ) -> conduit_core::objects::channel_settings::ChannelCredentials {
        let mut c = conduit_core::objects::channel_settings::ChannelCredentials::default();
        c.api_keys = keys.iter().map(|s| s.to_string()).collect();
        c
    }

    fn creds_with_oauth_access_token(
        token: &str,
    ) -> conduit_core::objects::channel_settings::ChannelCredentials {
        let mut c = conduit_core::objects::channel_settings::ChannelCredentials::default();
        c.oauth = Some(json!({"access_token": token}));
        c
    }

    fn creds_with_oauth_json_api_key(
        blob: &str,
    ) -> conduit_core::objects::channel_settings::ChannelCredentials {
        let mut c = conduit_core::objects::channel_settings::ChannelCredentials::default();
        c.api_key = blob.to_string();
        c
    }

    fn view<'a>(
        channel_type: &'a str,
        base_url: &'a str,
        creds: &'a conduit_core::objects::channel_settings::ChannelCredentials,
    ) -> ChannelCredentialView<'a> {
        ChannelCredentialView {
            channel_type,
            base_url,
            credentials: creds,
        }
    }

    /// Mirrors Go `TestHasCredentialsForProvider_WaferAPIKey`
    /// (`provider_quota_url_test.go:104-113`): wafer (URL-detected on OpenAI
    /// channel type) accepts an api_key.
    #[test]
    fn has_credentials_for_provider_wafer_accepts_api_key() {
        let creds = creds_with_api_key("sk-test");
        assert!(has_credentials_for_provider(&view(
            "openai",
            "https://wafer.ai",
            &creds
        )));
    }

    /// Mirrors Go `TestHasCredentialsForProvider_WaferNoKey`
    /// (`provider_quota_url_test.go:115-122`).
    #[test]
    fn has_credentials_for_provider_wafer_rejects_empty_creds() {
        let creds = conduit_core::objects::channel_settings::ChannelCredentials::default();
        assert!(!has_credentials_for_provider(&view(
            "openai",
            "https://wafer.ai",
            &creds
        )));
    }

    /// Mirrors Go `TestHasCredentialsForProvider_WaferOAuthIgnored`
    /// (`provider_quota_url_test.go:124-133`): OAuth alone does NOT satisfy
    /// the URL-detected-provider gate (only plain API keys do).
    #[test]
    fn has_credentials_for_provider_wafer_ignores_oauth_token() {
        let creds = creds_with_oauth_access_token("token");
        assert!(!has_credentials_for_provider(&view(
            "openai",
            "https://wafer.ai",
            &creds
        )));
    }

    /// Mirrors Go `TestHasCredentialsForProvider_SyntheticAPIKeys`
    /// (`provider_quota_url_test.go:135-144`): the `api_keys` list also
    /// satisfies the URL-detected-provider gate.
    #[test]
    fn has_credentials_for_provider_synthetic_accepts_api_keys_list() {
        let creds = creds_with_api_keys(&["sk-test"]);
        assert!(has_credentials_for_provider(&view(
            "openai",
            "https://api.synthetic.new",
            &creds
        )));
    }

    /// Mirrors Go `TestHasCredentialsForProvider_CodexWithOAuth` /
    /// `_CodexWithOAuthJSON` / `_CodexWithPlainAPIKey`
    /// (`provider_quota_url_test.go:164-192`): codex is OAuth-only — both
    /// OAuth field and OAuth-JSON api_key blob satisfy the gate, but a plain
    /// api_key string is rejected.
    #[test]
    fn has_credentials_for_provider_codex_accepts_oauth_only() {
        let oauth_field = creds_with_oauth_access_token("token");
        assert!(has_credentials_for_provider(&view(
            "codex",
            "https://chatgpt.com",
            &oauth_field
        )));

        let oauth_json = creds_with_oauth_json_api_key(
            r#"{"access_token": "token", "refresh_token": "refresh"}"#,
        );
        assert!(has_credentials_for_provider(&view(
            "codex",
            "https://chatgpt.com",
            &oauth_json
        )));

        // Plain api_key string is rejected for codex.
        let plain = creds_with_api_key("sk-plain-api-key");
        assert!(!has_credentials_for_provider(&view(
            "codex",
            "https://chatgpt.com",
            &plain
        )));
    }

    /// Mirrors Go `TestHasCredentialsForProvider_ClaudeCodeWithOAuth` /
    /// `_ClaudeCodeWithOAuthJSON` / `_ClaudeCodeWithPlainAPIKey`
    /// (`provider_quota_url_test.go:194-222`): claudecode follows the same
    /// OAuth-only rule as codex.
    #[test]
    fn has_credentials_for_provider_claudecode_accepts_oauth_only() {
        let oauth_field = creds_with_oauth_access_token("token");
        assert!(has_credentials_for_provider(&view(
            "claudecode",
            "https://api.anthropic.com",
            &oauth_field
        )));

        let oauth_json = creds_with_oauth_json_api_key(
            r#"{"access_token": "token", "refresh_token": "refresh"}"#,
        );
        assert!(has_credentials_for_provider(&view(
            "claudecode",
            "https://api.anthropic.com",
            &oauth_json
        )));

        let plain = creds_with_api_key("sk-plain-api-key");
        assert!(!has_credentials_for_provider(&view(
            "claudecode",
            "https://api.anthropic.com",
            &plain
        )));
    }

    /// Mirrors Go `TestHasCredentialsForProvider_NonOpenaiWithOAuth` /
    /// `_NonOpenaiNoCreds` (`provider_quota_url_test.go:146-162`): every other
    /// channel type accepts any credential flavor.
    #[test]
    fn has_credentials_for_provider_other_channel_types_accept_any_credential() {
        // OAuth field satisfies branch 3.
        let oauth = creds_with_oauth_access_token("token");
        assert!(has_credentials_for_provider(&view(
            "github_copilot",
            "",
            &oauth
        )));

        // Plain api_key satisfies branch 3.
        let plain = creds_with_api_key("sk-test");
        assert!(has_credentials_for_provider(&view(
            "github_copilot",
            "",
            &plain
        )));

        // Empty creds always fail.
        let empty = conduit_core::objects::channel_settings::ChannelCredentials::default();
        assert!(!has_credentials_for_provider(&view(
            "github_copilot",
            "",
            &empty
        )));
    }

    /// Mirrors Go `TestGetProviderType_OpenaiWithUnknownURL`
    /// (`provider_quota_url_test.go:27-38`): OpenAI channel whose base URL
    /// doesn't resolve to a URL-detected provider falls through to the
    /// "any-credential" branch — OAuth, api_key, or api_keys all satisfy the
    /// gate (Go: branch 3 in `hasCredentialsForProvider`).
    #[test]
    fn has_credentials_for_provider_openai_unknown_host_falls_through_to_any() {
        let oauth = creds_with_oauth_access_token("token");
        assert!(has_credentials_for_provider(&view(
            "openai",
            "https://api.unknown.com",
            &oauth
        )));
        let plain = creds_with_api_key("sk-test");
        assert!(has_credentials_for_provider(&view(
            "openai",
            "https://api.unknown.com",
            &plain
        )));
    }

    // ===== Cache layer: multi-channel storage + overwrite + concurrency =====
    // Mirror Go `provider_quota_cache_test.go::TestProviderQuotaService_GetQuotaStatus_ReturnsCorrectData`
    // (L14-37), `_UnknownChannel` (L39-46), `_UpdateQuotaCache_Overwrite`
    // (L67-79), `_ConcurrentAccess` (L81-113) and `_ConcurrentReadWrite`
    // (L115-147). Go's `quotaCache` is a `sync.Map`; the Rust port holds the
    // same shape behind an `Arc<Mutex<BTreeMap>>` inside
    // `InMemoryProviderQuotaStatusRepo`. The Mutex serializes writes, but the
    // observable contract — every writer's value is durable, every reader
    // sees a consistent snapshot — is the same and worth pinning with tests.

    /// Mirrors Go `TestProviderQuotaService_GetQuotaStatus_ReturnsCorrectData`
    /// (`provider_quota_cache_test.go:14-37`) plus `_UnknownChannel`
    /// (`:39-46`): three channels with distinct (status, ready) pairs
    /// round-trip through the in-memory repo, and an unknown channel returns
    /// `None`.
    #[tokio::test]
    async fn cache_repo_stores_and_reads_multiple_channels_with_distinct_statuses()
    -> ProviderQuotaServiceResult<()> {
        let repo = Arc::new(InMemoryProviderQuotaStatusRepo::new());
        let service = ProviderQuotaService::new(repo.clone());
        let ctx = ctx();

        let statuses = [
            ("channel-1", PROVIDER_QUOTA_STATUS_READY, true),
            ("channel-2", PROVIDER_QUOTA_STATUS_EXHAUSTED, false),
            ("channel-3", PROVIDER_QUOTA_STATUS_WARNING, true),
        ];

        for (id, status, ready) in statuses {
            let s = ProviderQuotaStatus {
                status: status.to_string(),
                ready,
                ..ProviderQuotaStatus::new(id, "codex")
            };
            service.set_status(&ctx, s).await?;
        }

        for (id, status, ready) in statuses {
            let fetched = service.get_status(&ctx, id).await?;
            assert_eq!(
                fetched.map(|s| (s.status, s.ready)),
                Some((status.to_string(), ready)),
                "channel {id} mismatch"
            );
        }

        // Unknown channel returns None (mirrors Go nil return).
        assert!(service.get_status(&ctx, "channel-999").await?.is_none());
        Ok(())
    }

    /// Mirrors Go `TestProviderQuotaService_UpdateQuotaCache_Overwrite`
    /// (`provider_quota_cache_test.go:67-79`): writing the same channel twice
    /// keeps only the latest snapshot.
    #[tokio::test]
    async fn cache_repo_latest_write_overwrites_previous_status() -> ProviderQuotaServiceResult<()>
    {
        let service = ProviderQuotaService::new(Arc::new(InMemoryProviderQuotaStatusRepo::new()));
        let ctx = ctx();

        let first = ProviderQuotaStatus {
            status: PROVIDER_QUOTA_STATUS_READY.to_string(),
            ready: true,
            ..ProviderQuotaStatus::new("channel-1", "codex")
        };
        service.set_status(&ctx, first).await?;

        let second = ProviderQuotaStatus {
            status: PROVIDER_QUOTA_STATUS_EXHAUSTED.to_string(),
            ready: false,
            ..ProviderQuotaStatus::new("channel-1", "codex")
        };
        service.set_status(&ctx, second).await?;

        let fetched = service.get_status(&ctx, "channel-1").await?;
        assert_eq!(
            fetched.map(|s| (s.status, s.ready)),
            Some((PROVIDER_QUOTA_STATUS_EXHAUSTED.to_string(), false))
        );
        Ok(())
    }

    /// Mirrors Go `TestProviderQuotaService_ConcurrentAccess`
    /// (`provider_quota_cache_test.go:81-113`): N writers concurrently
    /// populate N distinct channels, then every channel's status must be
    /// readable and correct. The Rust `Arc<Mutex<BTreeMap>>` serializes the
    /// writers, but the test still pins the observable "no lost writes"
    /// contract.
    #[tokio::test]
    async fn cache_repo_concurrent_writes_to_distinct_channels_preserve_all()
    -> ProviderQuotaServiceResult<()> {
        let service = Arc::new(ProviderQuotaService::new(Arc::new(
            InMemoryProviderQuotaStatusRepo::new(),
        )));
        let ctx = Arc::new(ctx());
        const N: usize = 50;

        let mut handles = Vec::new();
        for i in 0..N {
            let svc = service.clone();
            let ctx = ctx.clone();
            handles.push(tokio::spawn(async move {
                let id = format!("channel-{i}");
                let status = ProviderQuotaStatus {
                    status: PROVIDER_QUOTA_STATUS_READY.to_string(),
                    ready: true,
                    ..ProviderQuotaStatus::new(id.clone(), "codex")
                };
                svc.set_status(&ctx, status).await
            }));
        }
        for handle in handles {
            // Flatten `Result<Result<(), ServiceError>, JoinError>` into a
            // single error so a panic'd spawn or a failed set_status both
            // surface as test failures instead of being silently dropped.
            let inner = handle
                .await
                .map_err(|_e| ProviderQuotaServiceError::LockPoisoned)?;
            inner?;
        }

        for i in 0..N {
            let id = format!("channel-{i}");
            let fetched = service.get_status(&ctx, &id).await?;
            assert_eq!(
                fetched.map(|s| (s.status, s.ready)),
                Some((PROVIDER_QUOTA_STATUS_READY.to_string(), true)),
                "channel {id} lost its write"
            );
        }
        Ok(())
    }

    /// Mirrors Go `TestProviderQuotaService_ConcurrentReadWrite`
    /// (`provider_quota_cache_test.go:115-147`): N writers and N readers
    /// hammer the SAME channel concurrently. After all writers finish, the
    /// channel must reflect the last write exactly; readers throughout never
    /// observe a torn value (the Mutex guarantees atomic snapshots).
    #[tokio::test]
    async fn cache_repo_concurrent_reads_and_writes_on_same_channel_are_atomic()
    -> ProviderQuotaServiceResult<()> {
        let service = Arc::new(ProviderQuotaService::new(Arc::new(
            InMemoryProviderQuotaStatusRepo::new(),
        )));
        let ctx = Arc::new(ctx());

        // Seed with available.
        let seed = ProviderQuotaStatus {
            status: PROVIDER_QUOTA_STATUS_READY.to_string(),
            ready: true,
            ..ProviderQuotaStatus::new("channel-1", "codex")
        };
        service.set_status(&ctx, seed).await?;

        const ITERS: usize = 50;
        let mut handles = Vec::new();

        // Writers flip to exhausted.
        for _ in 0..ITERS {
            let svc = service.clone();
            let ctx = ctx.clone();
            handles.push(tokio::spawn(async move {
                let s = ProviderQuotaStatus {
                    status: PROVIDER_QUOTA_STATUS_EXHAUSTED.to_string(),
                    ready: false,
                    ..ProviderQuotaStatus::new("channel-1", "codex")
                };
                let _ = svc.set_status(&ctx, s).await;
            }));
        }
        // Readers just observe.
        for _ in 0..ITERS {
            let svc = service.clone();
            let ctx = ctx.clone();
            handles.push(tokio::spawn(async move {
                let _ = svc.get_status(&ctx, "channel-1").await;
            }));
        }

        for handle in handles {
            handle
                .await
                .map_err(|_e| ProviderQuotaServiceError::LockPoisoned)?;
        }

        let fetched = service.get_status(&ctx, "channel-1").await?;
        assert_eq!(
            fetched.map(|s| (s.status, s.ready)),
            Some((PROVIDER_QUOTA_STATUS_EXHAUSTED.to_string(), false))
        );
        Ok(())
    }
}
