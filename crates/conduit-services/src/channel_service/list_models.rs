//! `ListModels` aggregation (pure decision layer).
//!
//! Port of Go `internal/server/biz/channel.go::ChannelService.ListModels`
//! (lines 436-501), restricted to the pure aggregation logic that the host
//! runs over an already-loaded slice of channels. The DB query
//! (`Channel.Query().Where(channel.StatusIn(...))`) is host-owned; this seam
//! captures the in-memory aggregation so it stays parity-testable without a
//! live ent client.
//!
//! What is ported here (mirrors Go `ListModels` lines 453-498):
//! - [`status_priority`] — Go `statusPriority` map (enabled=3 > disabled=2 > archived=1).
//! - [`set_model_status`] — Go `setModelStatus` (insert-or-priority-replace).
//! - [`list_models_pure`] — the aggregation loop over the `else` branch of Go `ListModels` (i.e. `IncludeAllChannelModels == false`): supported_models + optional model mappings (admitted only when the mapping target is in `supported_models`) + optional `extra_model_prefix` aliases.
//! - [`filter_channels_by_status`] — the Go status-filter arm (`len(StatusIn) > 0` ? `StatusIn` : default `enabled`).
//!
//! The `IncludeAllChannelModels == true` arm of Go `ListModels` delegates to
//! `Channel.GetModelEntries`, whose Rust counterpart is the S06 logic in
//! [`crate::channel_service::model_sync`]. No Go `*_test.go` subtest of
//! `TestChannelService_ListModels` sets `IncludeAllChannelModels`, so that arm
//! is intentionally not exercised here — it is covered by the `model_sync`
//! tests (`s06_*`).
//!
//! Parity test source: `conduit/internal/server/biz/channel_test.go`
//! `TestChannelService_ListModels` (lines 17-209).

use std::collections::HashMap;

use conduit_core::objects::channel_settings::ChannelSettings;

/// Re-export of the canonical channel status strings so callers can spell
/// them without importing `conduit-db`. Mirrors Go `channel.Status` constants
/// (`StatusEnabled = "enabled"`, etc.).
pub mod channel_status {
    pub const ENABLED: &str = "enabled";
    pub const DISABLED: &str = "disabled";
    pub const ARCHIVED: &str = "archived";
}

/// Input filters for [`list_models_pure`]. Ported from Go `ListModelsInput`
/// (`channel.go:398-404`). `status_in` mirrors Go `StatusIn []channel.Status`
/// (stored as lowercase strings to match the wire form).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListModelsInput {
    /// Channels whose status is in this list are included. When empty,
    /// [`channel_status::ENABLED`] is used (mirrors Go lines 441-445).
    pub status_in: Vec<String>,
    /// Mirrors Go `IncludeAllChannelModels` — when true the host would use
    /// `GetModelEntries` instead of the supported-models/mapping/prefix arms.
    /// Not exercised by any Go `TestChannelService_ListModels` subtest; kept
    /// for shape parity.
    pub include_all_channel_models: bool,
    /// Mirrors Go `IncludeMapping` — admit `ModelMapping.from` aliases whose
    /// `to` target is in `supported_models` (Go lines 472-479).
    pub include_mapping: bool,
    /// Mirrors Go `IncludePrefix` — admit `"{extra_model_prefix}/{model}"`
    /// aliases for each supported model (Go lines 482-487).
    pub include_prefix: bool,
}

/// A model id paired with the channel status it was last attributed to.
/// Ported from Go `ModelIdentityWithStatus` (`channel.go:406-410`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelIdentityWithStatus {
    pub id: String,
    pub status: String,
}

/// Pure-data view of a channel as needed by [`list_models_pure`]. The host
/// fills this from its row/entity before calling the aggregator. Mirrors the
/// fields of Go's `*ent.Channel` read by `ListModels` (`Status`,
/// `SupportedModels`, `Settings`).
///
/// Derives `Debug, Clone, Default` only — `ChannelSettings` contains float
/// fields and therefore does not implement `Eq`, so neither can this view.
/// Tests compare the aggregated `ModelIdentityWithStatus` output rather than
/// the input view, so equality on the view itself is not required.
#[derive(Debug, Clone, Default)]
pub struct ListModelsChannel {
    pub status: String,
    pub supported_models: Vec<String>,
    pub settings: Option<ChannelSettings>,
}

/// Priority used by [`set_model_status`] when two channels contribute the same
/// model id with different statuses. Mirrors Go `statusPriority`
/// (`channel.go:420-424`): `enabled=3 > disabled=2 > archived=1`. Unknown
/// statuses get priority 0 so any known status overrides them.
pub fn status_priority(status: &str) -> i32 {
    match status {
        channel_status::ENABLED => 3,
        channel_status::DISABLED => 2,
        channel_status::ARCHIVED => 1,
        _ => 0,
    }
}

/// Insert `model_id` into `models` or, when it already exists, replace its
/// status only if `new_status` has strictly higher [`status_priority`].
/// Mirrors Go `setModelStatus` (`channel.go:428-432`).
pub fn set_model_status(models: &mut HashMap<String, String>, model_id: &str, new_status: &str) {
    match models.get(model_id) {
        Some(existing) if status_priority(new_status) <= status_priority(existing) => {}
        _ => {
            models.insert(model_id.to_string(), new_status.to_string());
        }
    }
}

/// Apply the Go `ListModels` status filter to a channel slice.
///
/// Mirrors Go lines 441-445: when `status_in` is non-empty, keep channels
/// whose status is in `status_in`; otherwise keep channels whose status is
/// [`channel_status::ENABLED`].
pub fn filter_channels_by_status<'a>(
    channels: &'a [ListModelsChannel],
    input: &ListModelsInput,
) -> Vec<&'a ListModelsChannel> {
    if input.status_in.is_empty() {
        channels
            .iter()
            .filter(|c| c.status == channel_status::ENABLED)
            .collect()
    } else {
        channels
            .iter()
            .filter(|c| input.status_in.iter().any(|s| s == &c.status))
            .collect()
    }
}

/// Pure aggregation mirroring Go `ListModels` (`channel.go:436-501`), `else`
/// branch only (see module docs on why `IncludeAllChannelModels` is not
/// exercised here).
///
/// The host passes the full channel slice and the input; this function filters
/// by status, then walks each channel contributing its `supported_models`,
/// optional mapping aliases (only those whose `to` is supported) and optional
/// `extra_model_prefix` aliases, using [`set_model_status`] to keep the
/// highest-priority status per model id.
///
/// The result is sorted by model id for deterministic comparison (Go returns
/// map-order which is nondeterministic; the Go test uses
/// `require.ElementsMatch` so order is irrelevant — sorting here keeps Rust
/// assertions stable).
pub fn list_models_pure(
    channels: &[ListModelsChannel],
    input: &ListModelsInput,
) -> Vec<ModelIdentityWithStatus> {
    let filtered = filter_channels_by_status(channels, input);
    let mut model_map: HashMap<String, String> = HashMap::new();

    for ch in filtered {
        // Add all supported models (Go lines 466-469).
        for model_id in &ch.supported_models {
            set_model_status(&mut model_map, model_id, &ch.status);
        }

        // Add model mappings if requested (Go lines 472-479). Only mappings
        // whose `to` target is in supported_models are admitted.
        if input.include_mapping
            && let Some(settings) = &ch.settings
        {
            for mapping in settings
                .model_mappings
                .iter()
                .filter(|m| ch.supported_models.iter().any(|s| s == &m.to))
            {
                set_model_status(&mut model_map, &mapping.from, &ch.status);
            }
        }

        // Add prefix-qualified aliases if requested (Go lines 482-487).
        if input.include_prefix
            && let Some(settings) = &ch.settings
            && !settings.extra_model_prefix.is_empty()
        {
            for model_id in &ch.supported_models {
                let prefixed = format!("{}/{}", settings.extra_model_prefix, model_id);
                set_model_status(&mut model_map, &prefixed, &ch.status);
            }
        }
    }

    let mut models: Vec<ModelIdentityWithStatus> = model_map
        .into_iter()
        .map(|(id, status)| ModelIdentityWithStatus { id, status })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::objects::channel_settings::{ChannelSettings, ModelMapping};
    use std::collections::HashMap;

    /// Build the same 4-channel fixture as Go
    /// `TestChannelService_ListModels` (`channel_test.go:26-76`): an enabled
    /// openai channel, a disabled anthropic channel with one model mapping,
    /// an archived openai channel, and an enabled deepseek channel with an
    /// `extra_model_prefix`.
    fn go_fixture() -> Vec<ListModelsChannel> {
        vec![
            // enabledCh (lines 26-35)
            ListModelsChannel {
                status: channel_status::ENABLED.into(),
                supported_models: vec!["gpt-4".into(), "gpt-3.5-turbo".into()],
                settings: None,
            },
            // disabledCh (lines 37-51) — has a model mapping
            ListModelsChannel {
                status: channel_status::DISABLED.into(),
                supported_models: vec!["claude-3-opus-20240229".into()],
                settings: Some(ChannelSettings {
                    model_mappings: vec![ModelMapping {
                        from: "claude-3-opus".into(),
                        to: "claude-3-opus-20240229".into(),
                    }],
                    ..Default::default()
                }),
            },
            // archivedCh (lines 53-62)
            ListModelsChannel {
                status: channel_status::ARCHIVED.into(),
                supported_models: vec!["gpt-4-turbo".into()],
                settings: None,
            },
            // prefixCh (lines 64-76) — extra_model_prefix
            ListModelsChannel {
                status: channel_status::ENABLED.into(),
                supported_models: vec!["deepseek-chat".into(), "deepseek-reasoner".into()],
                settings: Some(ChannelSettings {
                    extra_model_prefix: "deepseek".into(),
                    ..Default::default()
                }),
            },
        ]
    }

    fn ids(result: &[ModelIdentityWithStatus]) -> Vec<String> {
        let mut v: Vec<String> = result.iter().map(|m| m.id.clone()).collect();
        v.sort();
        v
    }

    // ---- status_priority / set_model_status unit parity ----

    #[test]
    fn s10_status_priority_matches_go_statuspriority_map() {
        // Go channel.go:420-424.
        assert_eq!(status_priority(channel_status::ENABLED), 3);
        assert_eq!(status_priority(channel_status::DISABLED), 2);
        assert_eq!(status_priority(channel_status::ARCHIVED), 1);
        assert_eq!(status_priority("unknown"), 0);
    }

    #[test]
    fn s10_set_model_status_inserts_when_absent() {
        // Go setModelStatus: !exists branch.
        let mut models = HashMap::new();
        set_model_status(&mut models, "gpt-4", channel_status::ENABLED);
        assert_eq!(
            models.get("gpt-4").map(|s| s.as_str()),
            Some(channel_status::ENABLED)
        );
    }

    #[test]
    fn s10_set_model_status_replaces_when_higher_priority() {
        // Go setModelStatus: existing disabled (2), new enabled (3) -> replace.
        let mut models = HashMap::new();
        set_model_status(&mut models, "m", channel_status::DISABLED);
        set_model_status(&mut models, "m", channel_status::ENABLED);
        assert_eq!(
            models.get("m").map(|s| s.as_str()),
            Some(channel_status::ENABLED)
        );
    }

    #[test]
    fn s10_set_model_status_keeps_existing_when_lower_or_equal_priority() {
        // Go setModelStatus: existing enabled (3), new disabled (2) -> keep.
        let mut models = HashMap::new();
        set_model_status(&mut models, "m", channel_status::ENABLED);
        set_model_status(&mut models, "m", channel_status::DISABLED);
        assert_eq!(
            models.get("m").map(|s| s.as_str()),
            Some(channel_status::ENABLED)
        );
        // equal priority (archived vs archived) -> keep (strictly-greater rule)
        set_model_status(&mut models, "m2", channel_status::ARCHIVED);
        set_model_status(&mut models, "m2", channel_status::ARCHIVED);
        assert_eq!(
            models.get("m2").map(|s| s.as_str()),
            Some(channel_status::ARCHIVED)
        );
    }

    // ---- Go TestChannelService_ListModels subtests (lines 17-209) ----

    #[test]
    fn s10_list_enabled_models_only_default() {
        // Go channel_test.go:85-100 "list enabled models only (default)".
        // StatusIn nil -> default enabled. wantModelIDs = enabled channels'
        // supported_models (gpt-4, gpt-3.5-turbo, deepseek-chat, deepseek-reasoner).
        let channels = go_fixture();
        let input = ListModelsInput {
            status_in: vec![],
            include_all_channel_models: false,
            include_mapping: false,
            include_prefix: false,
        };
        let result = list_models_pure(&channels, &input);
        let want = vec![
            "deepseek-chat",
            "deepseek-reasoner",
            "gpt-3.5-turbo",
            "gpt-4",
        ];
        assert_eq!(ids(&result), want);

        // wantStatuses: all enabled.
        let mut want_status: HashMap<&str, &str> = HashMap::new();
        for m in &want {
            want_status.insert(m, channel_status::ENABLED);
        }
        for m in &result {
            if let Some(w) = want_status.get(m.id.as_str()) {
                assert_eq!(m.status, *w, "status mismatch for {}", m.id);
            }
        }
    }

    #[test]
    fn s10_list_enabled_models_with_mappings() {
        // Go channel_test.go:101-109 "list enabled models with mappings".
        // Enabled channels have no mappings, so result == supported_models.
        let channels = go_fixture();
        let input = ListModelsInput {
            status_in: vec![channel_status::ENABLED.into()],
            include_all_channel_models: false,
            include_mapping: true,
            include_prefix: false,
        };
        let result = list_models_pure(&channels, &input);
        let want = vec![
            "deepseek-chat",
            "deepseek-reasoner",
            "gpt-3.5-turbo",
            "gpt-4",
        ];
        assert_eq!(ids(&result), want);
    }

    #[test]
    fn s10_list_enabled_models_with_prefix() {
        // Go channel_test.go:110-122 "list enabled models with prefix".
        // prefixCh adds "deepseek/deepseek-chat", "deepseek/deepseek-reasoner".
        let channels = go_fixture();
        let input = ListModelsInput {
            status_in: vec![channel_status::ENABLED.into()],
            include_all_channel_models: false,
            include_mapping: false,
            include_prefix: true,
        };
        let result = list_models_pure(&channels, &input);
        let want = vec![
            "deepseek-chat",
            "deepseek-reasoner",
            "deepseek/deepseek-chat",
            "deepseek/deepseek-reasoner",
            "gpt-3.5-turbo",
            "gpt-4",
        ];
        assert_eq!(ids(&result), want);
    }

    #[test]
    fn s10_list_disabled_models_with_mappings() {
        // Go channel_test.go:123-136 "list disabled models with mappings".
        // disabledCh has mapping {claude-3-opus -> claude-3-opus-20240229};
        // target is supported, so "claude-3-opus" alias is admitted.
        let channels = go_fixture();
        let input = ListModelsInput {
            status_in: vec![channel_status::DISABLED.into()],
            include_all_channel_models: false,
            include_mapping: true,
            include_prefix: false,
        };
        let result = list_models_pure(&channels, &input);
        let want = vec!["claude-3-opus", "claude-3-opus-20240229"];
        assert_eq!(ids(&result), want);

        // wantStatuses: both disabled.
        for m in &result {
            assert_eq!(
                m.status,
                channel_status::DISABLED,
                "status mismatch for {}",
                m.id
            );
        }
    }

    #[test]
    fn s10_list_multiple_statuses() {
        // Go channel_test.go:137-149 "list multiple statuses".
        // enabled + disabled, no mapping/prefix.
        let channels = go_fixture();
        let input = ListModelsInput {
            status_in: vec![
                channel_status::ENABLED.into(),
                channel_status::DISABLED.into(),
            ],
            include_all_channel_models: false,
            include_mapping: false,
            include_prefix: false,
        };
        let result = list_models_pure(&channels, &input);
        let want = vec![
            "claude-3-opus-20240229",
            "deepseek-chat",
            "deepseek-reasoner",
            "gpt-3.5-turbo",
            "gpt-4",
        ];
        assert_eq!(ids(&result), want);
    }

    #[test]
    fn s10_list_all_statuses_with_mappings_and_prefix() {
        // Go channel_test.go:150-163 "list all statuses with mappings and prefix".
        let channels = go_fixture();
        let input = ListModelsInput {
            status_in: vec![
                channel_status::ENABLED.into(),
                channel_status::DISABLED.into(),
                channel_status::ARCHIVED.into(),
            ],
            include_all_channel_models: false,
            include_mapping: true,
            include_prefix: true,
        };
        let result = list_models_pure(&channels, &input);
        let want = vec![
            "claude-3-opus",
            "claude-3-opus-20240229",
            "deepseek-chat",
            "deepseek-reasoner",
            "deepseek/deepseek-chat",
            "deepseek/deepseek-reasoner",
            "gpt-3.5-turbo",
            "gpt-4",
            "gpt-4-turbo",
        ];
        assert_eq!(ids(&result), want);
    }

    #[test]
    fn s10_list_archived_models_only() {
        // Go channel_test.go:164-176 "list archived models only".
        let channels = go_fixture();
        let input = ListModelsInput {
            status_in: vec![channel_status::ARCHIVED.into()],
            include_all_channel_models: false,
            include_mapping: false,
            include_prefix: false,
        };
        let result = list_models_pure(&channels, &input);
        let want = vec!["gpt-4-turbo"];
        assert_eq!(ids(&result), want);
        for m in &result {
            assert_eq!(
                m.status,
                channel_status::ARCHIVED,
                "status mismatch for {}",
                m.id
            );
        }
    }

    // ---- mapping-admission edge: target-not-supported is dropped ----

    #[test]
    fn s10_mapping_alias_dropped_when_target_not_supported() {
        // Go channel.go:475 — `slices.Contains(ch.SupportedModels, mapping.To)`
        // gate. A mapping whose `to` is NOT in supported_models is silently
        // dropped (not asserted by a Go subtest, but is the load-bearing rule
        // that makes "list disabled models with mappings" admit exactly one
        // alias).
        let channels = vec![ListModelsChannel {
            status: channel_status::ENABLED.into(),
            supported_models: vec!["gpt-4".into()],
            settings: Some(ChannelSettings {
                model_mappings: vec![ModelMapping {
                    from: "gpt-3.5".into(),
                    to: "gpt-3.5-turbo".into(), // not in supported_models
                }],
                ..Default::default()
            }),
        }];
        let input = ListModelsInput {
            status_in: vec![channel_status::ENABLED.into()],
            include_mapping: true,
            ..Default::default()
        };
        let result = list_models_pure(&channels, &input);
        // Only the direct model; the "gpt-3.5" alias is dropped.
        assert_eq!(ids(&result), vec!["gpt-4"]);
    }

    // ---- status-priority collision across channels ----

    #[test]
    fn s10_same_model_in_enabled_and_disabled_keeps_enabled() {
        // Go setModelStatus priority: when the same model id appears in both an
        // enabled and a disabled channel, the enabled status (priority 3)
        // wins. Not directly a Go subtest, but the rule that
        // `checkStatuses` in the Go test relies on.
        let channels = vec![
            ListModelsChannel {
                status: channel_status::DISABLED.into(),
                supported_models: vec!["shared".into()],
                settings: None,
            },
            ListModelsChannel {
                status: channel_status::ENABLED.into(),
                supported_models: vec!["shared".into()],
                settings: None,
            },
        ];
        let input = ListModelsInput {
            status_in: vec![
                channel_status::ENABLED.into(),
                channel_status::DISABLED.into(),
            ],
            ..Default::default()
        };
        let result = list_models_pure(&channels, &input);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "shared");
        assert_eq!(result[0].status, channel_status::ENABLED);
    }
}
