//! Model card and settings structs ported from `conduit/internal/objects/model.go`
//! (association structs live in `model_association.rs`).
//!
//! Covers the model-card surface: [`ModelCardReasoning`], [`ModelCardModalities`],
//! [`ModelCardCost`], [`ModelCardLimit`], [`ModelCard`], and [`ModelSettings`].
//! The association graph reachable from `ModelSettings.associations` is ported
//! in [`crate::objects::model_association`] and referenced here by full path.
//!
//! Field names, JSON tags, and zero-fill semantics mirror the Go source 1:1;
//! none of the ported Go fields carry `omitempty`, so each field gets
//! `#[serde(default)]` rather than `skip_serializing_if`.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Reasoning capability flags for a model card. Ported 1:1 from Go
/// `ModelCardReasoning`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelCardReasoning {
    #[serde(default)]
    pub supported: bool,
    #[serde(default)]
    pub default: bool,
}

/// Supported input/output modalities for a model card (e.g. `"text"`,
/// `"image"`, `"video"`). Ported 1:1 from Go `ModelCardModalities`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelCardModalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

/// Per-token cost components for a model card. Ported 1:1 from Go
/// `ModelCardCost`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelCardCost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}

/// Context/output token limits for a model card. Ported 1:1 from Go
/// `ModelCardLimit`; the Go `int` fields map to [`i64`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelCardLimit {
    #[serde(default)]
    pub context: i64,
    #[serde(default)]
    pub output: i64,
}

/// Static descriptive metadata for a model. Ported 1:1 from Go `ModelCard`;
/// every field is zero-filled in Go, so each carries `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelCard {
    #[serde(default)]
    pub reasoning: ModelCardReasoning,
    #[serde(default)]
    pub tool_call: bool,
    #[serde(default)]
    pub temperature: bool,
    #[serde(default)]
    pub modalities: ModelCardModalities,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub cost: ModelCardCost,
    #[serde(default)]
    pub limit: ModelCardLimit,
    #[serde(default)]
    pub knowledge: String,
    #[serde(default)]
    pub release_date: String,
    #[serde(default)]
    pub last_updated: String,
}

/// Top-level model configuration entry. Ported 1:1 from Go `ModelSettings`;
/// `associations` mirrors `[]*ModelAssociation` and references the type ported
/// in [`crate::objects::model_association`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelSettings {
    #[serde(default)]
    pub disable_developer_settings_inheritance: bool,
    #[serde(default)]
    pub associations: Vec<crate::objects::ModelAssociation>,
}

/// Developer-scoped model settings entry. Ported 1:1 from Go
/// `biz.DeveloperModelSettings` (`system.go` lines 427-430): a developer name
/// plus the shared associations inherited by every model from that developer
/// (unless inheritance is disabled on the model).
///
/// `developer` is matched case-sensitively against `Model.developer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DeveloperModelSettings {
    /// Developer identifier (e.g. `"openai"`, `"anthropic"`). Trimmed when the
    /// system settings are normalized.
    #[serde(default)]
    pub developer: String,
    /// Shared channel-selection rules inherited by sibling models.
    #[serde(default)]
    pub associations: Vec<crate::objects::ModelAssociation>,
}

/// System-wide model configuration. Ported 1:1 from Go
/// `biz.SystemModelSettings` (`system.go` lines 388-425).
///
/// The fields below control model-list behavior, blacklist filtering, and the
/// per-developer association defaults that [`ModelSettings`] can inherit. The
/// Go default instance lives in `system_default.go` (`defaultModelSettings`);
/// the Rust counterpart is [`SystemModelSettings::default`], which matches it
/// (see the per-field notes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SystemModelSettings {
    /// Fall back to legacy channel selection when the requested model has no
    /// associations. Go default: `true`.
    #[serde(default = "default_true")]
    pub fallback_to_channels_on_model_not_found: bool,
    /// Return all channel-supported models from the models API, not only
    /// configured `Model` entities. Go default: `true`.
    #[serde(default = "default_true")]
    pub query_all_channel_models: bool,
    /// Make `GET /v1/models` behave like `?include=all` by default. Go default:
    /// `false`.
    #[serde(default)]
    pub default_model_api_include_all: bool,
    /// Normalize model names carrying a reasoning-effort suffix. Go default:
    /// `false`.
    #[serde(default)]
    pub auto_reasoning_effort: bool,
    /// Regex excluding channel-derived model ids from the models API. Empty
    /// disables the filter. Go default: `""`.
    #[serde(default)]
    pub model_blacklist_regex: String,
    /// Per-developer inherited associations. Go default: empty slice.
    #[serde(default)]
    pub developer_settings: Vec<DeveloperModelSettings>,
}

impl SystemModelSettings {
    /// Returns the associations configured for `developer`, or an empty slice.
    ///
    /// Mirrors Go `developerAssociationsForModel` (`model_settings_inheritance.go`
    /// lines 109-123): case-sensitive match on the developer name; an empty
    /// developer name matches nothing.
    pub fn associations_for_developer(
        &self,
        developer: &str,
    ) -> &[crate::objects::ModelAssociation] {
        if developer.is_empty() {
            return &[];
        }
        self.developer_settings
            .iter()
            .find(|d| d.developer == developer)
            .map(|d| d.associations.as_slice())
            .unwrap_or(&[])
    }
}

fn default_true() -> bool {
    true
}

/// Hand-written to match Go `defaultModelSettings` (`system_default.go` lines
/// 33-40): both toggles default to `true`, the rest to `false`/empty.
impl Default for SystemModelSettings {
    fn default() -> Self {
        Self {
            fallback_to_channels_on_model_not_found: true,
            query_all_channel_models: true,
            default_model_api_include_all: false,
            auto_reasoning_effort: false,
            model_blacklist_regex: String::new(),
            developer_settings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::ModelAssociation;
    use serde_json::json;

    #[test]
    fn model_card_round_trip() -> Result<(), serde_json::Error> {
        let input = json!({
            "reasoning": {"supported": true, "default": false},
            "toolCall": true,
            "temperature": false,
            "modalities": {"input": ["text", "image"], "output": ["text"]},
            "vision": true,
            "cost": {"input": 0.01, "output": 0.03, "cacheRead": 0.001, "cacheWrite": 0.002},
            "limit": {"context": 128000, "output": 16384},
            "knowledge": "2024-01",
            "releaseDate": "2024-03-14",
            "lastUpdated": "2024-06-01"
        });
        let card: ModelCard = serde_json::from_value(input.clone())?;
        assert!(card.reasoning.supported);
        assert!(card.tool_call);
        assert_eq!(
            card.modalities.input,
            vec!["text".to_string(), "image".to_string()]
        );
        assert_eq!(card.limit.context, 128000);
        assert_eq!(card.cost.cache_read, 0.001);

        let re = serde_json::to_value(&card)?;
        assert_eq!(re, input);
        Ok(())
    }

    #[test]
    fn model_settings_round_trip_with_association() -> Result<(), serde_json::Error> {
        let input = json!({
            "disableDeveloperSettingsInheritance": true,
            "associations": [
                {
                    "type": "channel_model",
                    "priority": 2,
                    "disabled": false,
                    "channelModel": {"channelId": 7, "modelId": "gpt-4o"}
                }
            ]
        });
        let settings: ModelSettings = serde_json::from_value(input.clone())?;
        assert!(settings.disable_developer_settings_inheritance);
        assert_eq!(settings.associations.len(), 1);
        let assoc: &ModelAssociation = &settings.associations[0];
        assert_eq!(assoc.kind, "channel_model");
        assert_eq!(assoc.priority, 2);
        match &assoc.channel_model {
            Some(cm) => {
                assert_eq!(cm.channel_id, 7);
                assert_eq!(cm.model_id, "gpt-4o");
            }
            None => panic!("expected a channelModel branch"),
        }

        let re = serde_json::to_value(&settings)?;
        assert_eq!(re, input);
        Ok(())
    }
}
