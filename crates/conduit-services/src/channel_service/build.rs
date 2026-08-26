//! S04/S09 channel builder wiring: [`build_channel_with_transformer`] (plan)
//! and [`build_channel_with_outbounds`] (outbound-map assembly).
//!
//! Ported from Go `internal/server/biz/channel_llm.go`:
//! - `buildChannel` (lines 116-147): precompute disabled-key set + enabled keys
//!   and stash them on the Channel as caches.
//! - `buildChannelWithTransformer` (lines 443-1060): validate credentials per
//!   channel type, then resolve the primary outbound transformer family via the
//!   registry's `provider_descriptor` switch.
//! - `buildChannelWithOutbounds` (lines 194-235): populate the per-api_format
//!   outbound map. Default endpoints **alias the primary outbound** (S09); only
//!   user-configured non-default endpoints get their own per-endpoint outbound.
//!
//! The Go side constructs the real outbound transformer (HTTP client, OAuth
//! token provider, …) inline. That construction needs I/O and credentials
//! plumbing that lives in the host crate (the future `biz.ChannelService` port
//! that owns HTTP clients and token refreshers). To keep this module pure and
//! unit-testable, the builder is split into two phases:
//!
//! 1. [`build_channel_with_transformer`] consumes the channel inputs and the
//!    transformer registry, runs the credential validation + provider-family
//!    resolution, and returns a [`ChannelBuildPlan`]. The plan describes
//!    *which* primary outbound the host must construct (family + key-provider
//!    kind + base URL) without constructing it.
//! 2. [`build_channel_with_outbounds`] consumes that plan *plus* the
//!    already-constructed primary outbound (an `Arc<dyn OutboundTransformer>`
//!    built by the host from the plan) and the channel's endpoint list, then
//!    assembles the `BTreeMap<api_format, Arc<dyn OutboundTransformer>>`
//!    outbound map mirroring Go's aliasing rule (channel_llm.go:209-215).
//!
//! Host-side wiring follow-up (out of scope for these two crates): the host
//! crate that owns HTTP clients + OAuth providers constructs the real
//! `Arc<dyn OutboundTransformer>` from the [`ChannelBuildPlan`] and passes it
//! into [`build_channel_with_outbounds`]. The two functions here own the
//! contract parity (validation, family resolution, endpoint aliasing); the host
//! owns the I/O.

use std::collections::BTreeMap;
use std::sync::Arc;

use conduit_core::objects::channel_settings::ChannelEndpoint;
use conduit_transformers::registry::{
    CredentialRequirement, KeyProviderKind, ProviderDescriptor, key_provider_kind,
    provider_descriptor, required_credential_kind,
};
use conduit_transformers::traits::OutboundTransformer;

use super::endpoint::DefaultEndpointRegistry;

/// Errors raised by the channel-builder pipeline (S04). Mirror the Go
/// `fmt.Errorf` messages from `buildChannelWithTransformer`'s credential
/// validation switch (channel_llm.go:450-473) and the
/// `buildChannelWithOutbounds` per-endpoint failure path
/// (channel_llm.go:221-225).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelBuildError {
    /// `missing credentials: oauth or api key required for channel {name}`
    /// (Go channel_llm.go:452-453 — Codex / ClaudeCode branch).
    #[error("missing credentials: oauth or api key required for channel {name}")]
    MissingOAuthOrApiKey { name: String },
    /// `missing oauth credentials for channel {name}` (Go :458 — Copilot).
    #[error("missing oauth credentials for channel {name}")]
    MissingOAuth { name: String },
    /// `missing api key for channel {name}` (Go :463 antigravity, :471 default).
    #[error("missing api key for channel {name}")]
    MissingApiKey { name: String },
    /// `GCP credentials are required for anthropic_vertex channel` (Go :790).
    #[error("GCP credentials are required for anthropic_vertex channel")]
    MissingGcpCredentials,
    /// `unknown channel type` (Go :1058 — the default branch of the
    /// transformer-construction switch).
    #[error("unknown channel type {channel_type:?}")]
    UnknownChannelType { channel_type: String },
    /// `failed to build outbound for api_format {api_format:?} on channel
    /// {name}: {reason}` (Go :222-224 — wraps a per-endpoint build failure).
    #[error("failed to build outbound for api_format {api_format:?} on channel {name}: {reason}")]
    EndpointOutboundBuild {
        name: String,
        api_format: String,
        reason: String,
    },
}

/// Inputs for the channel builder. This is the pure-data view of the fields Go
/// reads off `*ent.Channel` inside `buildChannelWithTransformer`. The host
/// assembles it from the persisted channel row + per-request override.
#[derive(Debug, Clone)]
pub struct ChannelBuildInput<'a> {
    /// Channel display name (used in error messages; Go `c.Name`).
    pub name: &'a str,
    /// Channel type string (Go `c.Type.String()` — e.g. `"openai"`,
    /// `"anthropic"`, `"codex"`).
    pub channel_type: &'a str,
    /// Channel base URL (Go `c.BaseURL`). Recorded on the plan so the host
    /// can pass it to the transformer constructor without re-reading the row.
    pub base_url: &'a str,
    /// Enabled API keys (Go's `cachedEnabledAPIKeys` — already filtered to
    /// remove disabled keys).
    pub enabled_api_keys: &'a [String],
    /// Whether the channel has OAuth credentials configured (Go
    /// `c.Credentials.IsOAuth()`).
    pub is_oauth: bool,
    /// Whether the channel has Azure credentials configured.
    pub has_azure: bool,
    /// Whether the channel has GCP credentials configured (Go checks
    /// `c.Credentials.GCP != nil`).
    pub has_gcp: bool,
    /// Legacy single API key, used by the antigravity credential check (Go
    /// channel_llm.go:462-464: `strings.TrimSpace(c.Credentials.APIKey) == ""`).
    pub legacy_api_key: &'a str,
    /// Per-request API-key override (Go `apiKeyOverride` variadic arg). When
    /// non-empty the key-provider decision collapses to `Static`.
    pub api_key_override: &'a str,
    /// Channel's user-configured endpoints (Go `c.Endpoints`). Used by
    /// [`build_channel_with_outbounds`] to assemble the outbound map.
    pub user_endpoints: &'a [ChannelEndpoint],
}

impl<'a> ChannelBuildInput<'a> {
    /// Convenience constructor for the common case where the inputs come
    /// straight from a `ChannelCredentials` value plus the runtime caches
    /// (enabled keys + override).
    pub fn from_credentials(
        name: &'a str,
        channel_type: &'a str,
        base_url: &'a str,
        creds: &'a conduit_core::objects::channel_settings::ChannelCredentials,
        enabled_api_keys: &'a [String],
        user_endpoints: &'a [ChannelEndpoint],
        api_key_override: &'a str,
    ) -> Self {
        Self {
            name,
            channel_type,
            base_url,
            enabled_api_keys,
            is_oauth: creds.is_oauth(),
            has_azure: creds.azure.is_some(),
            has_gcp: creds.gcp.is_some(),
            legacy_api_key: &creds.api_key,
            api_key_override,
            user_endpoints,
        }
    }
}

/// The result of [`build_channel_with_transformer`]: everything the host needs
/// to construct the primary outbound transformer, plus the metadata the
/// outbound-map assembler ([`build_channel_with_outbounds`]) needs.
///
/// This type deliberately does NOT hold the constructed transformer — building
/// it needs HTTP/OAuth I/O that lives in the host crate. Instead the host
/// reads `descriptor.family` / `key_provider_kind` / `base_url`, constructs the
/// real `Arc<dyn OutboundTransformer>`, then hands it to
/// [`build_channel_with_outbounds`].
#[derive(Debug, Clone)]
pub struct ChannelBuildPlan {
    /// Resolved provider descriptor (mirrors Go's per-channel-type switch
    /// selecting the transformer constructor).
    pub descriptor: ProviderDescriptor,
    /// Resolved key-provider kind (Go `getAPIKeyProvider` decision).
    pub key_provider_kind: KeyProviderKind,
    /// Resolved credential requirement (the validation branch the channel
    /// passed).
    pub credential_requirement: CredentialRequirement,
    /// Effective base URL for the primary outbound (channel_llm.go:475 reads
    /// `svc.getHttpClient(c.Settings)`; the base URL itself is `c.BaseURL`).
    pub base_url: String,
    /// The channel type string, echoed back for the host's transformer
    /// constructor dispatch.
    pub channel_type: String,
}

/// S04 — Build the channel plan: validate credentials per channel type, then
/// resolve the primary outbound's provider family via the transformer
/// registry.
///
/// Mirrors Go `buildChannelWithTransformer` (channel_llm.go:443-1060):
/// 1. Validate credentials per `required_credential_kind(channel_type)`
///    (Go :450-473).
/// 2. Resolve the provider descriptor via `provider_descriptor(channel_type)`
///    (Go :481-1059 switch). An unknown type yields
///    [`ChannelBuildError::UnknownChannelType`] (Go :1057-1058 default branch).
/// 3. Resolve the key-provider kind via `key_provider_kind` (Go
///    `getAPIKeyProvider`, :155-170).
///
/// The real outbound transformer is **not** constructed here — that needs
/// HTTP/OAuth I/O owned by the host crate. The plan describes what to build.
pub fn build_channel_with_transformer(
    input: &ChannelBuildInput<'_>,
) -> Result<ChannelBuildPlan, ChannelBuildError> {
    // 1. Credential validation — mirrors Go channel_llm.go:450-473.
    let requirement = required_credential_kind(input.channel_type);
    validate_credentials(input, requirement)?;

    // 2. Provider-family resolution — mirrors Go channel_llm.go:481-1059.
    let descriptor = provider_descriptor(input.channel_type).ok_or_else(|| {
        ChannelBuildError::UnknownChannelType {
            channel_type: input.channel_type.to_string(),
        }
    })?;

    // 3. Key-provider resolution — mirrors Go `getAPIKeyProvider` (:155-170).
    let key_provider_kind = key_provider_kind(
        input.channel_type,
        input.enabled_api_keys.len(),
        !input.api_key_override.is_empty(),
    );

    Ok(ChannelBuildPlan {
        descriptor,
        key_provider_kind,
        credential_requirement: requirement,
        base_url: input.base_url.to_string(),
        channel_type: input.channel_type.to_string(),
    })
}

/// Validate channel credentials against the requirement for its type. Mirrors
/// Go channel_llm.go:450-473 (the validation switch that runs before the
/// transformer-construction switch).
fn validate_credentials(
    input: &ChannelBuildInput<'_>,
    requirement: CredentialRequirement,
) -> Result<(), ChannelBuildError> {
    let name = input.name.to_string();
    match requirement {
        CredentialRequirement::OAuthOrApiKey => {
            // Go :451-454 (Codex / ClaudeCode): oauth OR at least one enabled key.
            if !input.is_oauth && input.enabled_api_keys.is_empty() {
                return Err(ChannelBuildError::MissingOAuthOrApiKey { name });
            }
        }
        CredentialRequirement::OAuthOnly => {
            // Go :455-459 (github_copilot): strict OAuth.
            if !input.is_oauth {
                return Err(ChannelBuildError::MissingOAuth { name });
            }
        }
        CredentialRequirement::AntigravityLegacy => {
            // Go :460-464: legacy APIKey field must be non-empty.
            if input.legacy_api_key.trim().is_empty() {
                return Err(ChannelBuildError::MissingApiKey { name });
            }
        }
        CredentialRequirement::GcpCredentials => {
            // Go :465-468 + :786-791: anthropic_gcp requires GCP JSON.
            if !input.has_gcp {
                return Err(ChannelBuildError::MissingGcpCredentials);
            }
        }
        CredentialRequirement::None => {
            // Go :465-468 (anthropic_fake / openai_fake): skip.
        }
        CredentialRequirement::OptionalApiKey => {
            // Go :1039-1044 (ollama): keys optional, no validation here.
        }
        CredentialRequirement::ApiKey => {
            // Go :469-473 (default branch): at least one enabled key.
            if input.enabled_api_keys.is_empty() {
                return Err(ChannelBuildError::MissingApiKey { name });
            }
        }
    }
    Ok(())
}

/// S09 — Assemble the per-`api_format` outbound map for a channel.
///
/// Mirrors Go `buildChannelWithOutbounds` (channel_llm.go:194-235) exactly:
/// 1. Every **default endpoint** for the channel type aliases the **primary
///    outbound** (`outbounds[ep.APIFormat] = ch.Outbound`, Go :209-215). This
///    is the S09 secondary-default-aliases-primary rule.
/// 2. Every **user-configured endpoint** that is NOT a default gets its own
///    per-endpoint outbound via `endpoint_outbound_resolver` (Go :217-226 calls
///    `buildNonDefaultEndpointOutbound`). Endpoints whose `api_format` matches
///    a default are **already consumed by step 1** (their override still wins
///    via [`super::endpoint::merge_endpoints`], but they keep aliasing the
///    primary outbound — see Go's loop order: defaults are iterated first and
///    unconditionally alias the primary; user endpoints are iterated second and
///    only the *non-default* ones reach `buildNonDefaultEndpointOutbound`).
/// 3. User endpoints whose `api_format` overrides a default still need the
///    primary outbound under that format, so step 1's default-format keys
///    always survive (they map to the primary).
///
/// `endpoint_outbound_resolver` is a host-supplied closure that constructs the
/// outbound for a non-default user endpoint. It receives the endpoint and
/// returns the constructed transformer (or an error). This keeps the I/O
/// (HTTP client / OAuth provider construction) in the host crate while this
/// function owns the aliasing contract.
///
/// Returns `None` when the channel has no default endpoints and no user
/// endpoints (Go returns the channel unchanged with `Outbounds == nil`,
/// channel_llm.go:203-205).
pub fn build_channel_with_outbounds<R, E>(
    channel_type: &str,
    default_endpoints: &[ChannelEndpoint],
    user_endpoints: &[ChannelEndpoint],
    primary_outbound: Arc<dyn OutboundTransformer>,
    mut endpoint_outbound_resolver: R,
) -> Result<Option<BTreeMap<String, Arc<dyn OutboundTransformer>>>, ChannelBuildError>
where
    R: FnMut(&ChannelEndpoint) -> Result<Arc<dyn OutboundTransformer>, E>,
    E: std::fmt::Display,
{
    if default_endpoints.is_empty() && user_endpoints.is_empty() {
        return Ok(None);
    }

    let mut outbounds: BTreeMap<String, Arc<dyn OutboundTransformer>> = BTreeMap::new();

    // 1. Default endpoints alias the primary outbound (S09, Go :209-215).
    //    Every default endpoint's api_format maps to the SAME primary outbound.
    for ep in default_endpoints {
        if ep.api_format.is_empty() {
            continue;
        }
        outbounds.insert(ep.api_format.clone(), Arc::clone(&primary_outbound));
    }

    // 2. User endpoints: only the ones NOT consumed by a default get their own
    //    outbound (Go :217-226). User endpoints that override a default keep
    //    the primary outbound under that api_format — the override affects
    //    path/base_url/transport on the *resolved* endpoint, not the outbound
    //    transformer family, so the aliasing rule still holds.
    let default_formats: std::collections::HashSet<&str> = default_endpoints
        .iter()
        .filter(|ep| !ep.api_format.is_empty())
        .map(|ep| ep.api_format.as_str())
        .collect();

    for ep in user_endpoints {
        if ep.api_format.is_empty() {
            continue;
        }
        // Skip user endpoints whose api_format is a default — they alias the
        // primary outbound (already inserted in step 1).
        if default_formats.contains(ep.api_format.as_str()) {
            // Override still wins for path/base_url/transport resolution, but
            // the outbound transformer family is the primary one. Re-insert to
            // ensure the key is present even if step 1 was skipped (defensive).
            outbounds.insert(ep.api_format.clone(), Arc::clone(&primary_outbound));
            continue;
        }
        // Non-default user endpoint: resolve its own outbound via the host.
        let out = endpoint_outbound_resolver(ep).map_err(|e| {
            ChannelBuildError::EndpointOutboundBuild {
                // The Go side does not have the channel name in scope at this
                // call site (it's on `c`); we surface the channel_type for
                // diagnostics instead, matching the host's available context.
                name: channel_type.to_string(),
                api_format: ep.api_format.clone(),
                reason: e.to_string(),
            }
        })?;
        outbounds.insert(ep.api_format.clone(), out);
    }

    if outbounds.is_empty() {
        Ok(None)
    } else {
        Ok(Some(outbounds))
    }
}

/// S04 convenience: validate-and-plan in one call, using the
/// [`DefaultEndpointRegistry`] for endpoint resolution. Equivalent to Go's
/// `ChannelService.buildChannelWithTransformer` followed by reading
/// `DefaultEndpointsForChannelType(c.Type)`. Returns the plan plus the
/// resolved default-endpoint list so the host can pass both to
/// [`build_channel_with_outbounds`].
pub fn plan_channel_build(
    defaults: &DefaultEndpointRegistry,
    input: &ChannelBuildInput<'_>,
) -> Result<(ChannelBuildPlan, Vec<ChannelEndpoint>), ChannelBuildError> {
    let plan = build_channel_with_transformer(input)?;
    let default_endpoints = defaults
        .get(input.channel_type)
        .map(|slice| slice.to_vec())
        .unwrap_or_default();
    Ok((plan, default_endpoints))
}
