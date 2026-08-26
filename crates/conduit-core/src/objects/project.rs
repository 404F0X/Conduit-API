//! Project routing profiles, ported 1:1 from
//! `conduit/internal/objects/project.go`.
//!
//! `ProjectProfile::match_channel_tags` delegates to
//! [`apikey::match_channel_tags`](crate::objects::apikey::match_channel_tags),
//! the port of Go's free function `MatchChannelTags`.

use serde::{Deserialize, Serialize};

/// A project's named routing profile collection.
///
/// Mirrors Go `ProjectProfiles`. `activeProfile` selects which entry of
/// `profiles` is currently active.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProfiles {
    /// Name of the currently active profile entry.
    #[serde(default)]
    pub active_profile: String,
    /// Available profiles. Go zero-fills (empty slice) when absent.
    #[serde(default)]
    pub profiles: Vec<ProjectProfile>,
}

/// A single named channel-selection profile within a project.
///
/// Mirrors Go `ProjectProfile`. All selector fields are optional/omitempty on
/// the Go side: an empty profile matches every channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProfile {
    /// Profile name (unique within the parent [`ProjectProfiles`]).
    #[serde(default)]
    pub name: String,
    /// Explicit allow-list of channel IDs.
    #[serde(default, rename = "channelIDs", skip_serializing_if = "Vec::is_empty")]
    pub channel_ids: Vec<i64>,
    /// Tags a channel must carry to be selectable by this profile.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_tags: Vec<String>,
    /// How `channel_tags` is combined (`any` | `all` | `none` | `""`). Ported
    /// typed newtype [`ChannelTagsMatchMode`](crate::objects::apikey::ChannelTagsMatchMode).
    #[serde(
        default,
        rename = "channelTagsMatchMode",
        skip_serializing_if = "Option::is_none"
    )]
    pub channel_tags_match_mode: Option<crate::objects::apikey::ChannelTagsMatchMode>,
}

impl ProjectProfile {
    /// Returns `true` when this profile selects a channel carrying `tags`.
    ///
    /// Mirrors Go `(*ProjectProfile).MatchChannelTags`: empty `channel_tags`
    /// matches everything, otherwise delegates to
    /// [`apikey::match_channel_tags`](crate::objects::apikey::match_channel_tags).
    pub fn match_channel_tags(&self, tags: &[String]) -> bool {
        if self.channel_tags.is_empty() {
            return true;
        }
        let mode: &str = self.channel_tags_match_mode.as_deref().unwrap_or_default();
        crate::objects::apikey::match_channel_tags(&self.channel_tags, mode, tags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_project_profiles() -> Result<(), serde_json::Error> {
        let payload = json!({
            "activeProfile": "default",
            "profiles": [
                {
                    "name": "default",
                    "channelIDs": [1, 2, 3],
                    "channelTags": ["gpu", "cheap"],
                    "channelTagsMatchMode": "all"
                },
                {
                    "name": "fallback"
                }
            ]
        });

        let parsed: ProjectProfiles = serde_json::from_str(&payload.to_string())?;
        assert_eq!(parsed.active_profile, "default");
        assert_eq!(parsed.profiles.len(), 2);

        let first = &parsed.profiles[0];
        assert_eq!(first.name, "default");
        assert_eq!(first.channel_ids, vec![1_i64, 2, 3]);
        assert_eq!(
            first.channel_tags,
            vec!["gpu".to_string(), "cheap".to_string()]
        );
        match &first.channel_tags_match_mode {
            Some(mode) => assert_eq!(mode, "all"),
            None => panic!("expected mode \"all\""),
        }

        let second = &parsed.profiles[1];
        assert_eq!(second.name, "fallback");
        assert!(second.channel_ids.is_empty());
        assert!(second.channel_tags.is_empty());
        assert!(second.channel_tags_match_mode.is_none());

        // Re-serialize and assert key shape (omitempty must drop empty fields).
        let out = serde_json::to_value(&parsed)?;
        let first_out = out
            .get("profiles")
            .and_then(|v| v.get(0))
            .ok_or_else(|| serde::de::Error::custom("missing profiles[0]"))?;
        assert_eq!(first_out.get("name"), Some(&json!("default")));
        assert_eq!(first_out.get("channelIDs"), Some(&json!([1, 2, 3])));
        assert_eq!(first_out.get("channelTagsMatchMode"), Some(&json!("all")));

        let second_out = out
            .get("profiles")
            .and_then(|v| v.get(1))
            .ok_or_else(|| serde::de::Error::custom("missing profiles[1]"))?;
        // omitempty fields absent on the empty profile.
        assert!(second_out.get("channelIDs").is_none());
        assert!(second_out.get("channelTags").is_none());
        assert!(second_out.get("channelTagsMatchMode").is_none());
        Ok(())
    }

    #[test]
    fn round_trip_empty_defaults() -> Result<(), serde_json::Error> {
        // Empty JSON object decodes to all-default ProjectProfiles.
        let parsed: ProjectProfiles = serde_json::from_str("{}")?;
        assert_eq!(parsed.active_profile, "");
        assert!(parsed.profiles.is_empty());

        // Re-serializing an empty profile yields no selector keys.
        let empty = ProjectProfile::default();
        let out = serde_json::to_value(&empty)?;
        assert_eq!(out.get("name"), Some(&json!("")));
        assert!(out.get("channelIDs").is_none());
        assert!(out.get("channelTags").is_none());
        assert!(out.get("channelTagsMatchMode").is_none());
        Ok(())
    }

    #[test]
    fn match_channel_tags_delegates_to_combinator() {
        // Empty channel_tags short-circuits to true (matches Go).
        let profile = ProjectProfile::default();
        assert!(profile.match_channel_tags(&["anything".to_string()]));

        // Non-empty: delegates to apikey::match_channel_tags. "all" requires
        // every profile tag to be present in the input.
        let tagged = ProjectProfile {
            channel_tags: vec!["gpu".to_string()],
            channel_tags_match_mode: Some("all".to_string()),
            ..Default::default()
        };
        assert!(tagged.match_channel_tags(&["gpu".to_string(), "cpu".to_string()]));
        assert!(!tagged.match_channel_tags(&["cpu".to_string()]));
    }
}
