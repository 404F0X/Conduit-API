//! S05 supported-model merge + S06 model-entry map.
//!
//! Pure decision layer that recomputes a channel's runtime supported-model
//! view and the request-model -> provider-model mapping from the channel's
//! three source fields (`supported_models`, `manual_models`,
//! `auto_sync_supported_models`) plus the `ChannelSettings` that drive
//! aliasing and suppression.
//!
//! Ported from the Go biz package:
//! - `internal/server/biz/channel_model_sync.go`
//!   `syncChannelModelsForChannel` (merge + regex filter via `xregexp.Filter`).
//! - `internal/server/biz/channel_llm.go` `Channel.GetModelEntries`
//!   (source-priority tie-break, prefix / auto-trim / mapping aliasing, hide
//!   suppression, lowercase normalization).
//!
//! The wire-format types live in
//! [`conduit_core::objects::channel_settings`]; this module is pure logic over
//! those types.

use std::collections::BTreeMap;

use conduit_core::objects::channel_settings::ChannelSettings;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// An entry describing how a request model id resolves to the provider model.
/// Ported from Go `ChannelModelEntry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelModelEntry {
    /// Model name as it appears in the request.
    pub request_model: String,
    /// Actual model name sent to the provider.
    pub actual_model: String,
    /// How this model was derived: one of the [`ModelSource`] constants
    /// (`"direct"`, `"prefix"`, `"auto_trim"`, `"mapping"`). Stored as a string
    /// on the wire to match Go's bare `string` field.
    pub source: ModelSource,
}

/// How a [`ChannelModelEntry`] was derived. Mirrors the Go `Source` string
/// field; only the four known tiers are produced by
/// [`ChannelModelEntryMap::from_channel`], and the tie-breaking order matches
/// Go `sourcePriority` (`direct > auto_trim > mapping > prefix`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    Direct,
    Prefix,
    /// Serialized as `"auto_trim"` (matching Go).
    AutoTrim,
    Mapping,
}

impl ModelSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Prefix => "prefix",
            Self::AutoTrim => "auto_trim",
            Self::Mapping => "mapping",
        }
    }

    /// Tie-breaking priority used when two entries collide after lowercasing.
    /// Mirrors Go `sourcePriority`: `direct > auto_trim > mapping > prefix`.
    pub const fn priority(self) -> i32 {
        match self {
            Self::Direct => 4,
            Self::AutoTrim => 3,
            Self::Mapping => 2,
            Self::Prefix => 1,
        }
    }
}

impl From<ModelSource> for &'static str {
    fn from(source: ModelSource) -> Self {
        source.as_str()
    }
}

/// Supported-model runtime view (S05). Mirrors the merge performed by Go
/// `syncChannelModelsForChannel`: the runtime supported-models list is the
/// deduplicated union of the channel's stored `supported_models`, its
/// `manual_models`, and the regex-filtered `auto_sync_supported_models`.
///
/// Note: in Go the sync job *persists* the merged list back into
/// `supported_models`; this helper recomputes the same view from the three
/// source fields so it stays correct for channels that have not yet been
/// re-synced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SupportedModelSet {
    /// Channel's persisted supported-model list (manual + last-synced auto).
    pub supported_models: Vec<String>,
    /// Explicitly user-curated models (`manual_models`).
    pub manual_models: Vec<String>,
    /// Most recent models fetched from the provider (`auto_sync_supported_models`).
    pub auto_sync_supported_models: Vec<String>,
    /// RE2 pattern filtering `auto_sync_supported_models` (`auto_sync_model_pattern`).
    /// Empty / `None` keeps all fetched models.
    pub auto_sync_model_pattern: Option<String>,
}

impl SupportedModelSet {
    pub fn new(
        supported_models: impl Into<Vec<String>>,
        manual_models: impl Into<Vec<String>>,
        auto_sync_supported_models: impl Into<Vec<String>>,
    ) -> Self {
        Self {
            supported_models: supported_models.into(),
            manual_models: manual_models.into(),
            auto_sync_supported_models: auto_sync_supported_models.into(),
            auto_sync_model_pattern: None,
        }
    }

    pub fn with_auto_sync_model_pattern(mut self, pattern: impl Into<String>) -> Self {
        let pattern = pattern.into();
        self.auto_sync_model_pattern = if pattern.trim().is_empty() {
            None
        } else {
            Some(pattern)
        };
        self
    }

    /// Merged, deduplicated, order-preserving model list. Filters
    /// `auto_sync_supported_models` through `auto_sync_model_pattern` (RE2,
    /// matching Go `xregexp.Filter`). Invalid regex falls back to "keep all"
    /// (Go logs a warning and skips the filter; we mirror that here rather than
    /// erroring, since the persisted list is already authoritative).
    pub fn merged_models(&self) -> Vec<String> {
        let mut merged: Vec<String> = Vec::new();
        for model in &self.supported_models {
            push_unique_model(&mut merged, model);
        }
        for model in &self.manual_models {
            push_unique_model(&mut merged, model);
        }

        let pattern = self.auto_sync_model_pattern.as_deref();
        let regex = pattern.map(|p| (p, Regex::new(p)));
        for model in &self.auto_sync_supported_models {
            let keep = match &regex {
                None => true,
                Some((_, Ok(compiled))) => compiled.is_match(model),
                // Invalid regex: keep all (Go logs + skips the filter).
                Some((_, Err(_))) => true,
            };
            if keep {
                push_unique_model(&mut merged, model);
            }
        }

        merged
    }
}

/// Resolved model-entry map (S06). Ported 1:1 from Go
/// `Channel.GetModelEntries`. Produces a `request_model -> entry` mapping that
/// unifies:
/// 1. `supported_models` as `"direct"` entries,
/// 2. `extra_model_prefix`-prefixed aliases as `"prefix"` entries,
/// 3. `auto_trimed_model_prefixes`-derived trimmed aliases as `"auto_trim"`,
/// 4. explicit `model_mappings` as `"mapping"` entries,
///
/// then applies `hide_original_models` / `hide_mapped_models` suppression and
/// optional `lowercase_model_id` key normalization.
///
/// `supported_models` is taken from [`SupportedModelSet::merged_models`] so the
/// entry map reflects the same auto-sync merge as the public model list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelModelEntryMap {
    entries: BTreeMap<String, ChannelModelEntry>,
}

impl ChannelModelEntryMap {
    /// Build the entry map from a channel's supported-model list and its
    /// [`ChannelSettings`]. Mirrors Go `Channel.GetModelEntries` exactly,
    /// including source-priority tie-breaking on lowercase collisions.
    pub fn from_channel(supported_models: &[String], settings: &ChannelSettings) -> Self {
        let mut entries: BTreeMap<String, ChannelModelEntry> = BTreeMap::new();

        // 1. Direct models.
        for model in supported_models {
            entries
                .entry(model.clone())
                .or_insert_with(|| ChannelModelEntry {
                    request_model: model.clone(),
                    actual_model: model.clone(),
                    source: ModelSource::Direct,
                });
        }

        // 2. Prefixed models (extra_model_prefix). The alias is
        //    `<prefix>/<model>` and resolves to the bare provider model.
        if !settings.extra_model_prefix.is_empty() {
            let prefix = &settings.extra_model_prefix;
            for model in supported_models {
                let prefixed = format!("{prefix}/{model}");
                entries
                    .entry(prefixed.clone())
                    .or_insert_with(|| ChannelModelEntry {
                        request_model: prefixed,
                        actual_model: model.clone(),
                        source: ModelSource::Prefix,
                    });
            }
        }

        // 3. Auto-trimmed models (auto_trimed_model_prefixes). A supported
        //    model that starts with `<prefix>/` also becomes addressable by its
        //    trimmed tail.
        for prefix in &settings.auto_trimed_model_prefixes {
            if prefix.is_empty() {
                continue;
            }
            let needle = format!("{prefix}/");
            for model in supported_models {
                if let Some(trimmed) = model.strip_prefix(&needle) {
                    entries
                        .entry(trimmed.to_string())
                        .or_insert_with(|| ChannelModelEntry {
                            request_model: trimmed.to_string(),
                            actual_model: model.clone(),
                            source: ModelSource::AutoTrim,
                        });
                }
            }
        }

        // 4. Explicit model mappings. Only mappings whose target is in the
        //    supported list are admitted (Go `slices.Contains`).
        for mapping in &settings.model_mappings {
            if !supported_models.contains(&mapping.to) {
                continue;
            }
            entries
                .entry(mapping.from.clone())
                .or_insert_with(|| ChannelModelEntry {
                    request_model: mapping.from.clone(),
                    actual_model: mapping.to.clone(),
                    source: ModelSource::Mapping,
                });

            // hide_mapped_models removes any non-mapping entry whose
            // actual_model equals this mapping's target (covers direct /
            // prefix / auto_trim aliases of the same underlying model).
            if settings.hide_mapped_models {
                let target = mapping.to.clone();
                entries.retain(|_, entry| {
                    entry.source == ModelSource::Mapping || entry.actual_model != target
                });
            }
        }

        // 5. hide_original_models removes the bare "direct" entries.
        if settings.hide_original_models {
            entries.retain(|_, entry| entry.source != ModelSource::Direct);
        }

        // 6. Optional lowercase normalization of the request keys. Collisions
        //    are broken by source priority (direct > auto_trim > mapping >
        //    prefix > other).
        if settings.lowercase_model_id {
            let mut lowercased: BTreeMap<String, ChannelModelEntry> = BTreeMap::new();
            for (key, mut entry) in entries {
                let lower_key = key.to_ascii_lowercase();
                entry.request_model = lower_key.clone();
                lowercased
                    .entry(lower_key)
                    .and_modify(|existing| {
                        if entry.source.priority() > existing.source.priority() {
                            *existing = entry.clone();
                        }
                    })
                    .or_insert(entry);
            }
            entries = lowercased;
        }

        Self { entries }
    }

    pub fn get(&self, request_model: &str) -> Option<&ChannelModelEntry> {
        self.entries.get(request_model)
    }

    /// Iterate over `(request_model, entry)` pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ChannelModelEntry)> {
        self.entries.iter()
    }

    /// Make one canonical/public request model resolve to the concrete model
    /// accepted by this channel deployment. Commercial channel-model offers
    /// use this at request time so their `upstream_model_id` is authoritative
    /// instead of requiring a second, duplicate channel mapping.
    pub fn insert_offer_mapping(
        &mut self,
        request_model: impl Into<String>,
        actual_model: impl Into<String>,
    ) {
        let request_model = request_model.into();
        self.entries.insert(
            request_model.clone(),
            ChannelModelEntry {
                request_model,
                actual_model: actual_model.into(),
                source: ModelSource::Mapping,
            },
        );
    }

    /// Insert an entry directly, bypassing the build pipeline. Intended for
    /// tests that need a specific entry layout without reconstructing the full
    /// supported-model / settings inputs.
    pub fn insert_for_test(&mut self, entry: ChannelModelEntry) {
        self.entries.insert(entry.request_model.clone(), entry);
    }

    pub fn request_models(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn into_entries(self) -> BTreeMap<String, ChannelModelEntry> {
        self.entries
    }
}

/// Order-preserving dedup helper used by [`SupportedModelSet::merged_models`].
/// Mirrors Go's `slices.Contains`-guarded append in
/// `syncChannelModelsForChannel`.
fn push_unique_model(values: &mut Vec<String>, model: &str) {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return;
    }
    if !values.iter().any(|existing| existing == trimmed) {
        values.push(trimmed.to_string());
    }
}
