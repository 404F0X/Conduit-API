//! Prompt settings objects ported from `conduit/internal/objects/prompt.go`.
//!
//! Covers the full Go file: the [`PromptAction`] / [`PromptActivationCondition`]
//! / [`PromptActivationConditionComposite`] / [`PromptSettings`] structs and the
//! string-discriminator constants for `PromptActionType` and
//! `PromptActivationConditionType`.
//!
//! Pointer fields are mirrored as `Option<T>` (with `skip_serializing_if` to
//! honour Go `omitempty`), and the `type` discriminators are kept as `String`
//! (see [`action_type`] and [`activation_condition_type`] for the well-known
//! values) so unknown future variants round-trip without a deserialization
//! error, matching the Go comment "It is continue to add more ... types".

use serde::{Deserialize, Serialize};

/// Well-known values for [`PromptAction::kind`] (JSON `"type"`), ported from
/// the `PromptActionType*` constants in
/// `conduit/internal/objects/prompt.go`.
pub mod action_type {
    /// Prepend the prompt before the request messages.
    pub const PREPEND: &str = "prepend";
    /// Append the prompt after the request messages.
    pub const APPEND: &str = "append";
}

/// Well-known values for [`PromptActivationCondition::kind`] (JSON `"type"`),
/// ported from the `PromptActivationConditionType*` constants in
/// `conduit/internal/objects/prompt.go`.
pub mod activation_condition_type {
    /// Activate the prompt for the specified model ID.
    pub const MODEL_ID: &str = "model_id";
    /// Activate the prompt for the models that match the pattern.
    pub const MODEL_PATTERN: &str = "model_pattern";
    /// Activate the prompt for the specified API key.
    pub const API_KEY: &str = "api_key";
}

/// Action to perform when the prompt is activated. Ported 1:1 from Go
/// `PromptAction`.
///
/// The `type` discriminator selects the action; see [`action_type`] for the
/// known variants. It is stored as a `String` (rather than an enum) so future
/// action types round-trip without a deserialization error, matching the Go
/// comment that more action types may be added.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PromptAction {
    /// Type of prompt action. Mirrors Go field `Type` (JSON `"type"`).
    #[serde(default, rename = "type")]
    pub kind: String,
}

/// Condition to activate the prompt. Ported 1:1 from Go
/// `PromptActivationCondition`.
///
/// The `type` discriminator selects which of the optional payloads is
/// meaningful; see [`activation_condition_type`] for the known variants. As in
/// Go, all payload fields are pointers and omitted from JSON when absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PromptActivationCondition {
    /// Type of prompt activation condition. Mirrors Go field `Type`
    /// (JSON `"type"`).
    #[serde(default, rename = "type")]
    pub kind: String,
    /// ID of the model to activate the prompt. Mirrors Go field `ModelID`
    /// (JSON `"model_id"`, `omitempty`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Regular-expression pattern of the model name to activate the prompt.
    /// Mirrors Go field `ModelPattern` (JSON `"model_pattern"`, `omitempty`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_pattern: Option<String>,
    /// ID of the API key to activate the prompt. Mirrors Go field `APIKeyID`
    /// (JSON `"api_key_id"`, `omitempty`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_id: Option<i64>,
}

/// Composite condition to activate the prompt. Ported 1:1 from Go
/// `PromptActivationConditionComposite`.
///
/// At least one contained [`PromptActivationCondition`] must be met to
/// activate the prompt (OR semantics within a composite).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PromptActivationConditionComposite {
    /// Conditions to activate the prompt. Mirrors Go field `Conditions`
    /// (JSON `"conditions"`, `omitempty`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<PromptActivationCondition>,
}

/// Top-level prompt settings. Ported 1:1 from Go `PromptSettings`.
///
/// `action` is always present (no `omitempty` in Go), while `conditions` is a
/// list of composites — every composite must be satisfied (AND semantics across
/// composites) for the prompt to activate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromptSettings {
    /// Action to perform when the prompt is activated. Mirrors Go field
    /// `Action` (JSON `"action"`).
    #[serde(default)]
    pub action: PromptAction,
    /// Composite conditions, all of which must be met to activate the prompt.
    /// Mirrors Go field `Conditions` (JSON `"conditions"`, `omitempty`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<PromptActivationConditionComposite>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn action_type_constants_match_go() {
        assert_eq!(action_type::PREPEND, "prepend");
        assert_eq!(action_type::APPEND, "append");
    }

    #[test]
    fn activation_condition_type_constants_match_go() {
        assert_eq!(activation_condition_type::MODEL_ID, "model_id");
        assert_eq!(activation_condition_type::MODEL_PATTERN, "model_pattern");
        assert_eq!(activation_condition_type::API_KEY, "api_key");
    }

    #[test]
    fn prompt_action_round_trip() -> Result<(), serde_json::Error> {
        let input = r#"{"type":"prepend"}"#;
        let action: PromptAction = serde_json::from_str(input)?;
        assert_eq!(action.kind, action_type::PREPEND);

        let re = serde_json::to_value(&action)?;
        assert_eq!(re.get("type").and_then(Value::as_str), Some("prepend"));
        Ok(())
    }

    #[test]
    fn activation_condition_round_trip_with_payloads() -> Result<(), serde_json::Error> {
        let input = r#"{"type":"model_id","model_id":"gpt-4o"}"#;
        let cond: PromptActivationCondition = serde_json::from_str(input)?;
        assert_eq!(cond.kind, activation_condition_type::MODEL_ID);
        assert_eq!(cond.model_id.as_deref(), Some("gpt-4o"));
        assert!(cond.model_pattern.is_none());
        assert!(cond.api_key_id.is_none());

        let re = serde_json::to_value(&cond)?;
        assert_eq!(re.get("type").and_then(Value::as_str), Some("model_id"));
        assert_eq!(re.get("model_id").and_then(Value::as_str), Some("gpt-4o"));
        // omitempty: absent optional fields must not be serialized.
        assert!(re.get("model_pattern").is_none());
        assert!(re.get("api_key_id").is_none());
        Ok(())
    }

    #[test]
    fn activation_condition_api_key_branch_round_trip() -> Result<(), serde_json::Error> {
        let input = r#"{"type":"api_key","api_key_id":42}"#;
        let cond: PromptActivationCondition = serde_json::from_str(input)?;
        assert_eq!(cond.kind, activation_condition_type::API_KEY);
        assert_eq!(cond.api_key_id, Some(42));
        Ok(())
    }

    #[test]
    fn prompt_settings_round_trip() -> Result<(), serde_json::Error> {
        let input = r#"{"action":{"type":"append"},"conditions":[{"conditions":[{"type":"model_pattern","model_pattern":"gpt-.*"}]}]}"#;
        let settings: PromptSettings = serde_json::from_str(input)?;
        assert_eq!(settings.action.kind, action_type::APPEND);
        assert_eq!(settings.conditions.len(), 1);
        let inner = &settings.conditions[0].conditions;
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].kind, activation_condition_type::MODEL_PATTERN);
        assert_eq!(inner[0].model_pattern.as_deref(), Some("gpt-.*"));

        let re = serde_json::to_value(&settings)?;
        assert_eq!(
            re.get("action")
                .and_then(|v| v.get("type"))
                .and_then(Value::as_str),
            Some("append")
        );
        let comp = re
            .get("conditions")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first());
        let inner_cond = comp
            .and_then(|c| c.get("conditions"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first());
        assert_eq!(
            inner_cond
                .and_then(|c| c.get("type"))
                .and_then(Value::as_str),
            Some("model_pattern")
        );
        assert_eq!(
            inner_cond
                .and_then(|c| c.get("model_pattern"))
                .and_then(Value::as_str),
            Some("gpt-.*")
        );
        Ok(())
    }

    #[test]
    fn empty_prompt_settings_omits_conditions() -> Result<(), serde_json::Error> {
        let settings = PromptSettings::default();
        let re = serde_json::to_value(&settings)?;
        // action is always present (no omitempty); conditions is omitted when empty.
        assert!(re.get("action").is_some());
        assert!(re.get("conditions").is_none());
        Ok(())
    }

    #[test]
    fn unknown_action_type_round_trips() -> Result<(), serde_json::Error> {
        // Future / unknown action types must survive a round trip.
        let input = json!({"type": "wrap"});
        let action: PromptAction = serde_json::from_value(input.clone())?;
        assert_eq!(action.kind, "wrap");
        let re = serde_json::to_value(&action)?;
        assert_eq!(re, input);
        Ok(())
    }
}
