//! S09/S14 endpoint resolution: merge user endpoints over channel-type
//! defaults, validate them, and look up the per-channel-type default registry.
//!
//! Ported from Go `internal/server/biz/channel_endpoint.go`:
//! - `mergeEndpoints` (user override + remaining defaults + remaining user
//!   extras).
//! - `ResolveEndpoints` (the merge wrapped in a `Vec`).
//! - `ValidateEndpoints` (api_format / path / transport checks).
//! - `DefaultEndpointsForChannelType` (the static per-type table, exposed here
//!   as a runtime-populated [`DefaultEndpointRegistry`] so the host can wire
//!   the transformer registry's known api_formats without baking the Go enum
//!   table into this pure module).

use std::collections::BTreeMap;

use conduit_core::objects::channel_settings::{ChannelEndpoint, channel_endpoint_transport};
use conduit_llm::ApiFormat;
use conduit_transformers::registry::{
    DirectProvider, ProviderDescriptor, ProviderFamily, known_channel_types, provider_descriptor,
};

/// Build the primary endpoint implied by a channel type's provider descriptor.
///
/// A known provider must expose its primary protocol even when a caller builds
/// a [`ChannelService`](super::ChannelService) without an explicit endpoint
/// registry, so candidate selection never collapses to an empty `api_format`.
/// The inferred endpoint deliberately leaves `path` empty. Descriptor
/// `base_path` values such as `/chat/completions` assume provider-specific base
/// URL normalization (for example adding `/v1`) and therefore belong to the
/// outbound transformer. Only a user-configured endpoint path is an
/// authoritative path override.
pub fn default_endpoint_from_provider_descriptor(
    channel_type: impl AsRef<str>,
) -> Option<ChannelEndpoint> {
    let descriptor = provider_descriptor(channel_type.as_ref())?;
    Some(ChannelEndpoint {
        api_format: primary_api_format(descriptor).as_str().to_string(),
        path: String::new(),
        base_url: String::new(),
        transport: channel_endpoint_transport::HTTP.to_string(),
    })
}

fn primary_api_format(descriptor: ProviderDescriptor) -> ApiFormat {
    match descriptor.family {
        ProviderFamily::Responses | ProviderFamily::Codex => ApiFormat::OpenAiResponses,
        ProviderFamily::ClaudeCode | ProviderFamily::Anthropic | ProviderFamily::AnthropicFake => {
            ApiFormat::AnthropicMessages
        }
        ProviderFamily::Gemini | ProviderFamily::GeminiVertex | ProviderFamily::Antigravity => {
            ApiFormat::GeminiContents
        }
        ProviderFamily::Jina => ApiFormat::JinaRerank,
        ProviderFamily::Direct if descriptor.direct_provider == Some(DirectProvider::Ollama) => {
            ApiFormat::OllamaChat
        }
        ProviderFamily::OpenAiCompatible
        | ProviderFamily::GithubCopilot
        | ProviderFamily::GeminiOpenAi
        | ProviderFamily::Direct
        | ProviderFamily::OpenAiFake => ApiFormat::OpenAiChatCompletions,
    }
}

/// Merge user-configured endpoints over per-channel-type defaults (S09/S14).
/// Ported 1:1 from Go `mergeEndpoints`.
///
/// - A user endpoint overrides the default with the same `api_format`
///   (override wins, consumed from the override set).
/// - Remaining defaults are appended in their declared order.
/// - Remaining user endpoints (extra `api_format`s with no default) are appended
///   in their original order.
/// - Endpoints with an empty `api_format` are dropped.
///
/// Returns `None` when both inputs are empty (Go returns `nil`).
pub fn merge_endpoints(
    default_endpoints: &[ChannelEndpoint],
    user_endpoints: &[ChannelEndpoint],
) -> Option<Vec<ChannelEndpoint>> {
    if default_endpoints.is_empty() && user_endpoints.is_empty() {
        return None;
    }

    let mut overrides: BTreeMap<String, ChannelEndpoint> = BTreeMap::new();
    for ep in user_endpoints {
        if ep.api_format.is_empty() {
            continue;
        }
        overrides.insert(ep.api_format.clone(), ep.clone());
    }

    let mut merged: Vec<ChannelEndpoint> =
        Vec::with_capacity(default_endpoints.len() + overrides.len());

    for ep in default_endpoints {
        if ep.api_format.is_empty() {
            continue;
        }
        if let Some(override_ep) = overrides.remove(&ep.api_format) {
            merged.push(override_ep);
        } else {
            merged.push(ep.clone());
        }
    }

    // Append surviving user endpoints (those whose api_format had no matching
    // default) in their original input order. `overrides.remove` is guaranteed
    // to yield the value because we only reach this loop for keys we did not
    // consume in the defaults pass.
    for ep in user_endpoints {
        if ep.api_format.is_empty() {
            continue;
        }
        if let Some(survivor) = overrides.remove(&ep.api_format) {
            merged.push(survivor);
        }
    }

    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

/// Resolve the runtime-effective endpoint list for a channel given the
/// channel-type defaults and the channel's own user endpoints. Ported from Go
/// `Channel.ResolveEndpoints`.
pub fn resolve_endpoints(
    default_endpoints: &[ChannelEndpoint],
    user_endpoints: &[ChannelEndpoint],
) -> Vec<ChannelEndpoint> {
    merge_endpoints(default_endpoints, user_endpoints).unwrap_or_default()
}

/// Validate channel endpoint configurations. Ported 1:1 from Go
/// `ValidateEndpoints` (without the websocket/api_format-supported checks,
/// which require the full `ApiFormat` registry and are deferred to the
/// transformer port).
///
/// Ensures:
/// - `api_format` is non-empty and unique within the channel,
/// - `path`, when set, starts with `/` and is not a full URL,
/// - `transport`, when set, is `"http"` or `"websocket"`.
pub fn validate_endpoints(endpoints: &[ChannelEndpoint]) -> Result<(), ChannelEndpointError> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (i, ep) in endpoints.iter().enumerate() {
        if ep.api_format.is_empty() {
            return Err(ChannelEndpointError::MissingApiFormat { index: i });
        }
        if seen.contains_key(&ep.api_format) {
            return Err(ChannelEndpointError::DuplicateApiFormat {
                index: i,
                api_format: ep.api_format.clone(),
            });
        }
        seen.insert(ep.api_format.clone(), i);

        if !ep.transport.is_empty()
            && ep.transport != channel_endpoint_transport::HTTP
            && ep.transport != channel_endpoint_transport::WEBSOCKET
        {
            return Err(ChannelEndpointError::UnsupportedTransport {
                index: i,
                transport: ep.transport.clone(),
            });
        }

        if !ep.path.is_empty() {
            if ep.path.starts_with("http://") || ep.path.starts_with("https://") {
                return Err(ChannelEndpointError::PathIsUrl {
                    index: i,
                    path: ep.path.clone(),
                });
            }
            if !ep.path.starts_with('/') {
                return Err(ChannelEndpointError::PathMissingSlash {
                    index: i,
                    path: ep.path.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Endpoint-validation error. Mirrors the Go `fmt.Errorf` messages from
/// `ValidateEndpoints`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelEndpointError {
    #[error("endpoint[{index}]: api_format is required")]
    MissingApiFormat { index: usize },
    #[error("endpoint[{index}]: duplicate api_format {api_format:?}")]
    DuplicateApiFormat { index: usize, api_format: String },
    #[error("endpoint[{index}]: unsupported transport {transport:?}")]
    UnsupportedTransport { index: usize, transport: String },
    #[error("endpoint[{index}]: path must not be a full URL, got {path:?}")]
    PathIsUrl { index: usize, path: String },
    #[error("endpoint[{index}]: path must start with '/', got {path:?}")]
    PathMissingSlash { index: usize, path: String },
}

/// Per-channel-type registry of built-in default endpoints (S09). Mirrors Go
/// `defaultEndpointsForChannelType` (keyed by the lowercase channel-type string
/// used by the `channel.Type` enum).
#[derive(Debug, Clone, Default)]
pub struct DefaultEndpointRegistry {
    table: BTreeMap<String, Vec<ChannelEndpoint>>,
}

impl DefaultEndpointRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Populate a primary default endpoint for every known provider type.
    ///
    /// This is the production-safe baseline when no host-specific registry is
    /// supplied. Each row is derived from the same provider descriptor used to
    /// choose the outbound family, keeping channel-type and endpoint defaults
    /// in one source of truth.
    pub fn from_provider_descriptors() -> Self {
        let mut registry = Self::new();
        for channel_type in known_channel_types() {
            if let Some(endpoint) = default_endpoint_from_provider_descriptor(channel_type) {
                registry.register(channel_type, vec![endpoint]);
            }
        }
        registry
    }

    /// Register the default endpoint list for a channel type. Lowercases the
    /// key to match Go's enum-string comparison.
    pub fn register(
        &mut self,
        channel_type: impl AsRef<str>,
        endpoints: Vec<ChannelEndpoint>,
    ) -> &mut Self {
        self.table
            .insert(channel_type.as_ref().to_ascii_lowercase(), endpoints);
        self
    }

    /// Look up the default endpoints for a channel type (`None` if unknown).
    /// Mirrors Go `DefaultEndpointsForChannelType`.
    pub fn get(&self, channel_type: impl AsRef<str>) -> Option<&[ChannelEndpoint]> {
        self.table
            .get(&channel_type.as_ref().to_ascii_lowercase())
            .map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_descriptor_yields_primary_endpoint_metadata() {
        let cases = [
            ("openai", "openai/chat_completions"),
            ("openai_responses", "openai/responses"),
            ("anthropic", "anthropic/messages"),
            ("gemini", "gemini/contents"),
            ("jina", "jina/rerank"),
            ("ollama", "ollama/chat"),
        ];

        for (channel_type, api_format) in cases {
            let endpoint = default_endpoint_from_provider_descriptor(channel_type)
                .unwrap_or_else(|| panic!("missing descriptor endpoint for {channel_type}"));
            assert_eq!(endpoint.api_format, api_format, "{channel_type}");
            assert!(endpoint.path.is_empty(), "{channel_type}");
            assert_eq!(endpoint.transport, channel_endpoint_transport::HTTP);
            assert!(endpoint.base_url.is_empty());
        }
    }

    #[test]
    fn unknown_provider_has_no_descriptor_endpoint() {
        assert_eq!(
            default_endpoint_from_provider_descriptor("not-a-provider"),
            None
        );
    }

    #[test]
    fn provider_registry_contains_defaults_for_known_channel_types() {
        let registry = DefaultEndpointRegistry::from_provider_descriptors();

        assert_eq!(
            registry.get("openai").and_then(|items| items.first()),
            default_endpoint_from_provider_descriptor("openai").as_ref()
        );
        assert_eq!(
            registry
                .get("ANTHROPIC")
                .and_then(|items| items.first())
                .map(|endpoint| endpoint.api_format.as_str()),
            Some("anthropic/messages")
        );
        assert!(
            known_channel_types()
                .into_iter()
                .all(|channel_type| registry.get(channel_type).is_some())
        );
    }
}
