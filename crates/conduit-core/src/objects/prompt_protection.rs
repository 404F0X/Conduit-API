//! Prompt-protection settings, ported 1:1 from
//! `conduit/internal/objects/prompt_protection.go`.
//!
//! The Go source defines two string-enum newtypes ([`PromptProtectionAction`]
//! and [`PromptProtectionScope`]) plus the [`PromptProtectionSettings`] struct
//! that selects the masking/rejection behavior applied to a chat message.
//! All values here mirror the Go constants and JSON tags exactly.

use serde::{Deserialize, Serialize};

/// Action taken when prompt protection triggers. Ported 1:1 from the Go
/// `PromptProtectionAction` string newtype and its `mask` / `reject`
/// constants.
pub type PromptProtectionAction = String;

/// `PromptProtectionAction = "mask"`.
pub const PROMPT_PROTECTION_ACTION_MASK: &str = "mask";
/// `PromptProtectionAction = "reject"`.
pub const PROMPT_PROTECTION_ACTION_REJECT: &str = "reject";

/// Scope to which prompt protection applies. Ported 1:1 from the Go
/// `PromptProtectionScope` string newtype and its system / developer / user
/// / assistant / tool constants.
pub type PromptProtectionScope = String;

/// `PromptProtectionScope = "system"`.
pub const PROMPT_PROTECTION_SCOPE_SYSTEM: &str = "system";
/// `PromptProtectionScope = "developer"`.
pub const PROMPT_PROTECTION_SCOPE_DEVELOPER: &str = "developer";
/// `PromptProtectionScope = "user"`.
pub const PROMPT_PROTECTION_SCOPE_USER: &str = "user";
/// `PromptProtectionScope = "assistant"`.
pub const PROMPT_PROTECTION_SCOPE_ASSISTANT: &str = "assistant";
/// `PromptProtectionScope = "tool"`.
pub const PROMPT_PROTECTION_SCOPE_TOOL: &str = "tool";

/// Settings for prompt protection. Ported 1:1 from Go
/// `PromptProtectionSettings`. `action` is always present; `replacement` and
/// `scopes` are omitted from the JSON output when empty (matching the Go
/// `omitempty` tags).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromptProtectionSettings {
    /// Action to take on a flagged message (`mask` or `reject`).
    #[serde(default)]
    pub action: PromptProtectionAction,
    /// Replacement text used when `action == "mask"`. Mirrors Go
    /// `replacement,omitempty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<String>,
    /// Scopes the action applies to. Mirrors Go `scopes,omitempty`. Because
    /// Go `omitempty` drops a nil/empty slice, an empty `Vec` is also skipped
    /// on serialize.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<PromptProtectionScope>,
}

// Go declares `replacement string` with `omitempty`, which in Go means the
// field is *elided* when it holds the zero value `""`. To match that
// semantics on the wire we model it as `Option<String>` (None -> omitted)
// rather than `String` with `skip_serializing_if = "String::is_empty"`. Both
// approaches produce identical JSON for the `""` case; `Option<String>` keeps
// the distinction between "explicitly cleared" and "never set" faithful to
// the Go pointer-equivalent usage elsewhere in the package.

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_str, to_value};

    #[test]
    fn round_trip_reject_settings() -> Result<(), serde_json::Error> {
        let json = r#"{"action":"reject"}"#;
        let settings: PromptProtectionSettings = from_str(json)?;
        assert_eq!(settings.action, PROMPT_PROTECTION_ACTION_REJECT);
        assert_eq!(settings.replacement, None);
        assert!(settings.scopes.is_empty());

        let value = to_value(&settings)?;
        // Omitempty fields are absent.
        assert_eq!(value.get("action").and_then(|v| v.as_str()), Some("reject"));
        assert!(value.get("replacement").is_none());
        assert!(value.get("scopes").is_none());

        let re: PromptProtectionSettings = from_value_replica(value)?;
        assert_eq!(re, settings);
        Ok(())
    }

    #[test]
    fn round_trip_mask_with_replacement_and_scopes() -> Result<(), serde_json::Error> {
        let json = r#"{"action":"mask","replacement":"[redacted]","scopes":["system","user"]}"#;
        let settings: PromptProtectionSettings = from_str(json)?;
        assert_eq!(settings.action, PROMPT_PROTECTION_ACTION_MASK);
        assert_eq!(settings.replacement.as_deref(), Some("[redacted]"));
        assert_eq!(
            settings.scopes,
            vec![
                PROMPT_PROTECTION_SCOPE_SYSTEM.to_string(),
                PROMPT_PROTECTION_SCOPE_USER.to_string(),
            ]
        );

        let value = to_value(&settings)?;
        assert_eq!(value.get("action").and_then(|v| v.as_str()), Some("mask"));
        assert_eq!(
            value.get("replacement").and_then(|v| v.as_str()),
            Some("[redacted]")
        );
        let scopes = value.get("scopes").and_then(|v| v.as_array());
        assert!(scopes.is_some());
        assert_eq!(scopes.map(|a| a.len()), Some(2));

        let re: PromptProtectionSettings = from_value_replica(value)?;
        assert_eq!(re, settings);
        Ok(())
    }

    #[test]
    fn default_omits_optional_fields() -> Result<(), serde_json::Error> {
        let settings = PromptProtectionSettings::default();
        assert_eq!(settings.action, "");
        let value = to_value(&settings)?;
        // Go would emit `"action":""` because action lacks omitempty; the two
        // optional fields are elided.
        assert_eq!(value.get("action").and_then(|v| v.as_str()), Some(""));
        assert!(value.get("replacement").is_none());
        assert!(value.get("scopes").is_none());
        Ok(())
    }

    // Helper that avoids `.unwrap()` on the serde_json::Value -> struct path.
    fn from_value_replica(
        value: serde_json::Value,
    ) -> Result<PromptProtectionSettings, serde_json::Error> {
        serde_json::from_value(value)
    }
}
