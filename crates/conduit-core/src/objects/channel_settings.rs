//! Channel-settings objects ported from the bulk of
//! `conduit/internal/objects/channel.go`.
//!
//! Covers the settings subtree rooted at [`ChannelSettings`] together with its
//! dependent types: [`ChannelEndpoint`], [`ModelMapping`], [`HeaderEntry`],
//! [`OverrideMatch`] (re-exported from [`crate::objects::overrides`]),
//! [`TransformOptions`], [`RetryableErrorPattern`], [`ChannelRateLimit`],
//! [`DisabledAPIKey`], [`ChannelCredentials`] (with its
//! [`ChannelCredentials::get_all_api_keys`] /
//! [`ChannelCredentials::get_enabled_api_keys`] /
//! [`ChannelCredentials::is_oauth`] methods), [`AzureCredential`],
//! [`GCPCredential`], [`GCPCredentialsJSON`], [`CapabilityPolicy`] and
//! [`ChannelPolicies`].
//!
//! All field names, JSON tags, and `omitempty` semantics mirror the Go source
//! exactly. Pointer fields become `Option<T>` with
//! `skip_serializing_if = "Option::is_none"`.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

// Re-export the override types that the Go source co-locates with channel
// settings; they live in [`crate::objects::overrides`] for a clean split.
pub use crate::objects::overrides::{OverrideMatch, OverrideOperation};

/// An outbound API endpoint configuration within a channel. Ported 1:1 from Go
/// `ChannelEndpoint`.
///
/// Within a single channel `api_format` must be unique; `path` / `base_url` /
/// `transport` are optional overrides. All four JSON tags are snake_case in Go
/// (e.g. `api_format`, `base_url`), so this struct does **not** use
/// `rename_all = "camelCase"`; the Rust snake_case field names match the tags
/// directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ChannelEndpoint {
    /// Upstream API format identifier (must be unique within the channel).
    #[serde(default)]
    pub api_format: String,
    /// Optional custom path override. Omitted on the wire when empty
    /// (Go `omitempty`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Optional custom base URL override. Omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    /// Optional transport (`"http"` or `"websocket"`). Omitted when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub transport: String,
}

/// Known values of [`ChannelEndpoint::transport`]. Mirrors the Go
/// `ChannelEndpointTransport*` consts.
pub mod channel_endpoint_transport {
    /// `ChannelEndpointTransportHTTP = "http"`.
    pub const HTTP: &str = "http";
    /// `ChannelEndpointTransportWebSocket = "websocket"`.
    pub const WEBSOCKET: &str = "websocket";
}

/// A single model alias entry (`from` -> `to`). Ported 1:1 from Go
/// `ModelMapping`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelMapping {
    /// Model name as it appears in the request.
    #[serde(default)]
    pub from: String,
    /// Actual model name in the provider.
    #[serde(default)]
    pub to: String,
}

/// A single header key/value pair, used by the legacy `overrideHeaders` field.
/// Ported 1:1 from Go `HeaderEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HeaderEntry {
    /// Header name.
    #[serde(default)]
    pub key: String,
    /// Header value. The sentinel `"__CONDUIT_CLEAR__"` requests a deletion
    /// when converted to override operations.
    #[serde(default)]
    pub value: String,
}

/// Per-channel transform toggles. Ported 1:1 from Go `TransformOptions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TransformOptions {
    /// Force the channel to accept array format for `instructions`.
    #[serde(default)]
    pub force_array_instructions: bool,
    /// Force the channel to accept array format for `inputs`.
    #[serde(default)]
    pub force_array_inputs: bool,
    /// Replace `developer` role with `system` in messages (Bailian
    /// compatibility).
    #[serde(default)]
    pub replace_developer_role_with_system: bool,
}

/// Top-level channel settings. Ported 1:1 from Go `ChannelSettings`.
///
/// Note on `override_parameters` / `override_headers`: these legacy JSON-string
/// / `HeaderEntry` fields are kept for backward compatibility and are
/// superseded by the structured `bodyOverrideOperations` /
/// `headerOverrideOperations` fields. Both pairs are exposed verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSettings {
    /// Optional control-plane adapter used to manage the upstream behind this
    /// request channel. This is deliberately separate from the channel type:
    /// a NEW API station still speaks the OpenAI-compatible data-plane API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_adapter: Option<String>,
    /// Real currency used when purchasing balance from this upstream.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub billing_currency: String,
    /// Channel balance units received for one billing-currency unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recharge_multiplier: Option<Decimal>,
    /// Accept models with this extra prefix in addition to the bare names in
    /// `supported_models`.
    #[serde(default)]
    pub extra_model_prefix: String,
    /// Prefixes to automatically trim from model names when matching against
    /// `supported_models`.
    #[serde(default)]
    pub auto_trimed_model_prefixes: Vec<String>,
    /// Model alias mappings.
    #[serde(default)]
    pub model_mappings: Vec<ModelMapping>,
    /// Hide original (provider) model names from the model list when mappings
    /// are configured.
    #[serde(default)]
    pub hide_original_models: bool,
    /// Hide mapped model names from the model list when mappings are
    /// configured.
    #[serde(default)]
    pub hide_mapped_models: bool,
    /// Lowercase the model id used for matching (not the value sent upstream).
    #[serde(default, rename = "lowercaseModelId")]
    pub lowercase_model_id: bool,
    /// Legacy override parameters as a JSON string. Deprecated; prefer
    /// `body_override_operations`.
    #[serde(default)]
    pub override_parameters: String,
    /// Structured override operations for the request body. When present
    /// (including an empty array) it takes precedence over
    /// `override_parameters`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body_override_operations: Vec<OverrideOperation>,
    /// Legacy header overrides as `HeaderEntry` records. Deprecated; prefer
    /// `header_override_operations`.
    #[serde(default)]
    pub override_headers: Vec<HeaderEntry>,
    /// Structured override operations for request headers. When present
    /// (including an empty array) it takes precedence over `override_headers`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub header_override_operations: Vec<OverrideOperation>,
    /// Channel-level proxy configuration.
    // TODO(parity): typed httpclient::ProxyConfig once the httpclient port
    // lands; until then store the raw JSON so round-trips are lossless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<serde_json::Value>,
    /// Per-channel transform toggles.
    #[serde(default)]
    pub transform_options: TransformOptions,
    /// Override the global pass-through-User-Agent setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_through_user_agent: Option<bool>,
    /// Override the global pass-through-body setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass_through_body: Option<bool>,
    /// Upstream rate-limit configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<ChannelRateLimit>,
    /// Additional HTTP status codes that trigger retry for this channel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retryable_status_codes: Vec<i64>,
    /// Additional error-text patterns that trigger retry for this channel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retryable_error_patterns: Vec<RetryableErrorPattern>,
    /// Ordered rules that connect provider-discovered model ids to public
    /// model drafts. The first matching rule wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_model_mapping_rules: Vec<AutoModelMappingRule>,
    /// Ordered rules that rewrite the final client-visible error emitted for
    /// this channel. Retry and health decisions always use the original error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_response_rewrite_rules: Vec<ErrorResponseRewriteRule>,
}

/// One ordered rule for turning a discovered upstream model id into a public
/// model association. Regex capture syntax in templates follows Rust regex
/// expansion (`$1` / `$name`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoModelMappingRule {
    pub pattern: String,
    pub public_model_id_template: String,
    #[serde(default = "default_true")]
    pub create_draft: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub developer_template: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name_template: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group_template: String,
    #[serde(default = "default_model_type")]
    pub model_type: String,
}

impl Default for AutoModelMappingRule {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            public_model_id_template: String::new(),
            create_draft: true,
            developer_template: String::new(),
            name_template: String::new(),
            group_template: String::new(),
            model_type: default_model_type(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_model_type() -> String {
    "chat".to_string()
}

/// One final-response rewrite rule for a channel. Empty match fields mean
/// "match all"; the first matching rule wins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponseRewriteRule {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status_codes: Vec<u16>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body_pattern: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
}

/// Invalid channel recharge metadata supplied by an administrative mutation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelBillingMetadataError {
    #[error("billing_currency and recharge_multiplier must be provided together")]
    IncompletePair,
    #[error("billing_currency must match [A-Z]{{3}}")]
    InvalidCurrency,
    #[error("recharge_multiplier must be greater than zero")]
    NonPositiveMultiplier,
}

/// Validate channel recharge metadata and return its canonical currency code.
///
/// Both values are optional as a pair. A present currency is trimmed and
/// uppercased before it is checked as exactly three ASCII letters.
pub fn normalize_channel_billing_metadata(
    billing_currency: Option<&str>,
    recharge_multiplier: Option<&Decimal>,
) -> Result<Option<String>, ChannelBillingMetadataError> {
    let (Some(billing_currency), Some(recharge_multiplier)) =
        (billing_currency, recharge_multiplier)
    else {
        return if billing_currency.is_none() && recharge_multiplier.is_none() {
            Ok(None)
        } else {
            Err(ChannelBillingMetadataError::IncompletePair)
        };
    };

    let normalized = billing_currency.trim().to_ascii_uppercase();
    if normalized.len() != 3 || !normalized.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(ChannelBillingMetadataError::InvalidCurrency);
    }
    if *recharge_multiplier <= Decimal::ZERO {
        return Err(ChannelBillingMetadataError::NonPositiveMultiplier);
    }

    Ok(Some(normalized))
}

/// A retryable error-text pattern. Ported 1:1 from Go `RetryableErrorPattern`.
///
/// When `regex` is false, `pattern` is matched as a case-sensitive substring of
/// the error text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RetryableErrorPattern {
    /// Pattern text (regex when `regex` is true, case-sensitive substring
    /// otherwise).
    #[serde(default)]
    pub pattern: String,
    /// Whether `pattern` is a regular expression.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub regex: bool,
}

/// Upstream rate-limit configuration for a channel. Ported 1:1 from Go
/// `ChannelRateLimit`.
///
/// All fields are optional pointers in Go (`*int64`); `None` means
/// "unlimited" for `rpm` / `tpm` / `max_concurrent`, "soft mode" for
/// `queue_size` / `queue_timeout_ms` when zero-or-absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelRateLimit {
    /// Requests per minute. `None` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<i64>,
    /// Tokens per minute. `None` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm: Option<i64>,
    /// Maximum concurrent requests. `None` = unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<i64>,
    /// Queue capacity when `max_concurrent` is set. `None` / `0` = soft mode
    /// (count only); `> 0` = hard mode (bounded FIFO). No effect when
    /// `max_concurrent` is unset or `<= 0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_size: Option<i64>,
    /// Per-channel queue wait timeout in milliseconds. `None` / `0` = no
    /// per-channel timeout; only meaningful in hard mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_timeout_ms: Option<i64>,
}

/// A record of a disabled API key (sensitive; protected at the same level as
/// credentials). Ported 1:1 from Go `DisabledAPIKey`.
///
/// Disable tracking keys on the key plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DisabledAPIKey {
    /// Plaintext of the disabled key.
    #[serde(default)]
    pub key: String,
    /// When the key was disabled.
    #[serde(default)]
    pub disabled_at: DateTime<Utc>,
    /// Error code that caused the disable (e.g. upstream 401).
    #[serde(default)]
    pub error_code: i64,
    /// Optional human-readable reason. Omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// Channel credentials bundle. Ported 1:1 from Go `ChannelCredentials`.
///
/// The OAuth field is left as [`serde_json::Value`] pending the `oauth` package
/// port. The legacy `apiKey` field is kept for backward compatibility.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelCredentials {
    /// Single API key (legacy; prefer `oauth` or `api_keys`). Omitted on the
    /// wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// OAuth credentials for OAuth-style channels.
    // TODO(parity): typed OAuthCredentials once the oauth package is ported;
    // until then store the raw JSON so round-trips are lossless. Go reads
    // `oauth.AccessToken` in `IsOAuth`, which we approximate by inspecting the
    // JSON shape (see [`ChannelCredentials::is_oauth`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<serde_json::Value>,
    /// Multiple API keys, used round-robin.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_keys: Vec<String>,
    /// Azure-specific configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub azure: Option<AzureCredential>,
    /// GCP credentials.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "gcp")]
    pub gcp: Option<GCPCredential>,
}

impl ChannelCredentials {
    /// Return all API keys for the channel, combining the legacy `api_key` and
    /// the newer `api_keys` list. Ported 1:1 from Go
    /// `ChannelCredentials.GetAllAPIKeys`.
    ///
    /// The legacy `api_key` is included only when present **and** the channel
    /// is not an OAuth channel (i.e. `api_key` does not itself contain OAuth
    /// JSON). Returns `None` when both sources are empty (mirrors Go `nil`).
    pub fn get_all_api_keys(&self) -> Option<Vec<String>> {
        let mut keys: Vec<String> = Vec::new();

        // Add legacy api_key if present and not an OAuth credential.
        if !self.api_key.is_empty() && !self.is_oauth() {
            keys.push(self.api_key.clone());
        }

        // Append the new api_keys list.
        keys.extend(self.api_keys.iter().cloned());

        if keys.is_empty() { None } else { Some(keys) }
    }

    /// Return API keys that are not in the `disabled_keys` list. Ported 1:1
    /// from Go `ChannelCredentials.GetEnabledAPIKeys`.
    ///
    /// Empty-key entries in `disabled_keys` are ignored (matching Go's skip of
    /// `dk.Key == ""`). When `disabled_keys` is empty, all keys are returned
    /// unchanged.
    pub fn get_enabled_api_keys(&self, disabled_keys: &[DisabledAPIKey]) -> Option<Vec<String>> {
        let all_keys = self.get_all_api_keys();
        if disabled_keys.is_empty() {
            return all_keys;
        }
        let all_keys = all_keys?;

        let mut disabled_set: std::collections::HashSet<&str> =
            std::collections::HashSet::with_capacity(disabled_keys.len());
        for dk in disabled_keys {
            if dk.key.is_empty() {
                continue;
            }
            disabled_set.insert(dk.key.as_str());
        }

        let enabled: Vec<String> = all_keys
            .into_iter()
            .filter(|key| !disabled_set.contains(key.as_str()))
            .collect();

        if enabled.is_empty() {
            None
        } else {
            Some(enabled)
        }
    }

    /// Return whether OAuth credentials are configured and valid. Ported 1:1
    /// from Go `ChannelCredentials.IsOAuth`.
    ///
    /// Two paths:
    /// 1. The new `oauth` field is present and its JSON contains a non-empty
    ///    `access_token` string (Go reads the struct field
    ///    `c.OAuth.AccessToken`, whose JSON tag is `access_token`).
    /// 2. The legacy `api_key` field contains an OAuth JSON blob (detected by
    ///    [`is_oauth_json`]).
    pub fn is_oauth(&self) -> bool {
        // Check the new oauth field first: Go reads `c.OAuth.AccessToken`
        // (struct field, not JSON). The underlying `oauth.OAuthCredentials`
        // type serializes `AccessToken` as `access_token` (snake_case), so we
        // inspect that key in the raw JSON value.
        if let Some(oauth) = &self.oauth {
            let has_token = match oauth
                .get("access_token")
                .and_then(serde_json::Value::as_str)
            {
                Some(token) => !token.is_empty(),
                None => false,
            };
            if has_token {
                return true;
            }
        }

        // Backward compatibility: check whether api_key contains OAuth JSON.
        is_oauth_json(&self.api_key)
    }
}

/// Detect whether a string looks like an OAuth JSON credential. Ported 1:1
/// from Go `isOAuthJSON`.
///
/// Returns true when the trimmed string starts with `{` and contains
/// `access_token`. This is intentionally lenient (substring check, not full
/// JSON parsing) to match the Go implementation exactly.
pub fn is_oauth_json(s: &str) -> bool {
    let trimmed = s.trim();
    trimmed.starts_with('{') && trimmed.contains("access_token")
}

/// Azure-specific channel configuration. Ported 1:1 from Go `AzureCredential`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AzureCredential {
    /// Optional Azure API version.
    #[serde(default)]
    pub api_version: String,
}

/// GCP channel credentials (external form). Ported 1:1 from Go `GCPCredential`.
///
/// Note: `projectID` has an internal capital in the Go tag, so it gets an
/// explicit `rename` (camelCase would produce `projectId`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GCPCredential {
    /// GCP region.
    #[serde(default)]
    pub region: String,
    /// GCP project id.
    #[serde(default, rename = "projectID")]
    pub project_id: String,
    /// Raw GCP service-account JSON (string).
    #[serde(default)]
    pub json_data: String,
}

/// Parsed GCP service-account JSON. Ported 1:1 from Go `GCPCredentialsJSON`.
///
/// Every field is required by Go's `validate:"required"` tag, but the struct is
/// still constructed piecewise; the Rust side keeps them as plain `String`s
/// and does not enforce requiredness at the type level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GCPCredentialsJSON {
    /// Service-account type (always `"service_account"` for real credentials).
    #[serde(default)]
    pub r#type: String,
    /// GCP project id.
    #[serde(default, rename = "projectID")]
    pub project_id: String,
    /// Service-account private-key id.
    #[serde(default, rename = "privateKeyID")]
    pub private_key_id: String,
    /// PEM-encoded private key.
    #[serde(default, rename = "privateKey")]
    pub private_key: String,
    /// Service-account client email.
    #[serde(default, rename = "clientEmail")]
    pub client_email: String,
    /// Service-account client id.
    #[serde(default, rename = "clientID")]
    pub client_id: String,
    /// OAuth auth URI.
    #[serde(default, rename = "authURI")]
    pub auth_uri: String,
    /// OAuth token URI.
    #[serde(default, rename = "tokenURI")]
    pub token_uri: String,
    /// Auth-provider x509 cert URL.
    #[serde(default, rename = "authProviderX509CertURL")]
    pub auth_provider_x509_cert_url: String,
    /// Client x509 cert URL.
    #[serde(default, rename = "clientX509CertURL")]
    pub client_x509_cert_url: String,
    /// Universe domain.
    #[serde(default, rename = "universeDomain")]
    pub universe_domain: String,
}

/// Capability policy. Ported 1:1 from the Go `CapabilityPolicy` string
/// newtype; known variants live in the [`capability_policy`] module. Unknown
/// wire values round-trip without error because this is a `String` alias
/// rather than a closed enum.
pub type CapabilityPolicy = String;

/// Known values of [`CapabilityPolicy`]. Mirrors the Go `CapabilityPolicy*`
/// consts.
pub mod capability_policy {
    /// `CapabilityPolicy = "unlimited"` — the capability has no limit.
    pub const UNLIMITED: &str = "unlimited";
    /// `CapabilityPolicy = "require"` — the capability is required.
    pub const REQUIRE: &str = "require";
    /// `CapabilityPolicy = "forbid"` — the capability is forbidden.
    pub const FORBID: &str = "forbid";
}

/// Per-capability policies. Ported 1:1 from Go `ChannelPolicies`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ChannelPolicies {
    /// Streaming capability policy. Omitted on the wire when empty
    /// (Go `omitempty`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stream: CapabilityPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_billing_metadata_normalizes_valid_currency() {
        let multiplier = Decimal::new(125, 1);
        assert_eq!(
            normalize_channel_billing_metadata(Some(" usd "), Some(&multiplier)),
            Ok(Some("USD".to_string()))
        );
        assert_eq!(normalize_channel_billing_metadata(None, None), Ok(None));
    }

    #[test]
    fn channel_billing_metadata_requires_a_complete_pair() {
        let multiplier = Decimal::ONE;
        assert_eq!(
            normalize_channel_billing_metadata(Some("USD"), None),
            Err(ChannelBillingMetadataError::IncompletePair)
        );
        assert_eq!(
            normalize_channel_billing_metadata(None, Some(&multiplier)),
            Err(ChannelBillingMetadataError::IncompletePair)
        );
    }

    #[test]
    fn channel_billing_metadata_rejects_invalid_currency_and_multiplier() {
        let positive = Decimal::ONE;
        for currency in ["US", "US1", "USDE", "€UR"] {
            assert_eq!(
                normalize_channel_billing_metadata(Some(currency), Some(&positive)),
                Err(ChannelBillingMetadataError::InvalidCurrency),
                "currency: {currency}"
            );
        }

        for multiplier in [Decimal::ZERO, Decimal::NEGATIVE_ONE] {
            assert_eq!(
                normalize_channel_billing_metadata(Some("USD"), Some(&multiplier)),
                Err(ChannelBillingMetadataError::NonPositiveMultiplier)
            );
        }
    }

    #[test]
    fn channel_endpoint_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let ep = ChannelEndpoint {
            api_format: "openai".to_string(),
            path: "/v1/chat/completions".to_string(),
            base_url: "https://api.openai.com".to_string(),
            transport: channel_endpoint_transport::HTTP.to_string(),
        };
        let json = serde_json::to_string(&ep)?;
        let back: ChannelEndpoint = serde_json::from_str(&json)?;
        assert_eq!(ep, back);
        // Go uses snake_case tags for this struct (`api_format`, `base_url`);
        // without `rename_all`, the Rust snake_case field names match directly.
        assert!(json.contains("\"api_format\""));
        assert!(json.contains("\"base_url\""));
        Ok(())
    }

    #[test]
    fn channel_settings_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let settings = ChannelSettings {
            management_adapter: None,
            billing_currency: "USD".to_string(),
            recharge_multiplier: Some(Decimal::from(10)),
            extra_model_prefix: "deepseek".to_string(),
            auto_trimed_model_prefixes: vec!["openai".to_string(), "deepseek".to_string()],
            model_mappings: vec![ModelMapping {
                from: "deepseek-chat".to_string(),
                to: "deepseek/deepseek-chat".to_string(),
            }],
            hide_original_models: true,
            hide_mapped_models: false,
            lowercase_model_id: true,
            override_parameters: r#"{"temperature":0.7}"#.to_string(),
            body_override_operations: vec![OverrideOperation {
                op: crate::objects::overrides::override_op::SET.to_string(),
                path: "$.max_tokens".to_string(),
                value: "100".to_string(),
                ..Default::default()
            }],
            override_headers: vec![HeaderEntry {
                key: "User-Agent".to_string(),
                value: "Conduit API".to_string(),
            }],
            header_override_operations: vec![],
            proxy: Some(serde_json::json!({"type":"http"})),
            transform_options: TransformOptions {
                force_array_instructions: true,
                force_array_inputs: false,
                replace_developer_role_with_system: true,
            },
            pass_through_user_agent: Some(true),
            pass_through_body: None,
            rate_limit: Some(ChannelRateLimit {
                rpm: Some(60),
                tpm: None,
                max_concurrent: Some(10),
                queue_size: None,
                queue_timeout_ms: Some(5000),
            }),
            retryable_status_codes: vec![502, 503],
            retryable_error_patterns: vec![RetryableErrorPattern {
                pattern: "overloaded".to_string(),
                regex: false,
            }],
            auto_model_mapping_rules: vec![AutoModelMappingRule {
                pattern: r"^gpt-(.+)$".to_string(),
                public_model_id_template: "openai/gpt-$1".to_string(),
                developer_template: "OpenAI".to_string(),
                name_template: "GPT $1".to_string(),
                ..Default::default()
            }],
            error_response_rewrite_rules: vec![ErrorResponseRewriteRule {
                status_codes: vec![429],
                http_status: Some(503),
                message: Some("channel ${channel_id} is unavailable".to_string()),
                code: Some("channel_unavailable".to_string()),
                ..Default::default()
            }],
        };
        let json = serde_json::to_string(&settings)?;
        let back: ChannelSettings = serde_json::from_str(&json)?;
        assert_eq!(settings, back);
        Ok(())
    }

    #[test]
    fn channel_credentials_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let creds = ChannelCredentials {
            api_key: "sk-test".to_string(),
            oauth: None,
            api_keys: vec!["sk-a".to_string(), "sk-b".to_string()],
            azure: Some(AzureCredential {
                api_version: "2024-02-15".to_string(),
            }),
            gcp: Some(GCPCredential {
                region: "us-central1".to_string(),
                project_id: "my-project".to_string(),
                json_data: "{}".to_string(),
            }),
        };
        let json = serde_json::to_string(&creds)?;
        let back: ChannelCredentials = serde_json::from_str(&json)?;
        assert_eq!(creds, back);
        // Empty api_key should be omitted; here it's set so present.
        assert!(json.contains("\"apiKey\""));
        Ok(())
    }

    #[test]
    fn channel_credentials_omits_empty_fields() -> Result<(), Box<dyn std::error::Error>> {
        let creds = ChannelCredentials::default();
        let json = serde_json::to_string(&creds)?;
        assert_eq!(json, "{}");
        Ok(())
    }

    #[test]
    fn is_oauth_json_detects_oauth_blob() {
        assert!(is_oauth_json(
            r#"{"access_token":"abc","refresh_token":"def"}"#
        ));
        assert!(is_oauth_json("  {\"access_token\":\"x\"}  "));
    }

    #[test]
    fn is_oauth_json_rejects_plain_string() {
        assert!(!is_oauth_json("sk-plain-key"));
        assert!(!is_oauth_json(""));
        assert!(!is_oauth_json("not json at all"));
        // A bare object without access_token is not OAuth per the Go rule.
        assert!(!is_oauth_json(r#"{"apiKey":"x"}"#));
    }

    #[test]
    fn is_oauth_via_oauth_field_with_access_token() {
        let creds = ChannelCredentials {
            oauth: Some(serde_json::json!({"access_token":"tok-123","refresh_token":"r"})),
            ..Default::default()
        };
        assert!(creds.is_oauth());
    }

    #[test]
    fn is_oauth_via_legacy_api_key_json() {
        let creds = ChannelCredentials {
            api_key: r#"{"access_token":"legacy"}"#.to_string(),
            ..Default::default()
        };
        assert!(creds.is_oauth());
    }

    #[test]
    fn is_oauth_false_for_plain_api_key() {
        let creds = ChannelCredentials {
            api_key: "sk-plain".to_string(),
            ..Default::default()
        };
        assert!(!creds.is_oauth());
    }

    #[test]
    fn is_oauth_false_for_empty_oauth_field() {
        let creds = ChannelCredentials {
            oauth: Some(serde_json::json!({"access_token":""})),
            ..Default::default()
        };
        assert!(!creds.is_oauth());
    }

    #[test]
    fn get_all_api_keys_combines_legacy_and_list() {
        let creds = ChannelCredentials {
            api_key: "sk-legacy".to_string(),
            api_keys: vec!["sk-a".to_string(), "sk-b".to_string()],
            ..Default::default()
        };
        match creds.get_all_api_keys() {
            Some(keys) => assert_eq!(keys, vec!["sk-legacy", "sk-a", "sk-b"]),
            None => panic!("expected Some(keys)"),
        }
    }

    #[test]
    fn get_all_api_keys_excludes_legacy_when_oauth() {
        // When api_key holds OAuth JSON, IsOAuth() is true and GetAllAPIKeys
        // must NOT include the legacy api_key.
        let creds = ChannelCredentials {
            api_key: r#"{"access_token":"tok"}"#.to_string(),
            api_keys: vec!["sk-a".to_string()],
            ..Default::default()
        };
        match creds.get_all_api_keys() {
            Some(keys) => assert_eq!(keys, vec!["sk-a"]),
            None => panic!("expected Some(keys)"),
        }
    }

    #[test]
    fn get_all_api_keys_empty_returns_none() {
        let creds = ChannelCredentials::default();
        assert!(creds.get_all_api_keys().is_none());
    }

    #[test]
    fn get_enabled_api_keys_returns_all_when_no_disabled() {
        let creds = ChannelCredentials {
            api_keys: vec!["sk-a".to_string(), "sk-b".to_string()],
            ..Default::default()
        };
        let enabled = creds.get_enabled_api_keys(&[]);
        assert_eq!(enabled, Some(vec!["sk-a".to_string(), "sk-b".to_string()]));
    }

    #[test]
    fn get_enabled_api_keys_filters_disabled() {
        let creds = ChannelCredentials {
            api_key: "sk-legacy".to_string(),
            api_keys: vec!["sk-a".to_string(), "sk-b".to_string()],
            ..Default::default()
        };
        let disabled = vec![
            DisabledAPIKey {
                key: "sk-a".to_string(),
                disabled_at: chrono::Utc::now(),
                error_code: 401,
                reason: "revoked".to_string(),
            },
            // Empty-key entry is ignored by the filter, matching Go.
            DisabledAPIKey {
                key: String::new(),
                disabled_at: chrono::Utc::now(),
                error_code: 401,
                reason: String::new(),
            },
        ];
        let enabled = creds.get_enabled_api_keys(&disabled);
        match enabled {
            Some(keys) => assert_eq!(keys, vec!["sk-legacy", "sk-b"]),
            None => panic!("expected Some — remaining keys"),
        }
    }

    #[test]
    fn get_enabled_api_keys_all_disabled_returns_none() {
        let creds = ChannelCredentials {
            api_keys: vec!["sk-a".to_string()],
            ..Default::default()
        };
        let disabled = vec![DisabledAPIKey {
            key: "sk-a".to_string(),
            disabled_at: chrono::Utc::now(),
            error_code: 401,
            reason: String::new(),
        }];
        assert!(creds.get_enabled_api_keys(&disabled).is_none());
    }

    #[test]
    fn get_enabled_api_keys_empty_credentials_returns_none() {
        let creds = ChannelCredentials::default();
        let disabled = vec![DisabledAPIKey {
            key: "sk-x".to_string(),
            disabled_at: chrono::Utc::now(),
            error_code: 401,
            reason: String::new(),
        }];
        assert!(creds.get_enabled_api_keys(&disabled).is_none());
    }

    #[test]
    fn gcp_credentials_json_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let json_str = r#"{
            "type":"service_account",
            "projectID":"my-project",
            "privateKeyID":"kid",
            "privateKey":"-----BEGIN PRIVATE KEY-----\nKEY\n-----END PRIVATE KEY-----\n",
            "clientEmail":"sa@my-project.iam.gserviceaccount.com",
            "clientID":"123",
            "authURI":"https://accounts.google.com/o/oauth2/auth",
            "tokenURI":"https://oauth2.googleapis.com/token",
            "authProviderX509CertURL":"https://www.googleapis.com/oauth2/v1/certs",
            "clientX509CertURL":"https://www.googleapis.com/robot/v1/metadata/x509/sa%40my-project.iam.gserviceaccount.com",
            "universeDomain":"googleapis.com"
        }"#;
        let parsed: GCPCredentialsJSON = serde_json::from_str(json_str)?;
        assert_eq!(parsed.r#type, "service_account");
        assert_eq!(parsed.project_id, "my-project");
        assert_eq!(parsed.client_email, "sa@my-project.iam.gserviceaccount.com");
        // Round-trip preserves the acronym-tagged fields.
        let serialized = serde_json::to_string(&parsed)?;
        assert!(serialized.contains("\"projectID\""));
        assert!(serialized.contains("\"privateKeyID\""));
        assert!(serialized.contains("\"clientX509CertURL\""));
        Ok(())
    }

    #[test]
    fn channel_rate_limit_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let rl = ChannelRateLimit {
            rpm: Some(120),
            tpm: Some(100_000),
            max_concurrent: None,
            queue_size: Some(50),
            queue_timeout_ms: None,
        };
        let json = serde_json::to_string(&rl)?;
        let back: ChannelRateLimit = serde_json::from_str(&json)?;
        assert_eq!(rl, back);
        Ok(())
    }

    #[test]
    fn channel_policies_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let policies = ChannelPolicies {
            stream: capability_policy::UNLIMITED.to_string(),
        };
        let json = serde_json::to_string(&policies)?;
        let back: ChannelPolicies = serde_json::from_str(&json)?;
        assert_eq!(policies, back);
        // Empty policy should serialize to `{}` due to omitempty.
        let empty = ChannelPolicies::default();
        assert_eq!(serde_json::to_string(&empty)?, "{}");
        Ok(())
    }
}
