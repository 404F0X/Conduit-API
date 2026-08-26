//! Model association objects ported from `conduit/internal/objects/model.go`.
//!
//! Covers the association subset: [`ModelAssociation`], its typed branches,
//! [`ModelAssociationWhen`] (which wraps [`Condition`]), [`ExcludeAssociation`],
//! and the condition field-name constants. The `ModelCard*` / `ModelSettings`
//! structs remain pending under `OBJ-06`.
//!
//! Pointer fields are mirrored as `Option<T>` and `[]int` as `Vec<i64>`
//! (matching the `i64` id convention used elsewhere in the workspace).

use crate::objects::Condition;
use serde::{Deserialize, Serialize};

/// JSON field names supplied to [`Condition`] data by the model matcher, ported
/// from the `ModelAssociationConditionField*` constants in
/// `conduit/internal/objects/model.go`.
pub mod condition_field {
    pub const PROMPT_TOKENS: &str = "prompt_tokens";
    pub const STREAM: &str = "stream";
    pub const REQUEST_FORMAT: &str = "request_format";
    pub const DAILY_TIME: &str = "daily_time";
    pub const HAS_IMAGE: &str = "has_image";
    pub const HAS_VIDEO: &str = "has_video";
    pub const HAS_DOCUMENT: &str = "has_document";
    pub const HAS_AUDIO: &str = "has_audio";
}

/// When-clause for a model association. Ported 1:1 from Go
/// `ModelAssociationWhen`; `condition` is omitted when absent (`omitempty`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelAssociationWhen {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
}

/// Exclude rule for regex / model-id associations. Ported 1:1 from Go
/// `ExcludeAssociation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExcludeAssociation {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel_name_pattern: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_ids: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_tags: Vec<String>,
}

/// `channel_model` branch. Ported 1:1 from Go `ChannelModelAssociation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelModelAssociation {
    pub channel_id: i64,
    pub model_id: String,
}

/// `channel_regex` branch. Ported 1:1 from Go `ChannelRegexAssociation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelRegexAssociation {
    pub channel_id: i64,
    pub pattern: String,
}

/// `regex` branch. Ported 1:1 from Go `RegexAssociation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RegexAssociation {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pattern: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<ExcludeAssociation>,
}

/// `model` branch. Ported 1:1 from Go `ModelIDAssociation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelIDAssociation {
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<ExcludeAssociation>,
}

/// `channel_tags_model` branch. Ported 1:1 from Go `ChannelTagsModelAssociation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelTagsModelAssociation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model_id: String,
}

/// `channel_tags_regex` branch. Ported 1:1 from Go `ChannelTagsRegexAssociation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChannelTagsRegexAssociation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pattern: String,
}

/// A model association entry. Ported 1:1 from Go `ModelAssociation`; the
/// discriminator `type` selects which branch is populated.
///
/// Branch types: `channel_model`, `channel_regex`, `regex`, `model`,
/// `channel_tags_model`, `channel_tags_regex`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelAssociation {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<ModelAssociationWhen>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_model: Option<ChannelModelAssociation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_regex: Option<ChannelRegexAssociation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<RegexAssociation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<ModelIDAssociation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_tags_model: Option<ChannelTagsModelAssociation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_tags_regex: Option<ChannelTagsRegexAssociation>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::evaluate;
    use serde_json::json;

    #[test]
    fn condition_field_constants_match_go() {
        assert_eq!(condition_field::PROMPT_TOKENS, "prompt_tokens");
        assert_eq!(condition_field::STREAM, "stream");
        assert_eq!(condition_field::REQUEST_FORMAT, "request_format");
        assert_eq!(condition_field::DAILY_TIME, "daily_time");
        assert_eq!(condition_field::HAS_IMAGE, "has_image");
        assert_eq!(condition_field::HAS_VIDEO, "has_video");
        assert_eq!(condition_field::HAS_DOCUMENT, "has_document");
        assert_eq!(condition_field::HAS_AUDIO, "has_audio");
    }

    #[test]
    fn when_wraps_a_usable_condition() -> Result<(), serde_json::Error> {
        let json_str = r#"{"enabled":true,"condition":{"logic":"and","conditions":[{"field":"prompt_tokens","operator":"gt","value":100}]}}"#;
        let when: ModelAssociationWhen = serde_json::from_str(json_str)?;
        assert!(when.enabled);
        match &when.condition {
            Some(cond) => assert!(evaluate(cond, &json!({"prompt_tokens": 101}))),
            None => panic!("expected a wrapped condition"),
        }
        Ok(())
    }

    #[test]
    fn channel_model_association_round_trip() -> Result<(), serde_json::Error> {
        let input = r#"{"type":"channel_model","priority":1,"disabled":false,"channelModel":{"channelId":7,"modelId":"gpt-4o"}}"#;
        let assoc: ModelAssociation = serde_json::from_str(input)?;
        assert_eq!(assoc.kind, "channel_model");
        assert_eq!(assoc.priority, 1);
        assert!(assoc.channel_model.is_some());

        let re = serde_json::to_value(&assoc)?;
        assert_eq!(
            re.get("type").and_then(|v| v.as_str()),
            Some("channel_model")
        );
        assert_eq!(
            re.get("channelModel")
                .and_then(|v| v.get("channelId"))
                .and_then(|v| v.as_i64()),
            Some(7)
        );
        Ok(())
    }

    #[test]
    fn regex_association_with_exclude_round_trip() -> Result<(), serde_json::Error> {
        let input = r#"{"type":"regex","priority":0,"disabled":false,"regex":{"pattern":"gpt-.*","exclude":[{"channelNamePattern":"legacy-*","channelIds":[3,4],"channelTags":["beta"]}]}}"#;
        let assoc: ModelAssociation = serde_json::from_str(input)?;
        let regex = match &assoc.regex {
            Some(r) => r,
            None => panic!("expected a regex branch"),
        };
        assert_eq!(regex.pattern, "gpt-.*");
        assert_eq!(regex.exclude.len(), 1);
        assert_eq!(regex.exclude[0].channel_ids, vec![3, 4]);
        assert_eq!(regex.exclude[0].channel_tags, vec!["beta".to_string()]);
        Ok(())
    }
}
