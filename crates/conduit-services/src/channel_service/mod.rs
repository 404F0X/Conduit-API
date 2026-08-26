//! [`ChannelService`] module entry point. Re-exports the pure-logic decision
//! helpers that live in focused submodules mirroring the Go biz-package split
//! (`internal/server/biz/channel_*.go`).
//!
//! Submodule layout (file -> Go source):
//! - [`model_sync`]     -> `channel_model_sync.go` + `channel_llm.go::GetModelEntries`
//! - [`endpoint`]       -> `channel_endpoint.go`
//! - [`settings_merge`] -> `channel_override.go` (settings-layer merge)
//! - [`rate_limit`]     -> `channel_rate_limit.go` + `channel.go::NormalizeRetryable*`
//! - [`credentials`]    -> `channel_apikey.go` + `channel_apikey_provider.go`
//! - [`build`]          -> `channel_llm.go` (build path: plan + outbound map)
//! - [`auto_disable`]   -> `channel_auto_disable.go` + `channel_metrics.go::deriveErrorMessage`
//! - [`probe`]          -> `channel_probe.go` + `system.go::ChannelProbeSetting`
//! - [`bulk`]           -> `channel_bulk.go`
//! - [`list_models`]    -> `channel.go::ListModels` (pure aggregation)
//!
//! The [`ChannelService`] struct + [`ChannelLogic`] trait below are the thin
//! orchestrator-facing entry point that delegates to those submodule helpers.
//! All previously-monolithic items (S05/S06/S07/S08/S09/S11/S15) are still
//! reachable at `crate::channel_service::*` via the `pub use <sub>::*;`
//! re-exports, so external callers (`conduit-orchestrator`,
//! `conduit-scheduler`, …) keep their existing import paths.

use conduit_core::objects::channel_settings::{ChannelEndpoint, ChannelSettings};

// Re-export the repo traits so callers can `use conduit_services::channel_service::*`.
pub use conduit_db::repo::{ChannelRepo, SystemRepo};

pub mod auto_disable;
pub mod build;
pub mod bulk;
pub mod credentials;
pub mod endpoint;
pub mod list_models;
pub mod model_sync;
pub mod probe;
pub mod rate_limit;
pub mod settings_merge;

pub use auto_disable::*;
pub use build::*;
pub use bulk::*;
pub use credentials::*;
pub use endpoint::*;
pub use list_models::*;
pub use model_sync::*;
pub use probe::*;
pub use rate_limit::*;
pub use settings_merge::*;

/// Pure-logic channel helpers parameterized on the [`ChannelRepo`] /
/// [`SystemRepo`] traits. The struct itself is stateless beyond the
/// [`DefaultEndpointRegistry`]; the traits are carried as type parameters so
/// the same helpers compose against in-memory and DB-backed repos.
///
/// This implements the pure-logic subset (S05/S06/S09/S14/S15) of Go
/// `ChannelService`. DB-touching operations (CRUD, model sync scheduling,
/// cache refresh) live on the repo layer and are out of scope here. The
/// finer-grained decisions (credential selection, build planning,
/// auto-disable, probe, bulk) live on dedicated submodules and are surfaced
/// here via re-export.
#[derive(Debug, Clone, Default)]
pub struct ChannelService {
    defaults: DefaultEndpointRegistry,
}

impl ChannelService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_defaults(mut self, defaults: DefaultEndpointRegistry) -> Self {
        self.defaults = defaults;
        self
    }

    pub fn defaults(&self) -> &DefaultEndpointRegistry {
        &self.defaults
    }

    /// S05: merged supported-model list for a channel.
    pub fn supported_models(&self, set: &SupportedModelSet) -> Vec<String> {
        set.merged_models()
    }

    /// S06: resolved model-entry map for a channel.
    pub fn model_entry_map(
        &self,
        supported_models: &[String],
        settings: &ChannelSettings,
    ) -> ChannelModelEntryMap {
        ChannelModelEntryMap::from_channel(supported_models, settings)
    }

    /// S09/S14: resolved endpoint list for a channel (defaults merged with the
    /// channel's user endpoints).
    pub fn resolve_endpoints(
        &self,
        channel_type: impl AsRef<str>,
        user_endpoints: &[ChannelEndpoint],
    ) -> Vec<ChannelEndpoint> {
        let channel_type = channel_type.as_ref();
        let defaults = self.defaults.get(channel_type).unwrap_or(&[]);
        let resolved = resolve_endpoints(defaults, user_endpoints);
        if resolved.is_empty() {
            default_endpoint_from_provider_descriptor(channel_type)
                .into_iter()
                .collect()
        } else {
            resolved
        }
    }

    /// S09: select the resolved endpoint matching `api_format`, if any. Mirrors
    /// the lookup performed by Go's outbound builder when picking a transformer
    /// by API format.
    pub fn endpoint_for_api_format<'a>(
        &self,
        resolved: &'a [ChannelEndpoint],
        api_format: impl AsRef<str>,
    ) -> Option<&'a ChannelEndpoint> {
        let api_format = api_format.as_ref();
        resolved.iter().find(|ep| ep.api_format == api_format)
    }

    /// S15: merged settings layers (`system < model < channel < request`).
    pub fn merged_settings(
        &self,
        system_default: &ChannelSettings,
        model_setting: &ChannelSettings,
        channel_setting: &ChannelSettings,
        request_override: &ChannelSettings,
    ) -> ChannelSettings {
        merge_settings_layers(
            system_default,
            model_setting,
            channel_setting,
            request_override,
        )
    }
}

/// Repo-trait-bound channel service entry point used by the orchestrator. The
/// bound is a single method so any `ChannelRepo + SystemRepo` impl (including
/// in-memory fakes) satisfies it; methods here are thin pure-logic wrappers and
/// do not perform IO themselves, keeping the trait generic and testable.
#[async_trait::async_trait]
pub trait ChannelLogic: Send + Sync {
    /// Compute the supported-model view (S05) for one channel's source fields.
    fn supported_models(&self, set: &SupportedModelSet) -> Vec<String>;

    /// Compute the resolved model-entry map (S06).
    fn model_entry_map(
        &self,
        supported_models: &[String],
        settings: &ChannelSettings,
    ) -> ChannelModelEntryMap;

    /// Resolve endpoints (S09/S14).
    fn resolve_endpoints(
        &self,
        channel_type: &str,
        user_endpoints: &[ChannelEndpoint],
    ) -> Vec<ChannelEndpoint>;

    /// Merge settings layers (S15).
    fn merged_settings(
        &self,
        system_default: &ChannelSettings,
        model_setting: &ChannelSettings,
        channel_setting: &ChannelSettings,
        request_override: &ChannelSettings,
    ) -> ChannelSettings;
}

#[async_trait::async_trait]
impl ChannelLogic for ChannelService {
    fn supported_models(&self, set: &SupportedModelSet) -> Vec<String> {
        ChannelService::supported_models(self, set)
    }

    fn model_entry_map(
        &self,
        supported_models: &[String],
        settings: &ChannelSettings,
    ) -> ChannelModelEntryMap {
        ChannelService::model_entry_map(self, supported_models, settings)
    }

    fn resolve_endpoints(
        &self,
        channel_type: &str,
        user_endpoints: &[ChannelEndpoint],
    ) -> Vec<ChannelEndpoint> {
        ChannelService::resolve_endpoints(self, channel_type, user_endpoints)
    }

    fn merged_settings(
        &self,
        system_default: &ChannelSettings,
        model_setting: &ChannelSettings,
        channel_setting: &ChannelSettings,
        request_override: &ChannelSettings,
    ) -> ChannelSettings {
        ChannelService::merged_settings(
            self,
            system_default,
            model_setting,
            channel_setting,
            request_override,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::objects::channel_settings::{
        ChannelEndpoint, ChannelRateLimit, ChannelSettings, HeaderEntry, ModelMapping,
        RetryableErrorPattern, TransformOptions, channel_endpoint_transport,
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    // ---------- S05: supported models merge ----------

    #[test]
    fn s05_merges_supported_manual_and_auto_sync_with_dedup() {
        let set = SupportedModelSet::new(
            vec!["gpt-4o".to_string()],
            vec!["gpt-4o".to_string(), "manual-only".to_string()],
            vec!["manual-only".to_string(), "auto-only".to_string()],
        );

        assert_eq!(
            set.merged_models(),
            vec!["gpt-4o", "manual-only", "auto-only"]
        );
    }

    #[test]
    fn s05_filters_auto_sync_models_by_regex_pattern() {
        let substring = SupportedModelSet::new(
            Vec::<String>::new(),
            Vec::<String>::new(),
            vec![
                "gpt-4o".to_string(),
                "text-embedding-3-small".to_string(),
                "claude-3-5-sonnet".to_string(),
            ],
        )
        .with_auto_sync_model_pattern("gpt-4o");

        let anchored = SupportedModelSet::new(
            Vec::<String>::new(),
            Vec::<String>::new(),
            vec![
                "gpt-4o".to_string(),
                "gpt-4o-mini".to_string(),
                "claude-3-5-sonnet".to_string(),
            ],
        )
        .with_auto_sync_model_pattern("^gpt-4o$");

        let prefix_class = SupportedModelSet::new(
            vec!["manual-keep".to_string()],
            Vec::<String>::new(),
            vec![
                "gpt-4o".to_string(),
                "gpt-4.1-mini".to_string(),
                "claude-3-5-sonnet".to_string(),
            ],
        )
        .with_auto_sync_model_pattern("^gpt-4.*mini$");

        assert_eq!(substring.merged_models(), vec!["gpt-4o"]);
        assert_eq!(anchored.merged_models(), vec!["gpt-4o"]);
        assert_eq!(
            prefix_class.merged_models(),
            vec!["manual-keep", "gpt-4.1-mini"]
        );
    }

    #[test]
    fn s05_invalid_regex_falls_back_to_keep_all() {
        let set = SupportedModelSet::new(
            Vec::<String>::new(),
            Vec::<String>::new(),
            vec!["a".to_string(), "b".to_string()],
        )
        .with_auto_sync_model_pattern("(unclosed");

        assert_eq!(set.merged_models(), vec!["a", "b"]);
    }

    #[test]
    fn s05_empty_pattern_keeps_all_auto_sync_models() {
        let set = SupportedModelSet::new(
            Vec::<String>::new(),
            Vec::<String>::new(),
            vec!["a".to_string(), "b".to_string()],
        )
        .with_auto_sync_model_pattern("");

        assert_eq!(set.merged_models(), vec!["a", "b"]);
    }

    #[test]
    fn s05_ignores_blank_and_whitespace_model_entries() {
        let set = SupportedModelSet::new(
            vec!["  ".to_string()],
            vec!["gpt-4o".to_string()],
            vec!["".to_string()],
        );

        assert_eq!(set.merged_models(), vec!["gpt-4o"]);
    }

    // ---------- S06: model entry map ----------

    fn settings_with_prefix(prefix: &str) -> ChannelSettings {
        ChannelSettings {
            extra_model_prefix: prefix.to_string(),
            ..ChannelSettings::default()
        }
    }

    #[test]
    fn s06_direct_models_become_identity_entries() {
        let settings = ChannelSettings::default();
        let map = ChannelModelEntryMap::from_channel(
            &["gpt-4o".to_string(), "claude-3-5".to_string()],
            &settings,
        );

        let gpt = map.get("gpt-4o");
        assert_eq!(
            gpt.map(|e| (e.actual_model.as_str(), e.source)),
            Some(("gpt-4o", ModelSource::Direct))
        );
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn s06_extra_prefix_adds_alias_resolving_to_bare_model() {
        let settings = settings_with_prefix("openai");
        let map = ChannelModelEntryMap::from_channel(&["gpt-4o".to_string()], &settings);

        let prefixed = map.get("openai/gpt-4o");
        assert_eq!(
            prefixed.map(|e| (e.actual_model.as_str(), e.source)),
            Some(("gpt-4o", ModelSource::Prefix))
        );
        // Bare direct entry still present.
        assert!(map.get("gpt-4o").is_some());
    }

    #[test]
    fn s06_auto_trim_exposes_trimmed_tail_alias() {
        let settings = ChannelSettings {
            auto_trimed_model_prefixes: vec!["openai".to_string()],
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["openai/gpt-4o".to_string()], &settings);

        let trimmed = map.get("gpt-4o");
        assert_eq!(
            trimmed.map(|e| (e.actual_model.as_str(), e.source)),
            Some(("openai/gpt-4o", ModelSource::AutoTrim))
        );
    }

    #[test]
    fn s06_auto_trim_ignores_empty_prefix_entries() {
        let settings = ChannelSettings {
            auto_trimed_model_prefixes: vec!["".to_string(), "openai".to_string()],
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["openai/gpt-4o".to_string()], &settings);

        assert_eq!(map.len(), 2); // direct + trimmed
        assert!(map.get("gpt-4o").is_some());
    }

    #[test]
    fn s06_model_mapping_admitted_only_when_target_is_supported() {
        let settings = ChannelSettings {
            model_mappings: vec![
                ModelMapping {
                    from: "my-claude".to_string(),
                    to: "claude-3-5".to_string(),
                },
                ModelMapping {
                    from: "ghost".to_string(),
                    to: "not-supported".to_string(),
                },
            ],
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["claude-3-5".to_string()], &settings);

        let mapped = map.get("my-claude");
        assert_eq!(
            mapped.map(|e| (e.actual_model.as_str(), e.source)),
            Some(("claude-3-5", ModelSource::Mapping))
        );
        assert!(map.get("ghost").is_none());
    }

    #[test]
    fn s06_hide_original_models_removes_direct_entries() {
        let settings = ChannelSettings {
            extra_model_prefix: "openai".to_string(),
            hide_original_models: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["gpt-4o".to_string()], &settings);

        assert!(map.get("gpt-4o").is_none()); // direct removed
        assert!(map.get("openai/gpt-4o").is_some()); // prefix alias kept
    }

    #[test]
    fn s06_hide_mapped_models_removes_aliases_of_mapped_target() {
        let settings = ChannelSettings {
            extra_model_prefix: "openai".to_string(),
            model_mappings: vec![ModelMapping {
                from: "alias".to_string(),
                to: "gpt-4o".to_string(),
            }],
            hide_mapped_models: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["gpt-4o".to_string()], &settings);

        // Direct + prefix aliases of gpt-4o are removed; mapping entry kept.
        assert!(map.get("gpt-4o").is_none());
        assert!(map.get("openai/gpt-4o").is_none());
        let mapping = map.get("alias");
        assert_eq!(
            mapping.map(|e| (e.actual_model.as_str(), e.source)),
            Some(("gpt-4o", ModelSource::Mapping))
        );
    }

    #[test]
    fn s06_lowercase_model_id_normalizes_keys_with_source_priority() {
        // A supported model whose alias would lowercase-collide with the direct
        // entry. The direct entry must win over the prefix alias by source
        // priority (direct > prefix).
        let settings = ChannelSettings {
            extra_model_prefix: "x".to_string(),
            lowercase_model_id: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["gpt".to_string()], &settings);

        // "gpt" (direct) and "x/gpt" (prefix) both lowercase to themselves
        // here, so both keys survive; the direct wins any same-key collision.
        let direct = map.get("gpt");
        assert_eq!(
            direct.map(|e| (e.actual_model.as_str(), e.source)),
            Some(("gpt", ModelSource::Direct))
        );
        let prefix = map.get("x/gpt");
        assert_eq!(
            prefix.map(|e| (e.actual_model.as_str(), e.source)),
            Some(("gpt", ModelSource::Prefix))
        );

        // When two entries truly collide after lowercasing, higher source
        // priority wins. Use a prefix that lowercases onto the direct key.
        let settings_collision = ChannelSettings {
            extra_model_prefix: "G".to_string(),
            lowercase_model_id: true,
            ..ChannelSettings::default()
        };
        // Supported model "pt" -> prefix alias "G/pt" lowercases to "g/pt";
        // supported model "g/pt" -> direct lowercases to "g/pt". Collision.
        let map_collision = ChannelModelEntryMap::from_channel(
            &["g/pt".to_string(), "pt".to_string()],
            &settings_collision,
        );

        let winner = map_collision.get("g/pt");
        assert_eq!(
            winner.map(|e| (e.actual_model.as_str(), e.source)),
            // Direct entry (actual "g/pt") wins over prefix alias (actual "pt").
            Some(("g/pt", ModelSource::Direct))
        );
    }

    // ---------- S06 (extended): additional Go TestChannel_GetUnifiedModels
    // cases (channel_model_entry_test.go) not yet covered above. ----------

    /// Helper: collect the map into a sorted vec for elements-match comparison,
    /// mirroring Go's `require.ElementsMatch` on `[]ChannelModelEntry`.
    fn entry_set(map: &ChannelModelEntryMap) -> Vec<(&str, &str, ModelSource)> {
        // BTreeMap iterates by request_model key in sorted order, which is the
        // same order Go's ElementsMatch would compare against after sorting by
        // request_model. This gives us deterministic, comparable output without
        // needing Ord on ModelSource itself.
        map.iter()
            .map(|(_, e)| (e.request_model.as_str(), e.actual_model.as_str(), e.source))
            .collect()
    }

    /// Go `TestChannel_GetUnifiedModels` "combined: all features" (conduit
    /// channel_model_entry_test.go:83). All four sources must coexist with the
    /// documented tie-breaking when a request key would collide.
    #[test]
    fn s06_combined_all_features_exposes_every_source() {
        let settings = ChannelSettings {
            extra_model_prefix: "custom".to_string(),
            auto_trimed_model_prefixes: vec!["openai".to_string()],
            model_mappings: vec![ModelMapping {
                from: "gpt4".to_string(),
                to: "openai/gpt-4".to_string(),
            }],
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(
            &["openai/gpt-4".to_string(), "deepseek-chat".to_string()],
            &settings,
        );

        let expected = vec![
            ("custom/deepseek-chat", "deepseek-chat", ModelSource::Prefix),
            ("custom/openai/gpt-4", "openai/gpt-4", ModelSource::Prefix),
            ("deepseek-chat", "deepseek-chat", ModelSource::Direct),
            ("gpt-4", "openai/gpt-4", ModelSource::AutoTrim),
            ("gpt4", "openai/gpt-4", ModelSource::Mapping),
            ("openai/gpt-4", "openai/gpt-4", ModelSource::Direct),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "no duplicates" (channel_model_entry_test.go:106). A mapping whose
    /// `From` equals an existing direct key must not produce a second entry;
    /// the direct entry wins by source priority.
    #[test]
    fn s06_no_duplicate_when_mapping_target_equals_direct() {
        let settings = ChannelSettings {
            model_mappings: vec![ModelMapping {
                from: "gpt-4".to_string(),
                to: "gpt-4".to_string(),
            }],
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["gpt-4".to_string()], &settings);

        assert_eq!(map.len(), 1);
        let entry = map.get("gpt-4");
        assert_eq!(
            entry.map(|e| (e.actual_model.as_str(), e.source)),
            Some(("gpt-4", ModelSource::Direct))
        );
    }

    /// Go "nil settings" (channel_model_entry_test.go:122). Default settings
    /// yield only direct identity entries.
    #[test]
    fn s06_default_settings_yield_direct_entries_only() {
        let map =
            ChannelModelEntryMap::from_channel(&["gpt-4".to_string()], &ChannelSettings::default());

        let expected = vec![("gpt-4", "gpt-4", ModelSource::Direct)];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "hideOriginalModels: with model mappings only"
    /// (channel_model_entry_test.go:133). Direct entries are stripped while
    /// mapping entries remain.
    #[test]
    fn s06_hide_original_with_mappings_only_keeps_mapping_entries() {
        let settings = ChannelSettings {
            model_mappings: vec![
                ModelMapping {
                    from: "gpt-4".to_string(),
                    to: "gpt-4-turbo".to_string(),
                },
                ModelMapping {
                    from: "gpt4".to_string(),
                    to: "gpt-4-turbo".to_string(),
                },
            ],
            hide_original_models: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["gpt-4-turbo".to_string()], &settings);

        let expected = vec![
            ("gpt-4", "gpt-4-turbo", ModelSource::Mapping),
            ("gpt4", "gpt-4-turbo", ModelSource::Mapping),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "hideOriginalModels: with auto-trimmed prefixes"
    /// (channel_model_entry_test.go:168). Direct keys for trimmed models are
    /// removed, leaving only the trimmed-tail aliases.
    #[test]
    fn s06_hide_original_with_auto_trim_keeps_trimmed_aliases() {
        let settings = ChannelSettings {
            auto_trimed_model_prefixes: vec!["openai".to_string(), "deepseek-ai".to_string()],
            hide_original_models: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(
            &[
                "openai/gpt-4".to_string(),
                "deepseek-ai/deepseek-chat".to_string(),
            ],
            &settings,
        );

        let expected = vec![
            (
                "deepseek-chat",
                "deepseek-ai/deepseek-chat",
                ModelSource::AutoTrim,
            ),
            ("gpt-4", "openai/gpt-4", ModelSource::AutoTrim),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "hideOriginalModels: combined features"
    /// (channel_model_entry_test.go:184). Direct entries stripped; prefix /
    /// auto_trim / mapping all retained.
    #[test]
    fn s06_hide_original_combined_features_keeps_non_direct_entries() {
        let settings = ChannelSettings {
            extra_model_prefix: "custom".to_string(),
            auto_trimed_model_prefixes: vec!["openai".to_string()],
            model_mappings: vec![ModelMapping {
                from: "gpt4".to_string(),
                to: "openai/gpt-4".to_string(),
            }],
            hide_original_models: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(
            &["openai/gpt-4".to_string(), "deepseek-chat".to_string()],
            &settings,
        );

        let expected = vec![
            ("custom/deepseek-chat", "deepseek-chat", ModelSource::Prefix),
            ("custom/openai/gpt-4", "openai/gpt-4", ModelSource::Prefix),
            ("gpt-4", "openai/gpt-4", ModelSource::AutoTrim),
            ("gpt4", "openai/gpt-4", ModelSource::Mapping),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "hideOriginalModels: false keeps direct models"
    /// (channel_model_entry_test.go:206). Explicitly false preserves direct
    /// entries alongside mappings.
    #[test]
    fn s06_hide_original_false_keeps_direct_models() {
        let settings = ChannelSettings {
            model_mappings: vec![ModelMapping {
                from: "gpt-4".to_string(),
                to: "gpt-4-turbo".to_string(),
            }],
            hide_original_models: false,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["gpt-4-turbo".to_string()], &settings);

        let expected = vec![
            ("gpt-4", "gpt-4-turbo", ModelSource::Mapping),
            ("gpt-4-turbo", "gpt-4-turbo", ModelSource::Direct),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "hideMappedModels: with extra prefix and multiple models"
    /// (channel_model_entry_test.go:259). When one model is mapping-target and
    /// another is unrelated, the unrelated model's direct + prefix entries
    /// survive; only the mapping target's aliases are stripped.
    #[test]
    fn s06_hide_mapped_with_prefix_and_multiple_models_preserves_unrelated() {
        let settings = ChannelSettings {
            extra_model_prefix: "proxy".to_string(),
            model_mappings: vec![ModelMapping {
                from: "gpt-4".to_string(),
                to: "gpt-4-turbo".to_string(),
            }],
            hide_mapped_models: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(
            &["gpt-4-turbo".to_string(), "claude-3".to_string()],
            &settings,
        );

        let expected = vec![
            ("claude-3", "claude-3", ModelSource::Direct),
            ("gpt-4", "gpt-4-turbo", ModelSource::Mapping),
            ("proxy/claude-3", "claude-3", ModelSource::Prefix),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "hideMappedModels: with prefix and auto-trim combined"
    /// (channel_model_entry_test.go:297). The mapping target's direct /
    /// prefix / auto_trim aliases are all removed while unrelated models keep
    /// theirs.
    #[test]
    fn s06_hide_mapped_with_prefix_and_auto_trim_combined_preserves_unrelated() {
        let settings = ChannelSettings {
            extra_model_prefix: "proxy".to_string(),
            auto_trimed_model_prefixes: vec!["openai".to_string()],
            model_mappings: vec![ModelMapping {
                from: "gpt-4".to_string(),
                to: "openai/gpt-4-turbo".to_string(),
            }],
            hide_mapped_models: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(
            &["openai/gpt-4-turbo".to_string(), "claude-3".to_string()],
            &settings,
        );

        let expected = vec![
            ("claude-3", "claude-3", ModelSource::Direct),
            ("gpt-4", "openai/gpt-4-turbo", ModelSource::Mapping),
            ("proxy/claude-3", "claude-3", ModelSource::Prefix),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    // ---------- S06 (extended): Go TestChannel_GetUnifiedModels_LowercaseModelID
    // collision / multi-feature cases (channel_model_entry_test.go:351). -----

    /// Go "with model mappings — mapping To preserves original casing"
    /// (channel_model_entry_test.go:387). Lowercasing applies to request keys;
    /// the underlying `actual_model` keeps its original casing.
    #[test]
    fn s06_lowercase_with_mapping_preserves_actual_casing() {
        let settings = ChannelSettings {
            model_mappings: vec![ModelMapping {
                from: "GPT-4".to_string(),
                to: "GPT-4-Turbo".to_string(),
            }],
            lowercase_model_id: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["GPT-4-Turbo".to_string()], &settings);

        let expected = vec![
            ("gpt-4", "GPT-4-Turbo", ModelSource::Mapping),
            ("gpt-4-turbo", "GPT-4-Turbo", ModelSource::Direct),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "with auto-trim and prefix — combined features"
    /// (channel_model_entry_test.go:419). Multiple trim prefixes layer; each
    /// resulting request key is lowercased while actuals retain original case.
    #[test]
    fn s06_lowercase_with_auto_trim_and_multiple_prefixes_layers_aliases() {
        let settings = ChannelSettings {
            auto_trimed_model_prefixes: vec!["Pro".to_string(), "Pro/zai-org".to_string()],
            lowercase_model_id: true,
            ..ChannelSettings::default()
        };
        let map =
            ChannelModelEntryMap::from_channel(&["Pro/zai-org/GLM-5.1".to_string()], &settings);

        let expected = vec![
            ("glm-5.1", "Pro/zai-org/GLM-5.1", ModelSource::AutoTrim),
            (
                "pro/zai-org/glm-5.1",
                "Pro/zai-org/GLM-5.1",
                ModelSource::Direct,
            ),
            (
                "zai-org/glm-5.1",
                "Pro/zai-org/GLM-5.1",
                ModelSource::AutoTrim,
            ),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "collision: direct beats mapping" (channel_model_entry_test.go:452).
    /// When a mapping's `From` lowercases to the same key as an existing direct
    /// entry, the direct entry wins by source priority.
    #[test]
    fn s06_lowercase_collision_direct_beats_mapping() {
        let settings = ChannelSettings {
            model_mappings: vec![ModelMapping {
                from: "gpt-4".to_string(),
                to: "GPT-4".to_string(),
            }],
            lowercase_model_id: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["GPT-4".to_string()], &settings);

        // The direct entry "GPT-4" lowercases to "gpt-4"; the mapping's `From`
        // is already "gpt-4" — they collide and direct wins.
        let expected = vec![("gpt-4", "GPT-4", ModelSource::Direct)];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "collision: mapping beats prefix" (channel_model_entry_test.go:469).
    /// On a lowercase collision, mapping (priority 2) beats prefix (priority 1).
    #[test]
    fn s06_lowercase_collision_mapping_beats_prefix() {
        let settings = ChannelSettings {
            extra_model_prefix: "ZAI".to_string(),
            model_mappings: vec![ModelMapping {
                from: "zai/GLM-5.1".to_string(),
                to: "GLM-5.1".to_string(),
            }],
            lowercase_model_id: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["GLM-5.1".to_string()], &settings);

        // Prefix alias "ZAI/GLM-5.1" lowercases to "zai/glm-5.1"; mapping's
        // `From` lowercases identically — collision, mapping wins.
        let expected = vec![
            ("glm-5.1", "GLM-5.1", ModelSource::Direct),
            ("zai/glm-5.1", "GLM-5.1", ModelSource::Mapping),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    /// Go "collision: auto_trim beats mapping" (channel_model_entry_test.go:488).
    /// On a lowercase collision, auto_trim (priority 3) beats mapping (priority 2).
    #[test]
    fn s06_lowercase_collision_auto_trim_beats_mapping() {
        let settings = ChannelSettings {
            auto_trimed_model_prefixes: vec!["ZAI".to_string()],
            model_mappings: vec![ModelMapping {
                from: "glm-5.1".to_string(),
                to: "ZAI/GLM-5.1".to_string(),
            }],
            lowercase_model_id: true,
            ..ChannelSettings::default()
        };
        let map = ChannelModelEntryMap::from_channel(&["ZAI/GLM-5.1".to_string()], &settings);

        // Auto_trim yields "glm-5.1" -> "ZAI/GLM-5.1"; mapping's `From` is also
        // "glm-5.1" — collision, auto_trim wins.
        let expected = vec![
            ("glm-5.1", "ZAI/GLM-5.1", ModelSource::AutoTrim),
            ("zai/glm-5.1", "ZAI/GLM-5.1", ModelSource::Direct),
        ];
        assert_eq!(entry_set(&map), expected);
    }

    // ---------- S09/S14: endpoint resolution ----------

    fn ep(api_format: &str, path: &str) -> ChannelEndpoint {
        ChannelEndpoint {
            api_format: api_format.to_string(),
            path: path.to_string(),
            base_url: String::new(),
            transport: String::new(),
        }
    }

    #[test]
    fn s09_merge_returns_none_when_both_inputs_empty() {
        assert_eq!(merge_endpoints(&[], &[]), None);
    }

    #[test]
    fn s09_user_endpoint_overrides_default_of_same_api_format() {
        let defaults = vec![ep("openai/chat_completions", "/v1/chat/completions")];
        let user = vec![ChannelEndpoint {
            api_format: "openai/chat_completions".to_string(),
            path: "/custom".to_string(),
            base_url: "https://proxy".to_string(),
            transport: String::new(),
        }];

        let merged = merge_endpoints(&defaults, &user).unwrap_or_default();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].path, "/custom");
        assert_eq!(merged[0].base_url, "https://proxy");
    }

    #[test]
    fn s09_user_endpoint_with_new_api_format_is_appended_after_defaults() {
        let defaults = vec![ep("openai/chat_completions", "/v1/chat/completions")];
        let user = vec![ep("anthropic/messages", "/v1/messages")];

        let merged = merge_endpoints(&defaults, &user).unwrap_or_default();
        assert_eq!(
            merged
                .iter()
                .map(|e| e.api_format.as_str())
                .collect::<Vec<_>>(),
            vec!["openai/chat_completions", "anthropic/messages"]
        );
    }

    #[test]
    fn s09_endpoints_with_empty_api_format_are_dropped() {
        let defaults = vec![ChannelEndpoint::default()];
        let user = vec![ep("openai/chat_completions", "/v1/chat/completions")];

        let merged = merge_endpoints(&defaults, &user).unwrap_or_default();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].api_format, "openai/chat_completions");
    }

    #[test]
    fn s09_resolve_endpoints_uses_registry_defaults() {
        let mut defaults = DefaultEndpointRegistry::new();
        defaults.register(
            "openai",
            vec![
                ep("openai/chat_completions", "/v1/chat/completions"),
                ep("openai/embeddings", "/v1/embeddings"),
            ],
        );
        let service = ChannelService::new().with_defaults(defaults);

        let resolved = service.resolve_endpoints("OpenAI", &[]); // case-insensitive
        assert_eq!(
            resolved
                .iter()
                .map(|e| e.api_format.as_str())
                .collect::<Vec<_>>(),
            vec!["openai/chat_completions", "openai/embeddings"]
        );
    }

    #[test]
    fn s09_unknown_channel_type_yields_just_user_endpoints() {
        let service = ChannelService::new();
        let user = vec![ep("anthropic/messages", "/v1/messages")];

        let resolved = service.resolve_endpoints("anthropic", &user);
        assert_eq!(
            resolved
                .iter()
                .map(|e| e.api_format.as_str())
                .collect::<Vec<_>>(),
            vec!["anthropic/messages"]
        );
    }

    #[test]
    fn s09_endpoint_for_api_format_selects_match() {
        let service = ChannelService::new();
        let resolved = service.resolve_endpoints(
            "openai",
            &[ep("openai/chat_completions", "/v1/chat/completions")],
        );

        assert_eq!(
            service
                .endpoint_for_api_format(&resolved, "openai/chat_completions")
                .map(|e| e.path.as_str()),
            Some("/v1/chat/completions")
        );
        assert!(
            service
                .endpoint_for_api_format(&resolved, "anthropic/messages")
                .is_none()
        );
    }

    // ---------- S09: validate_endpoints ----------

    #[test]
    fn s09_validate_rejects_empty_api_format() {
        let endpoints = vec![ChannelEndpoint::default()];
        assert_eq!(
            validate_endpoints(&endpoints),
            Err(ChannelEndpointError::MissingApiFormat { index: 0 })
        );
    }

    #[test]
    fn s09_validate_rejects_duplicate_api_format() {
        let endpoints = vec![
            ep("openai/chat_completions", "/v1/chat/completions"),
            ep("openai/chat_completions", "/other"),
        ];
        assert!(matches!(
            validate_endpoints(&endpoints),
            Err(ChannelEndpointError::DuplicateApiFormat { index: 1, .. })
        ));
    }

    #[test]
    fn s09_validate_rejects_full_url_path() {
        let endpoints = vec![ChannelEndpoint {
            api_format: "openai/chat_completions".to_string(),
            path: "https://example.com/v1".to_string(),
            ..ChannelEndpoint::default()
        }];
        assert!(matches!(
            validate_endpoints(&endpoints),
            Err(ChannelEndpointError::PathIsUrl { index: 0, .. })
        ));
    }

    #[test]
    fn s09_validate_rejects_path_without_leading_slash() {
        let endpoints = vec![ChannelEndpoint {
            api_format: "openai/chat_completions".to_string(),
            path: "v1/chat".to_string(),
            ..ChannelEndpoint::default()
        }];
        assert!(matches!(
            validate_endpoints(&endpoints),
            Err(ChannelEndpointError::PathMissingSlash { index: 0, .. })
        ));
    }

    #[test]
    fn s09_validate_rejects_unknown_transport() {
        let endpoints = vec![ChannelEndpoint {
            api_format: "openai/chat_completions".to_string(),
            transport: "carrier-pigeon".to_string(),
            ..ChannelEndpoint::default()
        }];
        assert!(matches!(
            validate_endpoints(&endpoints),
            Err(ChannelEndpointError::UnsupportedTransport { index: 0, .. })
        ));
    }

    #[test]
    fn s09_validate_accepts_http_and_websocket_transports() {
        let endpoints = vec![
            ChannelEndpoint {
                api_format: "openai/chat_completions".to_string(),
                transport: channel_endpoint_transport::HTTP.to_string(),
                ..ChannelEndpoint::default()
            },
            ChannelEndpoint {
                api_format: "openai/responses".to_string(),
                transport: channel_endpoint_transport::WEBSOCKET.to_string(),
                ..ChannelEndpoint::default()
            },
        ];
        assert!(validate_endpoints(&endpoints).is_ok());
    }

    // ---------- S15: settings merge ----------

    fn rate_limit(rpm: i64) -> Option<ChannelRateLimit> {
        Some(ChannelRateLimit {
            rpm: Some(rpm),
            ..ChannelRateLimit::default()
        })
    }

    #[test]
    fn s15_system_default_is_baseline() {
        let system = ChannelSettings {
            extra_model_prefix: "base".to_string(),
            rate_limit: rate_limit(60),
            ..ChannelSettings::default()
        };
        let merged = merge_settings_layers(
            &system,
            &ChannelSettings::default(),
            &ChannelSettings::default(),
            &ChannelSettings::default(),
        );

        assert_eq!(merged.extra_model_prefix, "base");
        assert_eq!(merged.rate_limit, rate_limit(60));
    }

    #[test]
    fn s15_higher_layer_overrides_scalar_and_option_fields() {
        let system = ChannelSettings {
            extra_model_prefix: "base".to_string(),
            rate_limit: rate_limit(60),
            ..ChannelSettings::default()
        };
        let channel = ChannelSettings {
            extra_model_prefix: "channel".to_string(),
            rate_limit: rate_limit(120),
            ..ChannelSettings::default()
        };
        let request = ChannelSettings {
            extra_model_prefix: "request".to_string(),
            ..ChannelSettings::default()
        };

        let merged =
            merge_settings_layers(&system, &ChannelSettings::default(), &channel, &request);

        assert_eq!(merged.extra_model_prefix, "request"); // request wins
        assert_eq!(merged.rate_limit, rate_limit(120)); // channel wins (request has None)
    }

    #[test]
    fn s15_vec_fields_extend_across_layers() {
        let system = ChannelSettings {
            retryable_status_codes: vec![429],
            ..ChannelSettings::default()
        };
        let channel = ChannelSettings {
            retryable_status_codes: vec![503],
            ..ChannelSettings::default()
        };

        let merged = merge_settings_layers(
            &system,
            &ChannelSettings::default(),
            &channel,
            &ChannelSettings::default(),
        );

        assert_eq!(merged.retryable_status_codes, vec![429, 503]);
    }

    #[test]
    fn s15_bool_flags_or_across_layers() {
        let system = ChannelSettings {
            hide_original_models: false,
            ..ChannelSettings::default()
        };
        let channel = ChannelSettings {
            lowercase_model_id: true,
            ..ChannelSettings::default()
        };
        let request = ChannelSettings {
            hide_original_models: true,
            ..ChannelSettings::default()
        };

        let merged =
            merge_settings_layers(&system, &ChannelSettings::default(), &channel, &request);

        assert!(merged.hide_original_models);
        assert!(merged.lowercase_model_id);
    }

    #[test]
    fn s15_model_mappings_extend_across_layers() {
        let system = ChannelSettings {
            model_mappings: vec![ModelMapping {
                from: "a".to_string(),
                to: "alpha".to_string(),
            }],
            ..ChannelSettings::default()
        };
        let channel = ChannelSettings {
            model_mappings: vec![ModelMapping {
                from: "b".to_string(),
                to: "beta".to_string(),
            }],
            ..ChannelSettings::default()
        };

        let merged = merge_settings_layers(
            &system,
            &ChannelSettings::default(),
            &channel,
            &ChannelSettings::default(),
        );

        assert_eq!(merged.model_mappings.len(), 2);
    }

    #[test]
    fn s15_transform_options_or_per_flag() {
        let system = TransformOptions {
            force_array_instructions: true,
            ..TransformOptions::default()
        };
        let channel = TransformOptions {
            replace_developer_role_with_system: true,
            ..TransformOptions::default()
        };
        let system_settings = ChannelSettings {
            transform_options: system,
            ..ChannelSettings::default()
        };
        let channel_settings = ChannelSettings {
            transform_options: channel,
            ..ChannelSettings::default()
        };

        let merged = merge_settings_layers(
            &system_settings,
            &ChannelSettings::default(),
            &channel_settings,
            &ChannelSettings::default(),
        );

        assert!(merged.transform_options.force_array_instructions);
        assert!(merged.transform_options.replace_developer_role_with_system);
        assert!(!merged.transform_options.force_array_inputs);
    }

    #[test]
    fn s15_full_round_trip_through_service_helper() {
        let service = ChannelService::new();
        let system = ChannelSettings {
            override_parameters: r#"{"temperature":0.1}"#.to_string(),
            retryable_error_patterns: vec![RetryableErrorPattern {
                pattern: "overloaded".to_string(),
                regex: false,
            }],
            ..ChannelSettings::default()
        };
        let model = ChannelSettings {
            override_headers: vec![HeaderEntry {
                key: "X-Model".to_string(),
                value: "on".to_string(),
            }],
            ..ChannelSettings::default()
        };
        let channel = ChannelSettings {
            rate_limit: rate_limit(30),
            ..ChannelSettings::default()
        };
        let request = ChannelSettings {
            extra_model_prefix: "req".to_string(),
            ..ChannelSettings::default()
        };

        let merged = service.merged_settings(&system, &model, &channel, &request);

        assert_eq!(merged.override_parameters, r#"{"temperature":0.1}"#);
        assert_eq!(merged.retryable_error_patterns.len(), 1);
        assert_eq!(merged.override_headers.len(), 1);
        assert_eq!(merged.rate_limit, rate_limit(30));
        assert_eq!(merged.extra_model_prefix, "req");
    }

    // ---------- ChannelLogic trait dispatch ----------

    #[test]
    fn channel_logic_trait_dispatches_to_service() {
        let service = ChannelService::new();
        let logic: Box<dyn ChannelLogic> = Box::new(service);

        let set = SupportedModelSet::new(vec!["gpt-4o".to_string()], Vec::new(), Vec::new());
        assert_eq!(logic.supported_models(&set), vec!["gpt-4o".to_string()]);

        let map = logic.model_entry_map(&["gpt-4o".to_string()], &ChannelSettings::default());
        assert!(map.get("gpt-4o").is_some());

        assert!(
            logic
                .resolve_endpoints("openai", &[ep("openai/chat_completions", "/v1")])
                .iter()
                .any(|e| e.api_format == "openai/chat_completions")
        );

        let merged = logic.merged_settings(
            &ChannelSettings::default(),
            &ChannelSettings::default(),
            &ChannelSettings::default(),
            &ChannelSettings::default(),
        );
        assert_eq!(merged, ChannelSettings::default());
    }

    // ---------- ModelSource ----------

    #[test]
    fn model_source_priority_order_matches_go() {
        assert!(ModelSource::Direct.priority() > ModelSource::AutoTrim.priority());
        assert!(ModelSource::AutoTrim.priority() > ModelSource::Mapping.priority());
        assert!(ModelSource::Mapping.priority() > ModelSource::Prefix.priority());
        assert_eq!(ModelSource::AutoTrim.as_str(), "auto_trim");
    }

    #[test]
    fn model_source_round_trips_through_json() -> Result<(), serde_json::Error> {
        let entry = ChannelModelEntry {
            request_model: "x".to_string(),
            actual_model: "y".to_string(),
            source: ModelSource::AutoTrim,
        };
        let json = serde_json::to_value(&entry)?;
        assert_eq!(json["source"], json!("auto_trim"));
        let back: ChannelModelEntry = serde_json::from_value(json)?;
        assert_eq!(back.source, ModelSource::AutoTrim);
        Ok(())
    }

    // ---------- S07: credential selection ----------

    fn snap<'a>(keys: &'a [String]) -> CredentialSnapshot<'a> {
        CredentialSnapshot {
            enabled_api_keys: keys,
            legacy_api_key: "",
            is_oauth: false,
            has_azure: false,
            has_gcp: false,
            override_key: "",
        }
    }

    fn trace_none() -> TraceKeyState<'static> {
        TraceKeyState {
            trace_id: None,
            cached_sticky_key: None,
        }
    }

    fn trace_with<'a>(id: &'a str, cached: Option<&'a str>) -> TraceKeyState<'a> {
        TraceKeyState {
            trace_id: Some(id),
            cached_sticky_key: cached,
        }
    }

    #[test]
    fn s07_override_key_always_wins() {
        // Mirrors Go getAPIKeyProvider's apiKeyOverride short-circuit.
        let keys = vec!["k1".to_string(), "k2".to_string()];
        let mut s = snap(&keys);
        s.override_key = "force-me";
        let decision = decide_credential(&s, &trace_with("t1", None));
        assert_eq!(decision.kind, CredentialKind::ApiKey);
        assert_eq!(decision.api_key, Some("force-me"));
        assert!(!decision.keys_exhausted);
    }

    #[test]
    fn s07_oauth_channel_returns_oauth_kind_no_key() {
        // Mirrors Go's `IsOAuth()` branch in buildXxxOutbound: the credential
        // kind is OAuth and per-key rotation is skipped.
        let s = CredentialSnapshot {
            enabled_api_keys: &[],
            legacy_api_key: "{\"access_token\":\"x\"}",
            is_oauth: true,
            has_azure: false,
            has_gcp: false,
            override_key: "",
        };
        let decision = decide_credential(&s, &trace_none());
        assert_eq!(decision.kind, CredentialKind::OAuth);
        assert_eq!(decision.api_key, None);
        assert!(!decision.keys_exhausted);
    }

    #[test]
    fn s07_azure_and_gcp_dispatch() {
        let azure = CredentialSnapshot {
            enabled_api_keys: &[],
            legacy_api_key: "",
            is_oauth: false,
            has_azure: true,
            has_gcp: false,
            override_key: "",
        };
        assert_eq!(
            decide_credential(&azure, &trace_none()).kind,
            CredentialKind::Azure
        );

        let gcp = CredentialSnapshot {
            enabled_api_keys: &[],
            legacy_api_key: "",
            is_oauth: false,
            has_azure: false,
            has_gcp: true,
            override_key: "",
        };
        assert_eq!(
            decide_credential(&gcp, &trace_none()).kind,
            CredentialKind::Gcp
        );
    }

    #[test]
    fn s07_single_enabled_key_is_static() {
        // Mirrors Go NewStaticKeyProvider(enabled[0]) when len(enabled)==1.
        let keys = vec!["only-key".to_string()];
        let decision = decide_credential(&snap(&keys), &trace_none());
        assert_eq!(decision.api_key, Some("only-key"));
        assert!(!decision.keys_exhausted);
    }

    #[test]
    fn s07_multi_key_no_trace_picks_first_deterministically() {
        // Go uses rand.IntN here; a pure function cannot, so we pick the first
        // (documented behaviour). Same-trace stability is covered by the
        // sticky test below.
        let keys = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let decision = decide_credential(&snap(&keys), &trace_none());
        assert_eq!(decision.api_key, Some("a"));
        assert!(!decision.keys_exhausted);
    }

    #[test]
    fn s07_multi_key_same_trace_is_sticky() {
        // Mirrors Go TestTraceStickyKeyProvider_MultipleKeys_WithTrace_Sticky:
        // the same trace id must pick the same key across calls.
        let keys = vec![
            "key-1".to_string(),
            "key-2".to_string(),
            "key-3".to_string(),
        ];
        let trace = trace_with("trace-abc-123", None);
        let k1 = decide_credential(&snap(&keys), &trace).api_key;
        let k2 = decide_credential(&snap(&keys), &trace).api_key;
        assert_eq!(k1, k2);
        let chosen = k1.unwrap_or("UNEXPECTED_NONE");
        assert!(keys.iter().any(|k| k.as_str() == chosen));
    }

    #[test]
    fn s07_cached_sticky_key_is_reused_when_still_enabled() {
        // Mirrors Go's LRU cache hit path: when the cached pick is still in
        // the enabled set, reuse it verbatim (skip rendezvous).
        let keys = vec!["k1".to_string(), "k2".to_string(), "k3".to_string()];
        let cached = "k2";
        let trace = trace_with("trace-x", Some(cached));
        let decision = decide_credential(&snap(&keys), &trace);
        assert_eq!(decision.api_key, Some("k2"));
    }

    #[test]
    fn s07_cached_sticky_key_repicked_when_disabled() {
        // Mirrors Go's DisableKey_StickyByRemoval test: when the cached key
        // is no longer enabled, rendezvous re-picks among the survivors.
        let keys_all = vec!["k1".to_string(), "k2".to_string(), "k3".to_string()];
        let trace = trace_with("trace-x", Some("k2"));
        // First pick some key with k2 present (it would be reused).
        let with_k2 = decide_credential(&snap(&keys_all), &trace).api_key;
        assert_eq!(with_k2, Some("k2"));

        // Now simulate k2 being disabled — enabled list excludes it.
        let survivors = vec!["k1".to_string(), "k3".to_string()];
        let decision = decide_credential(&snap(&survivors), &trace);
        let chosen = decision.api_key.unwrap_or("UNEXPECTED_NONE");
        // Must be one of the survivors, never the disabled key.
        assert!(chosen == "k1" || chosen == "k3");
        assert_ne!(chosen, "k2");
    }

    #[test]
    fn s07_rendezvous_is_deterministic_for_same_inputs() {
        // Mirrors Go TestRendezvousSelect_Deterministic.
        let keys = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let r1 = rendezvous_select(&keys, "seed-1");
        let r2 = rendezvous_select(&keys, "seed-1");
        assert_eq!(r1, r2);
    }

    #[test]
    fn s07_rendezvous_different_seeds_may_differ() {
        // Mirrors Go TestRendezvousSelect_DifferentSeeds (relaxed: at least
        // two distinct winners across the seeds).
        let keys = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
            "e".to_string(),
        ];
        let mut winners = std::collections::HashSet::new();
        for i in 0..30 {
            winners.insert(rendezvous_select(&keys, &format!("seed-{i}")).map(str::to_string));
        }
        assert!(
            winners.len() > 1,
            "different seeds should pick different keys"
        );
    }

    #[test]
    fn s07_rendezvous_stable_under_key_addition() {
        // Mirrors Go TestRendezvousSelect_StableWithKeyAddition: at least half
        // of the selections stay the same after adding a key.
        let original = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let extended = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        let seeds = ["s1", "s2", "s3", "s4", "s5", "s6"];
        let stable = seeds
            .iter()
            .filter(|s| rendezvous_select(&original, s) == rendezvous_select(&extended, s))
            .count();
        assert!(stable >= seeds.len() / 2);
    }

    #[test]
    fn s07_rendezvous_only_affected_key_remaps() {
        // Mirrors Go TestRendezvousSelect_OnlyAffectedKeysRemap: removing a
        // non-winning key never changes the pick.
        let original = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let reduced = vec!["a".to_string(), "c".to_string()]; // b removed
        for seed in ["x1", "x2", "x3", "x4", "x5"] {
            let original_pick = rendezvous_select(&original, seed).unwrap_or("NONE");
            let reduced_pick = rendezvous_select(&reduced, seed).unwrap_or("NONE");
            if original_pick != "b" {
                assert_eq!(original_pick, reduced_pick);
            } else {
                assert_ne!(reduced_pick, "b");
            }
        }
    }

    #[test]
    fn s07_exhausted_keys_marked_when_no_enabled_and_not_oauth() {
        // Mirrors Go DisableAPIKey's "len(enabledKeys) == 0" channel-disable
        // branch. Here we surface keys_exhausted; the legacy key is returned
        // for diagnostic fallback when present.
        let s = CredentialSnapshot {
            enabled_api_keys: &[],
            legacy_api_key: "legacy",
            is_oauth: false,
            has_azure: false,
            has_gcp: false,
            override_key: "",
        };
        let decision = decide_credential(&s, &trace_none());
        assert_eq!(decision.kind, CredentialKind::ApiKey);
        assert_eq!(decision.api_key, Some("legacy"));
        assert!(decision.keys_exhausted);
    }

    #[test]
    fn s07_exhausted_keys_returns_none_when_no_legacy_key() {
        let s = CredentialSnapshot {
            enabled_api_keys: &[],
            legacy_api_key: "",
            is_oauth: false,
            has_azure: false,
            has_gcp: false,
            override_key: "",
        };
        let decision = decide_credential(&s, &trace_none());
        assert_eq!(decision.api_key, None);
        assert!(decision.keys_exhausted);
    }

    #[test]
    fn s07_fnv64a_matches_go_hash_api_key() {
        // Determinism + known FNV-1a value for the empty string (offset basis)
        // and a non-trivial input. The empty-string value is the canonical
        // FNV-1a 64-bit offset basis, which Go's fnv.New64a also yields.
        assert_eq!(hash_api_key_fnv64a(""), 0xcbf29ce484222325);
        // Different inputs produce different hashes.
        assert_ne!(
            hash_api_key_fnv64a("input-1"),
            hash_api_key_fnv64a("input-2")
        );
    }

    #[test]
    fn s07_from_credentials_reflects_oauth_and_kinds() -> Result<(), Box<dyn std::error::Error>> {
        use conduit_core::objects::channel_settings::{
            AzureCredential, ChannelCredentials, GCPCredential,
        };
        let creds = ChannelCredentials {
            api_key: "legacy".to_string(),
            api_keys: vec!["k1".to_string(), "k2".to_string()],
            oauth: None,
            azure: None,
            gcp: None,
        };
        let enabled = vec!["k1".to_string(), "k2".to_string()];
        let snap_local = CredentialSnapshot::from_credentials(&creds, &enabled, "");
        assert!(!snap_local.is_oauth);
        assert_eq!(snap_local.legacy_api_key, "legacy");

        let oauth_creds = ChannelCredentials {
            api_key: String::new(),
            api_keys: Vec::new(),
            oauth: Some(serde_json::json!({"access_token": "tok"})),
            azure: None,
            gcp: None,
        };
        let snap_oauth = CredentialSnapshot::from_credentials(&oauth_creds, &[], "");
        assert!(snap_oauth.is_oauth);

        let azure_creds = ChannelCredentials {
            api_key: String::new(),
            api_keys: Vec::new(),
            oauth: None,
            azure: Some(AzureCredential::default()),
            gcp: None,
        };
        assert!(CredentialSnapshot::from_credentials(&azure_creds, &[], "").has_azure);

        let gcp_creds = ChannelCredentials {
            api_key: String::new(),
            api_keys: Vec::new(),
            oauth: None,
            azure: None,
            gcp: Some(GCPCredential::default()),
        };
        assert!(CredentialSnapshot::from_credentials(&gcp_creds, &[], "").has_gcp);
        Ok(())
    }

    // ---------- S11: auto-disable decision ----------

    #[test]
    fn s11_disabled_policy_never_disables() {
        // Mirrors Go: policy.AutoDisableChannel.Enabled == false → no action.
        let policy = AutoDisablePolicy::from_statuses(false, vec![(401, 3)]);
        let perf = PerformanceError {
            channel_id: 1,
            api_key: None,
            response_status_code: 401,
            prior_count: 5,
            current_enabled_key_count: 0,
        };
        match decide_auto_disable(&policy, &perf) {
            AutoDisableDecision::Keep { new_count } => assert_eq!(new_count, 5),
            other => panic!("expected Keep, got {other:?}"),
        }
    }

    #[test]
    fn s11_untracked_status_keeps_but_increments() {
        // Mirrors Go: status code has no matching rule → no disable, but the
        // counter is incremented (so a later rule change can trip).
        let policy = AutoDisablePolicy::from_statuses(true, vec![(401, 3)]);
        let perf = PerformanceError {
            channel_id: 1,
            api_key: None,
            response_status_code: 500,
            prior_count: 2,
            current_enabled_key_count: 0,
        };
        match decide_auto_disable(&policy, &perf) {
            AutoDisableDecision::Keep { new_count } => assert_eq!(new_count, 3),
            other => panic!("expected Keep, got {other:?}"),
        }
    }

    #[test]
    fn s11_below_threshold_keeps_and_increments() {
        // Mirrors Go checkAndHandleChannelError "first error - should not
        // disable" / "second error - should not disable" (threshold 2).
        let policy = AutoDisablePolicy::from_statuses(true, vec![(401, 2)]);

        let first = PerformanceError {
            channel_id: 1,
            api_key: None,
            response_status_code: 401,
            prior_count: 0,
            current_enabled_key_count: 0,
        };
        match decide_auto_disable(&policy, &first) {
            AutoDisableDecision::Keep { new_count } => assert_eq!(new_count, 1),
            other => panic!("expected Keep, got {other:?}"),
        }
    }

    #[test]
    fn s11_channel_threshold_crossed_disables_channel() {
        // Mirrors Go TestChannelService_checkAndHandleChannelError "second
        // error - should disable channel" (threshold 2, prior_count 1).
        let policy = AutoDisablePolicy::from_statuses(true, vec![(401, 2)]);
        let perf = PerformanceError {
            channel_id: 1,
            api_key: None,
            response_status_code: 401,
            prior_count: 1,
            current_enabled_key_count: 0,
        };
        match decide_auto_disable(&policy, &perf) {
            AutoDisableDecision::DisableChannel {
                status_code,
                threshold,
                actual_count,
            } => {
                assert_eq!(status_code, 401);
                assert_eq!(threshold, 2);
                assert_eq!(actual_count, 2);
            }
            other => panic!("expected DisableChannel, got {other:?}"),
        }
    }

    #[test]
    fn s11_apikey_threshold_crossed_disables_key() {
        // Mirrors Go TestChannelService_checkAndHandleAPIKeyError "third error
        // - should disable API key" (threshold 3, prior_count 2, multiple
        // keys remain so channel survives).
        let policy = AutoDisablePolicy::from_statuses(true, vec![(401, 3)]);
        let perf = PerformanceError {
            channel_id: 1,
            api_key: Some("key1"),
            response_status_code: 401,
            prior_count: 2,
            current_enabled_key_count: 3,
        };
        match decide_auto_disable(&policy, &perf) {
            AutoDisableDecision::DisableAPIKey {
                api_key,
                status_code,
                threshold,
                actual_count,
                channel_exhausted,
            } => {
                assert_eq!(api_key, "key1");
                assert_eq!(status_code, 401);
                assert_eq!(threshold, 3);
                assert_eq!(actual_count, 3);
                assert!(!channel_exhausted);
            }
            other => panic!("expected DisableAPIKey, got {other:?}"),
        }
    }

    #[test]
    fn s11_apikey_disable_marks_channel_exhausted_when_last_key() {
        // Mirrors Go TestChannelService_DisableAllAPIKeysDisablesChannel:
        // disabling the last enabled key must flag channel exhaustion.
        let policy = AutoDisablePolicy::from_statuses(true, vec![(401, 1)]);
        let perf = PerformanceError {
            channel_id: 1,
            api_key: Some("last-key"),
            response_status_code: 401,
            prior_count: 0,
            current_enabled_key_count: 1,
        };
        match decide_auto_disable(&policy, &perf) {
            AutoDisableDecision::DisableAPIKey {
                channel_exhausted, ..
            } => assert!(channel_exhausted),
            other => panic!("expected DisableAPIKey, got {other:?}"),
        }
    }

    #[test]
    fn s11_multiple_status_codes_first_match_wins() {
        // Mirrors Go TestChannelService_MultipleStatusCodes: 401 needs 2,
        // 403 needs 1.
        let policy = AutoDisablePolicy::from_statuses(true, vec![(401, 2), (403, 1)]);

        let perf_401 = PerformanceError {
            channel_id: 1,
            api_key: Some("k"),
            response_status_code: 401,
            prior_count: 1,
            current_enabled_key_count: 2,
        };
        match decide_auto_disable(&policy, &perf_401) {
            AutoDisableDecision::DisableAPIKey {
                status_code,
                threshold,
                ..
            } => {
                assert_eq!(status_code, 401);
                assert_eq!(threshold, 2);
            }
            other => panic!("expected DisableAPIKey for 401, got {other:?}"),
        }

        let perf_403 = PerformanceError {
            channel_id: 1,
            api_key: Some("k"),
            response_status_code: 403,
            prior_count: 0,
            current_enabled_key_count: 2,
        };
        match decide_auto_disable(&policy, &perf_403) {
            AutoDisableDecision::DisableAPIKey {
                status_code,
                threshold,
                ..
            } => {
                assert_eq!(status_code, 403);
                assert_eq!(threshold, 1);
            }
            other => panic!("expected DisableAPIKey for 403, got {other:?}"),
        }
    }

    #[test]
    fn s11_different_apikey_does_not_cross_threshold() {
        // Mirrors Go "different API key - should not disable": per-key
        // counters are independent, so key2's error must not trip key1's
        // counter. The helper is called per-cell, so we pass key2's own
        // prior_count (0) — the result must be Keep.
        let policy = AutoDisablePolicy::from_statuses(true, vec![(401, 3)]);
        let perf = PerformanceError {
            channel_id: 1,
            api_key: Some("key2"),
            response_status_code: 401,
            prior_count: 0, // key1 had 2, but key2 starts fresh
            current_enabled_key_count: 3,
        };
        match decide_auto_disable(&policy, &perf) {
            AutoDisableDecision::Keep { new_count } => assert_eq!(new_count, 1),
            other => panic!("expected Keep, got {other:?}"),
        }
    }

    #[test]
    fn s11_first_match_rule_wins_on_duplicate_status() {
        // Go iterates the statuses slice and triggers on the first matching
        // rule; duplicate statuses use the first threshold. We mirror that.
        let policy = AutoDisablePolicy {
            enabled: true,
            statuses: vec![
                AutoDisableStatusRule {
                    status: 401,
                    times: 5,
                },
                AutoDisableStatusRule {
                    status: 401,
                    times: 1,
                },
            ],
        };
        let perf = PerformanceError {
            channel_id: 1,
            api_key: None,
            response_status_code: 401,
            prior_count: 0,
            current_enabled_key_count: 0,
        };
        // First rule (times=5) wins → prior 0 + 1 < 5 → Keep.
        assert!(matches!(
            decide_auto_disable(&policy, &perf),
            AutoDisableDecision::Keep { new_count: 1 }
        ));
    }

    #[test]
    fn s11_derive_error_message_matches_http_status_text() {
        // Mirrors Go deriveErrorMessage for the codes the auto-disable
        // policy typically targets.
        assert_eq!(derive_error_message(401), "Unauthorized");
        assert_eq!(derive_error_message(403), "Forbidden");
        assert_eq!(derive_error_message(429), "Too Many Requests");
        assert_eq!(derive_error_message(500), "Internal Server Error");
        assert_eq!(derive_error_message(503), "Service Unavailable");
        assert_eq!(derive_error_message(504), "Gateway Timeout");
        // Unknown code falls back to "Error <code>".
        assert_eq!(derive_error_message(799), "Error 799");
    }

    // ---------- S08: retryable-status / error-pattern normalize + validate ----------
    //
    // Mirrors Go `TestNormalizeRetryableStatusCodes` /
    // `TestNormalizeRetryableErrorPatterns`
    // (`channel_retryable_status_codes_test.go`) and `TestValidateRateLimit`
    // (`channel_rate_limit_test.go`).

    #[test]
    fn s08_normalize_retryable_status_codes_sorts_and_dedups() -> Result<(), ChannelNormalizeError>
    {
        // Mirrors Go "sorts and deduplicates error status codes".
        let mut settings = ChannelSettings {
            retryable_status_codes: vec![403, 400, 403, 500],
            ..Default::default()
        };

        normalize_retryable_status_codes(&mut settings)?;

        assert_eq!(settings.retryable_status_codes, vec![400, 403, 500]);
        Ok(())
    }

    #[test]
    fn s08_normalize_retryable_status_codes_allows_empty() -> Result<(), ChannelNormalizeError> {
        // Mirrors Go "allows empty settings" (nil + zero-value both OK).
        let mut empty = ChannelSettings::default();
        normalize_retryable_status_codes(&mut empty)?;
        assert!(empty.retryable_status_codes.is_empty());
        Ok(())
    }

    #[test]
    fn s08_normalize_retryable_status_codes_rejects_non_error_codes() {
        // Mirrors Go "rejects non error status codes".
        let mut settings = ChannelSettings {
            retryable_status_codes: vec![200],
            ..Default::default()
        };

        let err = match normalize_retryable_status_codes(&mut settings) {
            Ok(()) => panic!("200 should be rejected"),
            Err(e) => e,
        };
        assert!(
            err.to_string()
                .contains("invalid retryable status code 200"),
            "got: {err}"
        );
    }

    #[test]
    fn s08_normalize_retryable_status_codes_rejects_above_599() {
        // Boundary: 600 is out of range (400-599).
        let mut settings = ChannelSettings {
            retryable_status_codes: vec![600],
            ..Default::default()
        };

        let err = match normalize_retryable_status_codes(&mut settings) {
            Ok(()) => panic!("600 should be rejected"),
            Err(e) => e,
        };
        assert!(
            err.to_string()
                .contains("invalid retryable status code 600"),
            "got: {err}"
        );
    }

    #[test]
    fn s08_normalize_retryable_status_codes_accepts_boundaries() -> Result<(), ChannelNormalizeError>
    {
        // 400 and 599 are the inclusive bounds.
        let mut settings = ChannelSettings {
            retryable_status_codes: vec![599, 400],
            ..Default::default()
        };

        normalize_retryable_status_codes(&mut settings)?;
        assert_eq!(settings.retryable_status_codes, vec![400, 599]);
        Ok(())
    }

    #[test]
    fn s08_normalize_retryable_error_patterns_trims_and_dedups() -> Result<(), ChannelNormalizeError>
    {
        // Mirrors Go "trims and deduplicates retryable error patterns".
        let mut settings = ChannelSettings {
            retryable_error_patterns: vec![
                RetryableErrorPattern {
                    pattern: " Console API returned 403 ".to_string(),
                    regex: false,
                },
                RetryableErrorPattern {
                    pattern: "Console API returned 403".to_string(),
                    regex: false,
                },
                RetryableErrorPattern {
                    pattern: r"Console API returned \d+".to_string(),
                    regex: true,
                },
            ],
            ..Default::default()
        };

        normalize_retryable_error_patterns(&mut settings)?;

        assert_eq!(
            settings.retryable_error_patterns,
            vec![
                RetryableErrorPattern {
                    pattern: "Console API returned 403".to_string(),
                    regex: false,
                },
                RetryableErrorPattern {
                    pattern: r"Console API returned \d+".to_string(),
                    regex: true,
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn s08_normalize_retryable_error_patterns_allows_empty() -> Result<(), ChannelNormalizeError> {
        // Mirrors Go "allows empty settings".
        let mut empty = ChannelSettings::default();
        normalize_retryable_error_patterns(&mut empty)?;
        assert!(empty.retryable_error_patterns.is_empty());
        Ok(())
    }

    #[test]
    fn s08_normalize_retryable_error_patterns_rejects_invalid_regex() {
        // Mirrors Go "rejects invalid regex patterns".
        let mut settings = ChannelSettings {
            retryable_error_patterns: vec![RetryableErrorPattern {
                pattern: "Console API returned [".to_string(),
                regex: true,
            }],
            ..Default::default()
        };

        let err = match normalize_retryable_error_patterns(&mut settings) {
            Ok(()) => panic!("unclosed '[' should be rejected"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("invalid retryable error regex"),
            "got: {err}"
        );
    }

    #[test]
    fn s08_normalize_retryable_error_patterns_drops_empty_after_trim()
    -> Result<(), ChannelNormalizeError> {
        // Whitespace-only patterns are dropped (not errors), matching Go's
        // `if pattern.Pattern == "" { continue }`.
        let mut settings = ChannelSettings {
            retryable_error_patterns: vec![
                RetryableErrorPattern {
                    pattern: "   ".to_string(),
                    regex: false,
                },
                RetryableErrorPattern {
                    pattern: "real".to_string(),
                    regex: false,
                },
            ],
            ..Default::default()
        };

        normalize_retryable_error_patterns(&mut settings)?;
        assert_eq!(
            settings.retryable_error_patterns,
            vec![RetryableErrorPattern {
                pattern: "real".to_string(),
                regex: false,
            }]
        );
        Ok(())
    }

    #[test]
    fn s08_normalize_retryable_error_patterns_dedup_key_includes_regex_flag()
    -> Result<(), ChannelNormalizeError> {
        // Same text with different regex flags survives as distinct entries,
        // matching Go's `fmt.Sprintf("%t\x00%s", regex, pattern)` key.
        let mut settings = ChannelSettings {
            retryable_error_patterns: vec![
                RetryableErrorPattern {
                    pattern: "abc".to_string(),
                    regex: false,
                },
                RetryableErrorPattern {
                    pattern: "abc".to_string(),
                    regex: true,
                },
            ],
            ..Default::default()
        };

        normalize_retryable_error_patterns(&mut settings)?;
        assert_eq!(settings.retryable_error_patterns.len(), 2);
        Ok(())
    }

    #[test]
    fn s08_validate_rate_limit_table() -> Result<(), Box<dyn std::error::Error>> {
        // Mirrors Go `TestValidateRateLimit` table-driven cases byte-for-byte.
        let cases: &[(&str, Option<ChannelRateLimit>, Option<&str>)] = &[
            ("nil is allowed", None, None),
            (
                "all zero/nil is allowed",
                Some(ChannelRateLimit::default()),
                None,
            ),
            (
                "fully configured hard mode is allowed",
                Some(ChannelRateLimit {
                    rpm: Some(100),
                    tpm: Some(10000),
                    max_concurrent: Some(5),
                    queue_size: Some(20),
                    queue_timeout_ms: Some(30000),
                }),
                None,
            ),
            (
                "soft mode is allowed",
                Some(ChannelRateLimit {
                    max_concurrent: Some(5),
                    ..Default::default()
                }),
                None,
            ),
            (
                "negative rpm rejected",
                Some(ChannelRateLimit {
                    rpm: Some(-1),
                    ..Default::default()
                }),
                Some("rpm must be >= 0"),
            ),
            (
                "negative tpm rejected",
                Some(ChannelRateLimit {
                    tpm: Some(-1),
                    ..Default::default()
                }),
                Some("tpm must be >= 0"),
            ),
            (
                "negative maxConcurrent rejected",
                Some(ChannelRateLimit {
                    max_concurrent: Some(-1),
                    ..Default::default()
                }),
                Some("maxConcurrent must be >= 0"),
            ),
            (
                "negative queueSize rejected",
                Some(ChannelRateLimit {
                    queue_size: Some(-1),
                    ..Default::default()
                }),
                Some("queueSize must be >= 0"),
            ),
            (
                "negative queueTimeoutMs rejected",
                Some(ChannelRateLimit {
                    queue_timeout_ms: Some(-1),
                    ..Default::default()
                }),
                Some("queueTimeoutMs must be >= 0"),
            ),
            (
                "queueSize without maxConcurrent rejected",
                Some(ChannelRateLimit {
                    queue_size: Some(10),
                    ..Default::default()
                }),
                Some("queueSize requires maxConcurrent > 0"),
            ),
            (
                "queueSize with zero maxConcurrent rejected",
                Some(ChannelRateLimit {
                    queue_size: Some(10),
                    max_concurrent: Some(0),
                    ..Default::default()
                }),
                Some("queueSize requires maxConcurrent > 0"),
            ),
            (
                "queueTimeoutMs without queueSize is allowed (but inert)",
                Some(ChannelRateLimit {
                    max_concurrent: Some(5),
                    queue_timeout_ms: Some(1000),
                    ..Default::default()
                }),
                None,
            ),
        ];

        for (name, input, want_err) in cases {
            let result = validate_rate_limit(input.as_ref());
            match want_err {
                None => {
                    assert!(
                        result.is_ok(),
                        "{name}: expected ok, got {:?}",
                        result.err()
                    );
                }
                Some(want) => {
                    let err = match result {
                        Ok(()) => panic!("{name}: expected error"),
                        Err(e) => e,
                    };
                    assert!(
                        err.to_string().contains(want),
                        "{name}: error {err} should contain {want:?}"
                    );
                }
            }
        }
        Ok(())
    }

    // ---------- S04: build_channel_with_transformer (plan + validation) -----

    use conduit_llm::{HttpRequest, HttpResponse, LlmRequest, StreamEvent};
    use conduit_transformers::registry::{
        AuthStrategy as TAuthStrategy, CredentialRequirement as TCredentialRequirement,
        DirectProvider, KeyProviderKind as TKeyProviderKind, ProviderFamily as TProviderFamily,
    };
    use conduit_transformers::traits::OutboundTransformer;

    fn build_input<'a>(
        name: &'a str,
        channel_type: &'a str,
        keys: &'a [String],
    ) -> ChannelBuildInput<'a> {
        ChannelBuildInput {
            name,
            channel_type,
            base_url: "https://example.com",
            enabled_api_keys: keys,
            is_oauth: false,
            has_azure: false,
            has_gcp: false,
            legacy_api_key: "",
            api_key_override: "",
            user_endpoints: &[],
        }
    }

    /// Helper: assert a build plan is Ok and return it (avoids .unwrap()).
    fn plan_or_panic(input: &ChannelBuildInput<'_>) -> ChannelBuildPlan {
        match build_channel_with_transformer(input) {
            Ok(plan) => plan,
            Err(e) => panic!("expected Ok plan, got {e:?}"),
        }
    }

    /// Helper: assert a build result is Err and return the error.
    fn err_or_panic(input: &ChannelBuildInput<'_>) -> ChannelBuildError {
        match build_channel_with_transformer(input) {
            Ok(_) => panic!("expected Err"),
            Err(e) => e,
        }
    }

    #[test]
    fn s04_openai_channel_with_one_key_plans_openai_compatible_family() {
        // Mirrors Go channel_llm.go:956-971 (openai-compatible branch) and
        // :155-170 (single key -> StaticKeyProvider).
        let keys = vec!["sk-test".to_string()];
        let input = build_input("my-openai", "openai", &keys);
        let plan = plan_or_panic(&input);

        assert_eq!(plan.channel_type, "openai");
        assert_eq!(plan.descriptor.family, TProviderFamily::OpenAiCompatible);
        assert_eq!(plan.descriptor.auth_strategy, TAuthStrategy::Bearer);
        assert_eq!(plan.key_provider_kind, TKeyProviderKind::Static);
        assert_eq!(plan.credential_requirement, TCredentialRequirement::ApiKey);
        assert_eq!(plan.base_url, "https://example.com");
    }

    #[test]
    fn s04_anthropic_channel_resolves_anthropic_family_with_api_key_header() {
        // Mirrors Go channel_llm.go:628-640 (anthropic direct branch).
        let keys = vec!["sk-ant".to_string()];
        let input = build_input("ant", "anthropic", &keys);
        let plan = plan_or_panic(&input);

        assert_eq!(plan.descriptor.family, TProviderFamily::Anthropic);
        assert_eq!(plan.descriptor.auth_strategy, TAuthStrategy::ApiKeyHeader);
        assert_eq!(plan.key_provider_kind, TKeyProviderKind::Static);
    }

    #[test]
    fn s04_multi_key_channel_selects_trace_sticky_provider_go_161_163() {
        // Mirrors Go getAPIKeyProvider: len(enabled) > 1 -> TraceSticky.
        let keys = vec!["k1".to_string(), "k2".to_string()];
        let input = build_input("multi", "openai", &keys);
        let plan = plan_or_panic(&input);
        assert_eq!(plan.key_provider_kind, TKeyProviderKind::TraceSticky);
    }

    #[test]
    fn s04_api_key_override_forces_static_provider_go_156_158() {
        let keys = vec!["k1".to_string(), "k2".to_string(), "k3".to_string()];
        let mut input = build_input("override", "openai", &keys);
        input.api_key_override = "force-key";
        let plan = plan_or_panic(&input);
        assert_eq!(plan.key_provider_kind, TKeyProviderKind::Static);
    }

    #[test]
    fn s04_codex_with_oauth_plans_codex_family_go_451_454_887_896() {
        // Mirrors Go channel_llm.go:451-454 (codex accepts OAuth OR key) and
        // :887-896 (codex outbound).
        let input = ChannelBuildInput {
            name: "codex-ch",
            channel_type: "codex",
            base_url: "https://example.com",
            enabled_api_keys: &[],
            is_oauth: true,
            has_azure: false,
            has_gcp: false,
            legacy_api_key: "",
            api_key_override: "",
            user_endpoints: &[],
        };
        let plan = plan_or_panic(&input);
        assert_eq!(plan.descriptor.family, TProviderFamily::Codex);
        assert_eq!(plan.descriptor.auth_strategy, TAuthStrategy::OAuth);
        assert_eq!(
            plan.credential_requirement,
            TCredentialRequirement::OAuthOrApiKey
        );
    }

    #[test]
    fn s04_codex_without_any_credentials_errors_go_451_454() {
        let input = build_input("codex-empty", "codex", &[]);
        let err = err_or_panic(&input);
        match err {
            ChannelBuildError::MissingOAuthOrApiKey { name } => assert_eq!(name, "codex-empty"),
            other => panic!("expected MissingOAuthOrApiKey, got {other:?}"),
        }
    }

    #[test]
    fn s04_github_copilot_requires_oauth_go_455_459() {
        // Without OAuth: error.
        let input = build_input("copilot", "github_copilot", &[]);
        let err = err_or_panic(&input);
        assert!(matches!(err, ChannelBuildError::MissingOAuth { .. }));

        // With OAuth: plan resolves to GithubCopilot family.
        let input_ok = ChannelBuildInput {
            name: "copilot",
            channel_type: "github_copilot",
            base_url: "https://example.com",
            enabled_api_keys: &[],
            is_oauth: true,
            has_azure: false,
            has_gcp: false,
            legacy_api_key: "",
            api_key_override: "",
            user_endpoints: &[],
        };
        let plan = plan_or_panic(&input_ok);
        assert_eq!(plan.descriptor.family, TProviderFamily::GithubCopilot);
        assert_eq!(
            plan.credential_requirement,
            TCredentialRequirement::OAuthOnly
        );
    }

    #[test]
    fn s04_antigravity_requires_legacy_api_key_go_460_464() {
        // Empty legacy key -> error.
        let input = build_input("ag", "antigravity", &[]);
        let err = err_or_panic(&input);
        assert!(matches!(err, ChannelBuildError::MissingApiKey { .. }));

        // Non-empty legacy key -> plan.
        let mut input_ok = build_input("ag", "antigravity", &[]);
        input_ok.legacy_api_key = "refresh|project";
        let plan = plan_or_panic(&input_ok);
        assert_eq!(plan.descriptor.family, TProviderFamily::Antigravity);
    }

    #[test]
    fn s04_anthropic_gcp_requires_gcp_credentials_go_786_791() {
        let input = build_input("vertex", "anthropic_gcp", &[]);
        let err = err_or_panic(&input);
        assert!(matches!(err, ChannelBuildError::MissingGcpCredentials));

        let mut input_ok = build_input("vertex", "anthropic_gcp", &[]);
        input_ok.has_gcp = true;
        let plan = plan_or_panic(&input_ok);
        assert_eq!(
            plan.descriptor.auth_strategy,
            TAuthStrategy::GcpServiceAccount
        );
    }

    #[test]
    fn s04_default_branch_rejects_zero_keys_go_469_473() {
        let input = build_input("no-keys", "deepseek", &[]);
        let err = err_or_panic(&input);
        assert!(matches!(err, ChannelBuildError::MissingApiKey { .. }));
    }

    #[test]
    fn s04_fake_transformers_require_no_credentials_go_465_468_806_812() {
        let input = ChannelBuildInput {
            name: "fake",
            channel_type: "anthropic_fake",
            base_url: "",
            enabled_api_keys: &[],
            is_oauth: false,
            has_azure: false,
            has_gcp: false,
            legacy_api_key: "",
            api_key_override: "",
            user_endpoints: &[],
        };
        let plan = plan_or_panic(&input);
        assert_eq!(plan.descriptor.family, TProviderFamily::AnthropicFake);
        assert_eq!(plan.descriptor.auth_strategy, TAuthStrategy::None);

        let input_oai = ChannelBuildInput {
            channel_type: "openai_fake",
            ..input
        };
        let plan_oai = plan_or_panic(&input_oai);
        assert_eq!(plan_oai.descriptor.family, TProviderFamily::OpenAiFake);
    }

    #[test]
    fn s04_ollama_allows_zero_keys_go_1039_1056() {
        let input = build_input("local", "ollama", &[]);
        let plan = plan_or_panic(&input);
        assert_eq!(plan.descriptor.family, TProviderFamily::Direct);
        assert_eq!(
            plan.descriptor.direct_provider,
            Some(DirectProvider::Ollama)
        );
        assert_eq!(plan.key_provider_kind, TKeyProviderKind::None);
    }

    #[test]
    fn s04_unknown_channel_type_errors_go_1057_1058() {
        let keys = vec!["k".to_string()];
        let input = build_input("mystery", "not-a-real-channel", &keys);
        let err = err_or_panic(&input);
        match err {
            ChannelBuildError::UnknownChannelType { channel_type } => {
                assert_eq!(channel_type, "not-a-real-channel");
            }
            other => panic!("expected UnknownChannelType, got {other:?}"),
        }
    }

    // ---------- S09: build_channel_with_outbounds (aliasing rule) -----------

    /// Minimal OutboundTransformer impl for tests — identity on every method.
    struct StubOutbound {
        tag: &'static str,
    }

    impl OutboundTransformer for StubOutbound {
        fn name(&self) -> &'static str {
            self.tag
        }
        fn outbound_request(
            &self,
            _request: &LlmRequest,
        ) -> conduit_transformers::traits::TransformerResult<HttpRequest> {
            Ok(HttpRequest::default())
        }
        fn outbound_response(
            &self,
            response: HttpResponse,
        ) -> conduit_transformers::traits::TransformerResult<HttpResponse> {
            Ok(response)
        }
        fn outbound_stream_event(
            &self,
            event: StreamEvent,
        ) -> conduit_transformers::traits::TransformerResult<StreamEvent> {
            Ok(event)
        }
        fn outbound_error(
            &self,
            _response: HttpResponse,
        ) -> conduit_transformers::traits::TransformerResult<conduit_core::ConduitError> {
            Ok(conduit_core::ConduitError::internal("stub"))
        }
    }

    fn stub_outbound(tag: &'static str) -> Arc<dyn OutboundTransformer> {
        Arc::new(StubOutbound { tag })
    }

    /// Helper: build outbounds or panic (avoids .unwrap()).
    fn outbounds_or_panic<R, E>(
        channel_type: &str,
        defaults: &[ChannelEndpoint],
        user: &[ChannelEndpoint],
        primary: Arc<dyn OutboundTransformer>,
        resolver: R,
    ) -> Option<BTreeMap<String, Arc<dyn OutboundTransformer>>>
    where
        R: FnMut(&ChannelEndpoint) -> Result<Arc<dyn OutboundTransformer>, E>,
        E: std::fmt::Display,
    {
        match build_channel_with_outbounds(channel_type, defaults, user, primary, resolver) {
            Ok(map) => map,
            Err(e) => panic!("expected Ok outbound map, got {e:?}"),
        }
    }

    /// Helper: get a key from the outbound map or panic (avoids .unwrap()).
    fn outbound_get<'a>(
        map: &'a BTreeMap<String, Arc<dyn OutboundTransformer>>,
        key: &str,
    ) -> &'a Arc<dyn OutboundTransformer> {
        match map.get(key) {
            Some(v) => v,
            None => panic!("missing outbound for api_format {key:?}"),
        }
    }

    #[test]
    fn s09_no_endpoints_returns_none_go_203_205() {
        // Mirrors Go: "if len(defaultEndpoints) == 0 && len(userEndpoints) == 0
        // { return ch, nil }" — outbound map stays nil.
        let primary = stub_outbound("primary");
        let result = outbounds_or_panic("openai", &[], &[], primary, |_| {
            Ok::<_, std::convert::Infallible>(stub_outbound("unused"))
        });
        assert!(result.is_none());
    }

    #[test]
    fn s09_primary_default_endpoints_alias_primary_outbound_go_209_215() {
        // Mirrors Go channel_llm.go:209-215: every default endpoint maps to
        // the SAME primary outbound (ch.Outbound).
        let defaults = vec![
            ep("openai/chat_completions", "/v1/chat/completions"),
            ep("openai/embeddings", "/v1/embeddings"),
            ep("openai/image_generations", "/v1/images/generations"),
        ];
        let primary = stub_outbound("primary");
        let result = outbounds_or_panic("openai", &defaults, &[], Arc::clone(&primary), |_| {
            Ok::<_, std::convert::Infallible>(stub_outbound("should-not-be-called"))
        });
        let map = match result {
            Some(m) => m,
            None => panic!("expected outbound map"),
        };

        // All three default api_formats present and ALL point at the primary.
        assert_eq!(map.len(), 3);
        for fmt in [
            "openai/chat_completions",
            "openai/embeddings",
            "openai/image_generations",
        ] {
            let out = outbound_get(&map, fmt);
            assert_eq!(
                out.name(),
                "primary",
                "api_format {fmt} should alias primary"
            );
        }
    }

    #[test]
    fn s09_secondary_default_endpoint_aliases_primary_outbound_go_209_215_s09() {
        // The S09 contract pin: a channel type with multiple default endpoints
        // (e.g. gemini has contents + embedding) must alias the primary
        // outbound for BOTH. This is the core S09 unblock.
        let defaults = vec![
            ep("gemini/contents", "/v1beta/contents"),
            ep("gemini/embeddings", "/v1beta/embeddings"),
        ];
        let primary = stub_outbound("gemini-primary");
        let result = outbounds_or_panic("gemini", &defaults, &[], Arc::clone(&primary), |_| {
            Ok::<_, std::convert::Infallible>(stub_outbound("unused"))
        });
        let map = match result {
            Some(m) => m,
            None => panic!("expected map"),
        };

        assert_eq!(map.len(), 2);
        assert_eq!(
            outbound_get(&map, "gemini/contents").name(),
            "gemini-primary"
        );
        assert_eq!(
            outbound_get(&map, "gemini/embeddings").name(),
            "gemini-primary"
        );
        // Both entries are the SAME Arc (not just equal names).
        let a = outbound_get(&map, "gemini/contents");
        let b = outbound_get(&map, "gemini/embeddings");
        assert!(
            Arc::ptr_eq(a, b),
            "secondary default must share the primary Arc"
        );
    }

    #[test]
    fn s09_user_endpoint_overriding_default_still_aliases_primary_go_217_226() {
        // Mirrors Go loop order: defaults are iterated first (alias primary),
        // user endpoints second. A user endpoint that overrides a default's
        // api_format is NOT re-routed through buildNonDefaultEndpointOutbound —
        // it keeps the primary outbound. The resolver closure must not be
        // called for it.
        let defaults = vec![ep("openai/chat_completions", "/v1/chat/completions")];
        let user = vec![ChannelEndpoint {
            api_format: "openai/chat_completions".to_string(),
            path: "/custom-path".to_string(),
            base_url: "https://proxy".to_string(),
            transport: String::new(),
        }];
        let primary = stub_outbound("primary");
        let mut resolver_called = false;
        let result = outbounds_or_panic("openai", &defaults, &user, Arc::clone(&primary), |_| {
            resolver_called = true;
            Ok::<_, std::convert::Infallible>(stub_outbound("per-endpoint"))
        });
        let map = match result {
            Some(m) => m,
            None => panic!("expected map"),
        };

        assert!(
            !resolver_called,
            "override endpoint must not invoke resolver"
        );
        assert_eq!(
            outbound_get(&map, "openai/chat_completions").name(),
            "primary"
        );
    }

    #[test]
    fn s09_non_default_user_endpoint_gets_own_outbound_go_217_226() {
        // A user endpoint whose api_format has no matching default reaches the
        // resolver and gets its own outbound.
        let defaults = vec![ep("openai/chat_completions", "/v1/chat/completions")];
        let user = vec![ep("anthropic/messages", "/v1/messages")];
        let primary = stub_outbound("primary");
        let result = outbounds_or_panic("anthropic-proxy", &defaults, &user, primary, |ep_obj| {
            assert_eq!(ep_obj.api_format, "anthropic/messages");
            Ok::<_, std::convert::Infallible>(stub_outbound("anthropic-out"))
        });
        let map = match result {
            Some(m) => m,
            None => panic!("expected map"),
        };

        assert_eq!(map.len(), 2);
        assert_eq!(
            outbound_get(&map, "openai/chat_completions").name(),
            "primary"
        );
        assert_eq!(
            outbound_get(&map, "anthropic/messages").name(),
            "anthropic-out"
        );
    }

    #[test]
    fn s09_resolver_error_wraps_with_channel_and_format_go_221_225() {
        let user = vec![ep("custom/format", "/x")];
        let primary = stub_outbound("primary");
        let result =
            build_channel_with_outbounds::<_, String>("my-channel", &[], &user, primary, |_| {
                Err("simulated construction failure".to_string())
            });

        let err = match result {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        match err {
            ChannelBuildError::EndpointOutboundBuild {
                name,
                api_format,
                reason,
            } => {
                assert_eq!(name, "my-channel");
                assert_eq!(api_format, "custom/format");
                assert!(reason.contains("simulated construction failure"));
            }
            other => panic!("expected EndpointOutboundBuild, got {other:?}"),
        }
    }

    #[test]
    fn s09_endpoints_with_empty_api_format_are_skipped() {
        // Mirrors Go: `if ep.APIFormat == "" { continue }`.
        let defaults = vec![
            ChannelEndpoint::default(), // empty api_format
            ep("openai/chat_completions", "/v1"),
        ];
        let user = vec![ChannelEndpoint::default()]; // empty api_format
        let primary = stub_outbound("primary");
        let result = outbounds_or_panic("openai", &defaults, &user, primary, |_| {
            Ok::<_, std::convert::Infallible>(stub_outbound("x"))
        });
        let map = match result {
            Some(m) => m,
            None => panic!("expected map"),
        };
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("openai/chat_completions"));
    }

    #[test]
    fn s04_plan_channel_build_combines_plan_and_defaults() {
        // Mirrors the Go call sequence: buildChannelWithTransformer then
        // DefaultEndpointsForChannelType(c.Type).
        let mut defaults = DefaultEndpointRegistry::new();
        defaults.register(
            "openai",
            vec![
                ep("openai/chat_completions", "/v1/chat/completions"),
                ep("openai/embeddings", "/v1/embeddings"),
            ],
        );
        let keys = vec!["sk-test".to_string()];
        let input = build_input("my-openai", "openai", &keys);
        let (plan, default_endpoints) = match plan_channel_build(&defaults, &input) {
            Ok(v) => v,
            Err(e) => panic!("expected Ok, got {e:?}"),
        };

        assert_eq!(plan.descriptor.family, TProviderFamily::OpenAiCompatible);
        assert_eq!(default_endpoints.len(), 2);
        assert_eq!(default_endpoints[0].api_format, "openai/chat_completions");
    }

    #[test]
    fn s04_plan_channel_build_unknown_type_has_no_defaults() {
        let defaults = DefaultEndpointRegistry::new();
        let keys = vec!["k".to_string()];
        let input = build_input("mystery", "not-a-real-channel", &keys);
        let result = plan_channel_build(&defaults, &input);
        assert!(matches!(
            result,
            Err(ChannelBuildError::UnknownChannelType { .. })
        ));
    }

    #[test]
    fn s04_credential_kind_dispatch_for_all_requirement_tiers() {
        // Smoke-test the validation matrix mirroring Go :450-473.
        // ApiKey tier.
        let plan = plan_or_panic(&build_input("d", "deepseek", &["k".to_string()]));
        assert_eq!(plan.credential_requirement, TCredentialRequirement::ApiKey);

        // None tier (fake).
        let fake_in = ChannelBuildInput {
            name: "f",
            channel_type: "openai_fake",
            ..build_input("f", "openai_fake", &[])
        };
        let plan_fake = plan_or_panic(&fake_in);
        assert_eq!(
            plan_fake.credential_requirement,
            TCredentialRequirement::None
        );

        // OptionalApiKey tier (ollama).
        let plan_ollama = plan_or_panic(&build_input("o", "ollama", &[]));
        assert_eq!(
            plan_ollama.credential_requirement,
            TCredentialRequirement::OptionalApiKey
        );
    }
}
