//! Host-side channel-association matcher wiring.
//!
//! Ports the model↔channel matching surface that backs the admin Models page:
//!   - `Query.queryModelChannelConnections` ("which channels serve this model")
//!   - `Query.queryUnassociatedChannels` ("channels whose models no rule covers")
//!
//! Go references (the contract — never guess):
//!   - `conduit/internal/server/biz/model_association_matcher.go` — the six
//!     per-type match fns + `DuplicateKeyTracker` + `MatchConnections`.
//!   - `conduit/internal/server/biz/model.go` — `QueryModelChannelConnections`
//!     (lines 521-545) and `QueryUnassociatedChannels` / `findUnassociatedChannels`
//!     (lines 745-865).
//!   - `conduit/internal/server/biz/channel_llm.go` — `GetModelEntries`
//!     (lines 1107-1236): the request-model → `ChannelModelEntry` enumeration.
//!
//! ## Reuse of the canonical matcher
//!
//! The six match fns, the `(channel, model)` dedup tracker, `MatchConnections`,
//! and `findUnassociatedChannels` are ALREADY ported (and tested against the Go
//! golden cases) in [`conduit_services::model_service`] as
//! [`match_connections`] / [`find_unassociated_channels`] over the pure
//! [`MatcherChannel`] view. This module does NOT duplicate that logic. It
//! supplies the two pieces the service matcher deliberately leaves to the host:
//!
//!   1. `GetModelEntries` — turning a live [`ChannelRow`] (supported_models +
//!      the settings JSON: prefix / auto-trim / mappings / hide flags /
//!      lowercase) into the request-model → entry map. The service
//!      [`MatcherChannel`] only carries the entry *keys* (the request-model
//!      ids); the entry payload (`actualModel` / `source`) needed for the
//!      GraphQL `ChannelModelEntry` is rebuilt here.
//!   2. mapping the matcher's `channel_id` / `model_id` results back onto the
//!      live GraphQL `Channel` (via `crate::conv::channel_row_to_gql`) and the
//!      typed entries.

use std::collections::BTreeMap;

use conduit_admin_graphql::channel::Channel as GqlChannel;
use conduit_admin_graphql::model_ext::{
    ChannelModelEntry as GqlChannelModelEntry, ModelChannelConnection, UnassociatedChannel,
};
use conduit_core::objects as core;
use conduit_core::objects::SystemModelSettings;
use conduit_core::objects::channel_settings::ChannelSettings;
use conduit_db::row::{ChannelRow, ModelRow};
use conduit_services::model_service::{
    MatcherChannel, effective_model_associations, find_unassociated_channels, match_connections,
};

/// A resolved request-model entry, mirroring Go `biz.ChannelModelEntry`
/// (`channel.go:30-39`). `request_model` is the id used in requests,
/// `actual_model` is what is sent upstream, `source` is how the entry arose
/// (`direct` / `prefix` / `auto_trim` / `mapping`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelModelEntry {
    pub request_model: String,
    pub actual_model: String,
    pub source: String,
}

/// Source-priority weights used to resolve lowercase-collision winners, mirroring
/// Go `sourcePriority` (`channel_llm.go:54`: direct > auto_trim > mapping > prefix).
fn source_priority(source: &str) -> i32 {
    match source {
        "direct" => 4,
        "auto_trim" => 3,
        "mapping" => 2,
        "prefix" => 1,
        _ => 0,
    }
}

/// Enumerate every request-model this channel can serve, keyed by request model.
///
/// 1:1 port of Go `(*Channel).GetModelEntries` (`channel_llm.go:1107-1236`):
///   1. direct models from `supported_models`;
///   2. prefixed models (`extra_model_prefix`);
///   3. auto-trimmed models (`auto_trimed_model_prefixes`);
///   4. model mappings (only when the target is supported), with the
///      `hide_mapped_models` cleanup of the target's other access paths;
///   5. `hide_original_models` drops `direct` entries;
///   6. `lowercase_model_id` lowercases the matching keys, resolving collisions
///      by `source_priority`.
///
/// A `BTreeMap` is used (Go uses a plain map): the matcher only tests presence
/// and iterates, and deterministic order keeps the GraphQL output stable.
pub fn build_model_entries(
    supported_models: &[String],
    settings: &ChannelSettings,
) -> BTreeMap<String, ChannelModelEntry> {
    let mut entries: BTreeMap<String, ChannelModelEntry> = BTreeMap::new();

    // 1. Direct models.
    for model in supported_models {
        entries
            .entry(model.clone())
            .or_insert_with(|| ChannelModelEntry {
                request_model: model.clone(),
                actual_model: model.clone(),
                source: "direct".to_string(),
            });
    }

    // 2. Prefixed models.
    if !settings.extra_model_prefix.is_empty() {
        let prefix = &settings.extra_model_prefix;
        for model in supported_models {
            let prefixed = format!("{prefix}/{model}");
            entries
                .entry(prefixed.clone())
                .or_insert_with(|| ChannelModelEntry {
                    request_model: prefixed.clone(),
                    actual_model: model.clone(),
                    source: "prefix".to_string(),
                });
        }
    }

    // 3. Auto-trimmed models.
    for raw_prefix in &settings.auto_trimed_model_prefixes {
        if raw_prefix.is_empty() {
            continue;
        }
        let prefix = format!("{raw_prefix}/");
        for model in supported_models {
            if let Some(trimmed) = model.strip_prefix(&prefix) {
                let trimmed = trimmed.to_string();
                entries
                    .entry(trimmed.clone())
                    .or_insert_with(|| ChannelModelEntry {
                        request_model: trimmed.clone(),
                        actual_model: model.clone(),
                        source: "auto_trim".to_string(),
                    });
            }
        }
    }

    // 4. Model mappings (only when the target model is supported).
    for mapping in &settings.model_mappings {
        if !supported_models.contains(&mapping.to) {
            continue;
        }
        if entries.contains_key(&mapping.from) {
            continue;
        }
        entries.insert(
            mapping.from.clone(),
            ChannelModelEntry {
                request_model: mapping.from.clone(),
                actual_model: mapping.to.clone(),
                source: "mapping".to_string(),
            },
        );
        // When hide_mapped_models is on, remove every non-mapping entry that
        // resolves to the same target model (its other access paths).
        if settings.hide_mapped_models {
            let target = mapping.to.clone();
            entries.retain(|_, entry| !(entry.actual_model == target && entry.source != "mapping"));
        }
    }

    // 5. Hide original (direct) models.
    if settings.hide_original_models {
        entries.retain(|_, entry| entry.source != "direct");
    }

    // 6. Lowercase model ids for matching (actual_model is NOT lowered).
    if settings.lowercase_model_id {
        let mut lowered: BTreeMap<String, ChannelModelEntry> = BTreeMap::new();
        for (key, mut entry) in entries.into_iter() {
            let lower_key = key.to_lowercase();
            entry.request_model = entry.request_model.to_lowercase();
            match lowered.get(&lower_key) {
                Some(existing)
                    if source_priority(&entry.source) <= source_priority(&existing.source) => {}
                _ => {
                    lowered.insert(lower_key, entry);
                }
            }
        }
        entries = lowered;
    }

    entries
}

/// A channel prepared for matching: the numeric id + name + tags + its full
/// entry map, plus the source row (kept so the live GraphQL `Channel` can be
/// rebuilt for each emitted connection — a channel may appear in more than one
/// connection when several associations match it with different models).
struct ChannelView {
    row: ChannelRow,
    channel_id: i64,
    entries: BTreeMap<String, ChannelModelEntry>,
}

impl ChannelView {
    fn from_row(row: ChannelRow) -> Self {
        // ChannelRow.id is the bare numeric DB id (channel_row_to_gql wraps it
        // into a `gid://...`). Associations reference the numeric id.
        let channel_id = row.id.parse::<i64>().unwrap_or(0);
        let settings: ChannelSettings =
            serde_json::from_value(row.settings.clone()).unwrap_or_default();
        let entries = build_model_entries(&row.supported_models, &settings);
        Self {
            row,
            channel_id,
            entries,
        }
    }

    fn matcher(&self) -> MatcherChannel {
        MatcherChannel::new(
            self.channel_id,
            self.row.name.clone(),
            self.entries.keys().cloned(),
        )
        .with_tags(self.row.tags.clone())
    }
}

/// Convert a matched request-model id into the GraphQL `ChannelModelEntry`,
/// pulling `actual_model` / `source` from the channel's entry map. Falls back to
/// a `direct` self-mapping if the id is somehow absent (defensive; the matcher
/// only emits ids drawn from these very entries).
fn entry_to_gql(view: &ChannelView, request_model: &str) -> GqlChannelModelEntry {
    match view.entries.get(request_model) {
        Some(entry) => GqlChannelModelEntry {
            request_model: entry.request_model.clone(),
            actual_model: entry.actual_model.clone(),
            source: entry.source.clone(),
        },
        None => GqlChannelModelEntry {
            request_model: request_model.to_string(),
            actual_model: request_model.to_string(),
            source: "direct".to_string(),
        },
    }
}

/// Resolve model→channel connections for the given associations against the
/// loaded channel rows. Mirrors Go `QueryModelChannelConnections` after the DB
/// load: build channel views, run the canonical matcher, and map each
/// `AssociationConnection` back onto the live GraphQL `Channel` + typed entries.
///
/// `channel_rows` must already be ordered as Go orders them (ordering_weight
/// desc); the matcher preserves channel iteration order per association.
pub fn resolve_model_channel_connections(
    channel_rows: Vec<ChannelRow>,
    associations: &[core::ModelAssociation],
) -> Vec<ModelChannelConnection> {
    let views: Vec<ChannelView> = channel_rows
        .into_iter()
        .map(ChannelView::from_row)
        .collect();
    let matchers: Vec<MatcherChannel> = views.iter().map(ChannelView::matcher).collect();

    let connections = match_connections(associations, &matchers);

    let mut out = Vec::with_capacity(connections.len());
    for conn in connections {
        let Some(view) = views.iter().find(|v| v.channel_id == conn.channel_id) else {
            continue;
        };
        let models: Vec<GqlChannelModelEntry> = conn
            .model_ids
            .iter()
            .map(|id| entry_to_gql(view, id))
            .collect();
        out.push(ModelChannelConnection {
            channel: gql_channel(view),
            models,
            // Go `ModelChannelConnection.Priority` is int; GraphQL exposes Int
            // (i32). Priorities are small config values, so the cast is safe.
            priority: conn.priority as i32,
        });
    }
    out
}

/// Count unique channels matched by the effective association set.
///
/// This is the host-side equivalent of Go
/// `ModelService.countAssociatedChannels`: run the canonical matcher over all
/// enabled/disabled channels, then deduplicate by channel id because multiple
/// associations may emit separate connections for the same channel.
pub fn count_associated_channels(
    channel_rows: Vec<ChannelRow>,
    associations: &[core::ModelAssociation],
) -> usize {
    if channel_rows.is_empty() || associations.is_empty() {
        return 0;
    }
    let views: Vec<ChannelView> = channel_rows
        .into_iter()
        .map(ChannelView::from_row)
        .collect();
    let matchers: Vec<MatcherChannel> = views.iter().map(ChannelView::matcher).collect();
    match_connections(associations, &matchers)
        .into_iter()
        .map(|connection| connection.channel_id)
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// Resolve the "unassociated channels" list: channels carrying at least one
/// request-model that no association matched. Mirrors Go
/// `findUnassociatedChannels` (`model.go:810-865`) after the DB load.
pub fn resolve_unassociated_channels(
    channel_rows: Vec<ChannelRow>,
    associations: &[core::ModelAssociation],
) -> Vec<UnassociatedChannel> {
    if channel_rows.is_empty() {
        return Vec::new();
    }
    let views: Vec<ChannelView> = channel_rows
        .into_iter()
        .map(ChannelView::from_row)
        .collect();
    let matchers: Vec<MatcherChannel> = views.iter().map(ChannelView::matcher).collect();

    let unassociated = find_unassociated_channels(&matchers, associations);

    let mut out = Vec::with_capacity(unassociated.len());
    for item in unassociated {
        let Some(view) = views.iter().find(|v| v.channel_id == item.channel_id) else {
            continue;
        };
        out.push(UnassociatedChannel {
            channel: gql_channel(view),
            models: item.models,
        });
    }
    out
}

/// Compute the flattened effective associations across a set of models,
/// mirroring the Go `QueryUnassociatedChannels` loop (`model.go:764-769`):
/// each model contributes `EffectiveModelAssociations(systemSettings, model)`.
/// Callers pass only the enabled+disabled models (the Go query's status filter).
pub fn effective_associations_for_models(
    models: &[ModelRow],
    system_settings: &SystemModelSettings,
) -> Vec<core::ModelAssociation> {
    let mut all = Vec::new();
    for model in models {
        let settings: Option<core::ModelSettings> =
            serde_json::from_value(model.settings.clone()).ok();
        all.extend(effective_model_associations(
            system_settings,
            &model.developer,
            &model.model_id,
            settings.as_ref(),
        ));
    }
    all
}

/// Rebuild the live GraphQL `Channel` for a view (clones the row; a channel may
/// surface in multiple connections).
fn gql_channel(view: &ChannelView) -> GqlChannel {
    crate::conv::channel_row_to_gql(view.row.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::objects::channel_settings::ModelMapping;

    type TestError = Box<dyn std::error::Error>;

    fn default_settings() -> ChannelSettings {
        ChannelSettings::default()
    }

    /// Build a minimal ChannelRow for matcher tests (all non-essential columns
    /// take their zero/default values).
    fn channel_row(id: &str, name: &str, supported: &[&str], tags: &[&str]) -> ChannelRow {
        ChannelRow {
            id: id.to_string(),
            channel_type: "openai".to_string(),
            base_url: None,
            website_url: None,
            quota_currency: None,
            actual_quota_used: None,
            quota_remaining: None,
            name: name.to_string(),
            status: "enabled".to_string(),
            credentials: serde_json::Value::Object(Default::default()),
            disabled_api_keys: serde_json::Value::Null,
            supported_models: supported.iter().map(|s| s.to_string()).collect(),
            manual_models: Vec::new(),
            auto_sync_supported_models: false,
            auto_sync_model_pattern: String::new(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            default_test_model: String::new(),
            policies: serde_json::json!({"stream": "unlimited"}),
            settings: serde_json::json!({"model_mappings": []}),
            ordering_weight: 0,
            error_message: None,
            remark: None,
            endpoints: Vec::new(),
            created_at: chrono::DateTime::<chrono::Utc>::default(),
            updated_at: chrono::DateTime::<chrono::Utc>::default(),
            deleted_at: None,
        }
    }

    fn channel_model_assoc(
        channel_id: i64,
        model_id: &str,
        priority: i64,
    ) -> core::ModelAssociation {
        core::ModelAssociation {
            kind: "channel_model".to_string(),
            priority,
            channel_model: Some(core::ChannelModelAssociation {
                channel_id,
                model_id: model_id.to_string(),
            }),
            ..core::ModelAssociation::default()
        }
    }

    fn model_assoc(model_id: &str, priority: i64) -> core::ModelAssociation {
        core::ModelAssociation {
            kind: "model".to_string(),
            priority,
            model_id: Some(core::ModelIDAssociation {
                model_id: model_id.to_string(),
                exclude: Vec::new(),
            }),
            ..core::ModelAssociation::default()
        }
    }

    fn regex_assoc(pattern: &str, priority: i64) -> core::ModelAssociation {
        core::ModelAssociation {
            kind: "regex".to_string(),
            priority,
            regex: Some(core::RegexAssociation {
                pattern: pattern.to_string(),
                exclude: Vec::new(),
            }),
            ..core::ModelAssociation::default()
        }
    }

    // --- GetModelEntries port -------------------------------------------

    #[test]
    fn build_entries_direct_models() {
        let entries = build_model_entries(
            &["gpt-4o".to_string(), "gpt-4o-mini".to_string()],
            &default_settings(),
        );
        assert_eq!(entries.len(), 2);
        let e = match entries.get("gpt-4o") {
            Some(e) => e,
            None => panic!("gpt-4o entry missing"),
        };
        assert_eq!(e.actual_model, "gpt-4o");
        assert_eq!(e.source, "direct");
    }

    #[test]
    fn build_entries_prefix_and_mapping() {
        let mut settings = default_settings();
        settings.extra_model_prefix = "azure".to_string();
        settings.model_mappings = vec![ModelMapping {
            from: "gpt4".to_string(),
            to: "gpt-4o".to_string(),
        }];
        let entries = build_model_entries(&["gpt-4o".to_string()], &settings);
        // direct + prefixed + mapping alias
        assert!(entries.contains_key("gpt-4o"));
        let prefixed = match entries.get("azure/gpt-4o") {
            Some(e) => e,
            None => panic!("prefixed entry missing"),
        };
        assert_eq!(prefixed.actual_model, "gpt-4o");
        assert_eq!(prefixed.source, "prefix");
        let alias = match entries.get("gpt4") {
            Some(e) => e,
            None => panic!("mapping alias missing"),
        };
        assert_eq!(alias.actual_model, "gpt-4o");
        assert_eq!(alias.source, "mapping");
    }

    // --- channel_model exact match --------------------------------------

    #[test]
    fn channel_model_exact_match() {
        let rows = vec![
            channel_row("1", "c1", &["gpt-4o"], &[]),
            channel_row("2", "c2", &["claude-3"], &[]),
        ];
        let connections =
            resolve_model_channel_connections(rows, &[channel_model_assoc(1, "gpt-4o", 5)]);
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].priority, 5);
        assert_eq!(connections[0].models.len(), 1);
        assert_eq!(connections[0].models[0].request_model, "gpt-4o");
        assert_eq!(connections[0].models[0].source, "direct");
    }

    #[test]
    fn channel_model_no_match_when_model_absent() {
        let rows = vec![channel_row("1", "c1", &["gpt-4o"], &[])];
        let connections =
            resolve_model_channel_connections(rows, &[channel_model_assoc(1, "missing", 0)]);
        assert!(connections.is_empty());
    }

    // --- model (by supported model) match -------------------------------

    #[test]
    fn model_match_across_channels() {
        let rows = vec![
            channel_row("1", "c1", &["gpt-4o"], &[]),
            channel_row("2", "c2", &["gpt-4o", "claude-3"], &[]),
            channel_row("3", "c3", &["claude-3"], &[]),
        ];
        let connections = resolve_model_channel_connections(rows, &[model_assoc("gpt-4o", 1)]);
        // channels 1 and 2 serve gpt-4o; channel 3 does not.
        assert_eq!(connections.len(), 2);
        for conn in &connections {
            assert_eq!(conn.models.len(), 1);
            assert_eq!(conn.models[0].request_model, "gpt-4o");
        }
    }

    // --- regex match ----------------------------------------------------

    #[test]
    fn regex_match_selects_models() {
        let rows = vec![channel_row(
            "1",
            "c1",
            &["gpt-4o", "gpt-4o-mini", "claude-3"],
            &[],
        )];
        let connections = resolve_model_channel_connections(rows, &[regex_assoc("gpt-.*", 2)]);
        assert_eq!(connections.len(), 1);
        let matched: Vec<&str> = connections[0]
            .models
            .iter()
            .map(|m| m.request_model.as_str())
            .collect();
        assert!(matched.contains(&"gpt-4o"));
        assert!(matched.contains(&"gpt-4o-mini"));
        assert!(!matched.contains(&"claude-3"));
    }

    #[test]
    fn invalid_regex_is_skipped_not_errored() {
        let rows = vec![channel_row("1", "c1", &["gpt-4o"], &[])];
        // An unbalanced group is an invalid regex → matches nothing (Go xregexp
        // compileErr → false), and the query must not fail.
        let connections = resolve_model_channel_connections(rows, &[regex_assoc("gpt-(", 0)]);
        assert!(connections.is_empty());
    }

    // --- duplicate tracker dedup ----------------------------------------

    #[test]
    fn duplicate_channel_model_deduped_across_associations() {
        let rows = vec![channel_row("1", "c1", &["gpt-4o"], &[])];
        // Two associations both resolve (channel 1, gpt-4o); the shared tracker
        // must emit it only once.
        let connections = resolve_model_channel_connections(
            rows,
            &[
                channel_model_assoc(1, "gpt-4o", 1),
                model_assoc("gpt-4o", 2),
            ],
        );
        let total_models: usize = connections.iter().map(|c| c.models.len()).sum();
        assert_eq!(
            total_models, 1,
            "the (channel, model) pair must dedup to one"
        );
    }

    #[test]
    fn associated_channel_count_deduplicates_channels() {
        let rows = vec![
            channel_row("1", "c1", &["gpt-4o"], &[]),
            channel_row("2", "c2", &["gpt-4o"], &[]),
            channel_row("3", "c3", &["claude-3"], &[]),
        ];
        let count = count_associated_channels(
            rows,
            &[
                model_assoc("gpt-4o", 1),
                channel_model_assoc(1, "gpt-4o", 2),
            ],
        );
        assert_eq!(count, 2, "channel 1 must not be counted twice");
    }

    #[test]
    fn associated_channel_count_is_zero_without_associations() {
        let rows = vec![channel_row("1", "c1", &["gpt-4o"], &[])];
        assert_eq!(count_associated_channels(rows, &[]), 0);
    }

    // --- unassociated channels ------------------------------------------

    #[test]
    fn unassociated_lists_uncovered_models() -> Result<(), TestError> {
        let rows = vec![
            channel_row("1", "c1", &["gpt-4o", "gpt-3.5"], &[]),
            channel_row("2", "c2", &["claude-3"], &[]),
        ];
        // Only gpt-4o on channel 1 is associated.
        let unassoc = resolve_unassociated_channels(rows, &[channel_model_assoc(1, "gpt-4o", 0)]);
        // channel 1 still has gpt-3.5 uncovered; channel 2 has claude-3 uncovered.
        assert_eq!(unassoc.len(), 2);
        let c1 = unassoc
            .iter()
            .find(|u| u.channel.name == "c1")
            .ok_or("c1 missing from unassociated")?;
        assert_eq!(c1.models, vec!["gpt-3.5".to_string()]);
        Ok(())
    }

    #[test]
    fn unassociated_empty_channels_returns_empty() {
        let unassoc = resolve_unassociated_channels(Vec::new(), &[]);
        assert!(unassoc.is_empty());
    }
}
