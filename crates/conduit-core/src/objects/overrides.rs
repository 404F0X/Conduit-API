//! Header / body override operations, ported from the override-related types
//! and functions in `conduit/internal/objects/channel.go`.
//!
//! This module is split from [`crate::objects::channel_settings`] because the
//! `ChannelSettings` struct references [`OverrideOperation`] in several of its
//! fields (`bodyOverrideOperations`, `headerOverrideOperations`); the split
//! keeps the dependency direction one-way (`channel_settings` -> `overrides`).
//!
//! All field names, JSON tags, and `omitempty` semantics mirror the Go source
//! exactly. Pointer fields (`*OverrideMatch`, `*int`, `*bool`) become
//! `Option<T>` with `skip_serializing_if = "Option::is_none"`.

use serde::{Deserialize, Serialize};

/// Override operation opcode constants. Ported 1:1 from the Go `OverrideOp*`
/// consts. See [`OverrideOperation::op`].
pub mod override_op {
    /// `OverrideOpSet = "set"` — set / replace the value at `path`.
    pub const SET: &str = "set";
    /// `OverrideOpDelete = "delete"` — delete the value at `path`.
    pub const DELETE: &str = "delete";
    /// `OverrideOpRename = "rename"` — rename the key `from` to `to`.
    pub const RENAME: &str = "rename";
    /// `OverrideOpCopy = "copy"` — copy the value from `from` to `to`.
    pub const COPY: &str = "copy";
    /// `OverrideOpArrayAppend = "array_append"` — append to the array at `path`.
    pub const ARRAY_APPEND: &str = "array_append";
    /// `OverrideOpArrayPrepend = "array_prepend"` — prepend to the array at `path`.
    pub const ARRAY_PREPEND: &str = "array_prepend";
    /// `OverrideOpArrayInsert = "array_insert"` — insert into the array at
    /// `path` at position `index`.
    pub const ARRAY_INSERT: &str = "array_insert";
    /// `OverrideOpArrayRemove = "array_remove"` — remove from the array at
    /// `path` the items matched by `match`.
    pub const ARRAY_REMOVE: &str = "array_remove";
}

/// A simple equality matcher for `array_remove` operations. Ported 1:1 from Go
/// `OverrideMatch`.
///
/// `path` is resolved relative to each array item, and the item is removed when
/// its value at `path` equals `eq`. Both JSON tags (`path`, `eq`) are
/// single-word, so no `rename_all` is needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OverrideMatch {
    /// Path resolved relative to each array item.
    #[serde(default)]
    pub path: String,
    /// Value that removes the item when it matches.
    #[serde(default)]
    pub eq: String,
}

/// A structured override operation for request body / header manipulation.
/// Ported 1:1 from Go `OverrideOperation`.
///
/// The meaning of the fields depends on `op`:
/// - `set` uses `path` + `value`.
/// - `delete` uses `path`.
/// - `rename` / `copy` use `from` + `to`.
/// - `array_append` / `array_prepend` use `path` + `value` (+ `splat`).
/// - `array_insert` uses `path` + `value` + `index` (+ `splat`).
/// - `array_remove` uses `path` + `match`.
///
/// `condition` is an optional expression that gates the operation. All JSON
/// tags are single words, so no `rename_all` is needed; the `r#match` Rust
/// field serializes as `match` (serde strips the `r#` raw-identifier prefix).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OverrideOperation {
    /// Operation opcode. One of the [`override_op`] constants.
    #[serde(default)]
    pub op: String,
    /// Target path. Omitted on the wire when empty (Go `omitempty`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Source key for `rename` / `copy`. Omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    /// Destination key for `rename` / `copy`. Omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to: String,
    /// Value for `set` / array ops. Omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    /// Optional gating condition expression. Omitted on the wire when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub condition: String,
    /// Matcher identifying array items removed by `array_remove`.
    /// Mirrors Go `*OverrideMatch` with `omitempty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#match: Option<OverrideMatch>,
    /// Target position for `array_insert`. Negative values count from the end
    /// (-1 = before last); out-of-range values are clamped to `[0, len]`.
    /// Mirrors Go `*int` with `omitempty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<i64>,
    /// Whether a JSON-array `value` is spread into the target array (`true`,
    /// the default) or inserted as a single nested element (`false`). Only
    /// meaningful for `array_append`, `array_prepend`, `array_insert`.
    /// Mirrors Go `*bool` with `omitempty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub splat: Option<bool>,
}

/// Convert a legacy [`crate::objects::channel_settings::HeaderEntry`] list to
/// override operations. Ported 1:1 from Go `HeaderEntriesToOverrideOperations`.
///
/// Each header becomes one [`OverrideOperation`]:
/// - value `"__CONDUIT_CLEAR__"` -> `{op: "delete", path: key}`,
/// - any other value -> `{op: "set", path: key, value}`.
///
/// Returns `None` (mirroring Go `nil`) when `headers` is empty.
pub fn header_entries_to_override_operations(
    headers: &[crate::objects::channel_settings::HeaderEntry],
) -> Option<Vec<OverrideOperation>> {
    if headers.is_empty() {
        return None;
    }

    let mut ops = Vec::with_capacity(headers.len());
    for header in headers {
        if header.value == "__CONDUIT_CLEAR__" {
            ops.push(OverrideOperation {
                op: override_op::DELETE.to_string(),
                path: header.key.clone(),
                ..Default::default()
            });
            continue;
        }

        ops.push(OverrideOperation {
            op: override_op::SET.to_string(),
            path: header.key.clone(),
            value: header.value.clone(),
            ..Default::default()
        });
    }

    Some(ops)
}

/// Parse an override-parameters string into a slice of [`OverrideOperation`]s.
/// Ported 1:1 from Go `ParseOverrideOperations`.
///
/// Supports both the new operation-array format (`JSON array` of
/// `OverrideOperation`) and the legacy `JSON object` map format, which is
/// automatically converted:
/// - empty / `"{}"` / `"[]"` -> `None`,
/// - a value `"__CONDUIT_CLEAR__"` -> `{op: "delete", path: key}`,
/// - any other value -> `{op: "set", path: key, value}` where non-string values
///   are stringified via Go's `fmt.Sprintf("%v", v)` equivalent
///   ([`serde_json::Value`] string formatting).
///
/// Map iteration order (and therefore the order of converted ops) follows Go's
/// non-deterministic `map[string]any` iteration; callers must not rely on it.
pub fn parse_override_operations(raw: &str) -> Result<Option<Vec<OverrideOperation>>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "{}" || trimmed == "[]" {
        return Ok(None);
    }

    if trimmed.as_bytes().first() == Some(&b'[') {
        let ops: Vec<OverrideOperation> = serde_json::from_str(trimmed)
            .map_err(|err| format!("invalid override operations: {err}"))?;
        return Ok(Some(ops));
    }

    // Legacy map format.
    let legacy: serde_json::Map<String, serde_json::Value> = serde_json::from_str(trimmed)
        .map_err(|err| format!("invalid override parameters: {err}"))?;

    let mut ops = Vec::with_capacity(legacy.len());
    for (key, value) in legacy {
        let str_value = stringify_legacy_value(&value);
        if str_value == "__CONDUIT_CLEAR__" {
            ops.push(OverrideOperation {
                op: override_op::DELETE.to_string(),
                path: key,
                ..Default::default()
            });
        } else {
            ops.push(OverrideOperation {
                op: override_op::SET.to_string(),
                path: key,
                value: str_value,
                ..Default::default()
            });
        }
    }

    Ok(Some(ops))
}

/// Render a [`serde_json::Value`] the way Go's `fmt.Sprintf("%v", v)` renders
/// the parsed `map[string]any` element: bare string -> unquoted, anything else
/// -> Go default formatting (which for our purposes matches `Display` of the
/// underlying type). Ported to mirror the switch in Go
/// `ParseOverrideOperations`.
fn stringify_legacy_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        // Go's `fmt.Sprintf("%v", v)` on a `bool`/`float64`/`nil` produces
        // `true`/`false`/`<noquote>` and JSON numbers render as Go floats.
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "<nil>".to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        // Objects / arrays fall back to their JSON text, matching Go's default
        // `%v` formatting for `map[...]interface{}` / `[]interface{}`.
        other => other.to_string(),
    }
}

/// Serialize a slice of [`OverrideOperation`] to a JSON string for storage.
/// Ported 1:1 from Go `SerializeOverrideOperations`.
///
/// Empty input serializes to `"[]"` (matching Go), never `""`.
pub fn serialize_override_operations(ops: &[OverrideOperation]) -> Result<String, String> {
    if ops.is_empty() {
        return Ok("[]".to_string());
    }

    serde_json::to_string(ops)
        .map_err(|err| format!("failed to serialize override operations: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::objects::channel_settings::HeaderEntry;

    #[test]
    fn override_operation_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let op = OverrideOperation {
            op: override_op::ARRAY_INSERT.to_string(),
            path: "$.messages".to_string(),
            value: r#"{"role":"system"}"#.to_string(),
            index: Some(0),
            splat: Some(false),
            ..Default::default()
        };
        let json = serde_json::to_string(&op)?;
        let back: OverrideOperation = serde_json::from_str(&json)?;
        assert_eq!(op, back);
        Ok(())
    }

    #[test]
    fn override_operation_omits_empty_fields() -> Result<(), Box<dyn std::error::Error>> {
        let op = OverrideOperation {
            op: override_op::DELETE.to_string(),
            path: "$.temperature".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&op)?;
        // `from`, `to`, `value`, `condition`, `match`, `index`, `splat` must be
        // absent (Go `omitempty`).
        assert_eq!(json, r#"{"op":"delete","path":"$.temperature"}"#);
        Ok(())
    }

    #[test]
    fn override_match_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let m = OverrideMatch {
            path: "$.id".to_string(),
            eq: "remove-me".to_string(),
        };
        let json = serde_json::to_string(&m)?;
        let back: OverrideMatch = serde_json::from_str(&json)?;
        assert_eq!(m, back);
        Ok(())
    }

    #[test]
    fn header_entries_clear_becomes_delete() {
        let headers = vec![
            HeaderEntry {
                key: "X-Old".to_string(),
                value: "__CONDUIT_CLEAR__".to_string(),
            },
            HeaderEntry {
                key: "User-Agent".to_string(),
                value: "Conduit API".to_string(),
            },
        ];
        match header_entries_to_override_operations(&headers) {
            Some(ops) => {
                assert_eq!(ops.len(), 2);
                assert_eq!(ops[0].op, override_op::DELETE);
                assert_eq!(ops[0].path, "X-Old");
                assert_eq!(ops[1].op, override_op::SET);
                assert_eq!(ops[1].path, "User-Agent");
                assert_eq!(ops[1].value, "Conduit API");
            }
            None => panic!("non-empty headers should produce Some"),
        }
    }

    #[test]
    fn header_entries_empty_returns_none() {
        assert!(header_entries_to_override_operations(&[]).is_none());
    }

    #[test]
    fn parse_empty_inputs_return_none() -> Result<(), String> {
        assert!(parse_override_operations("")?.is_none());
        assert!(parse_override_operations("   ")?.is_none());
        assert!(parse_override_operations("{}")?.is_none());
        assert!(parse_override_operations("[]")?.is_none());
        Ok(())
    }

    #[test]
    fn parse_array_format_round_trip() -> Result<(), String> {
        let raw = r#"[
            {"op":"set","path":"$.temperature","value":"0.7"},
            {"op":"delete","path":"$.top_p"}
        ]"#;
        let ops = parse_override_operations(raw)?;
        let ops = match ops {
            Some(ops) => ops,
            None => return Err("non-empty array should yield Some".to_string()),
        };
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].op, override_op::SET);
        assert_eq!(ops[0].path, "$.temperature");
        assert_eq!(ops[1].op, override_op::DELETE);

        // Round-trip through serialize.
        let serialized = serialize_override_operations(&ops)?;
        let reparsed = match parse_override_operations(&serialized)? {
            Some(r) => r,
            None => return Err("reparsed should yield Some".to_string()),
        };
        assert_eq!(ops, reparsed);
        Ok(())
    }

    #[test]
    fn parse_legacy_map_clear_becomes_delete() -> Result<(), String> {
        let raw = r#"{"max_tokens":"__CONDUIT_CLEAR__","temperature":"0.7"}"#;
        let ops = parse_override_operations(raw)?;
        let ops = match ops {
            Some(ops) => ops,
            None => return Err("legacy map should yield Some".to_string()),
        };
        // Map iteration order is not deterministic; find by path.
        let mut by_path: std::collections::HashMap<&str, &OverrideOperation> =
            std::collections::HashMap::new();
        for op in &ops {
            if !op.path.is_empty() {
                by_path.insert(op.path.as_str(), op);
            }
        }
        assert_eq!(by_path.len(), 2);
        match by_path.get("max_tokens") {
            Some(max_tokens) => assert_eq!(max_tokens.op, override_op::DELETE),
            None => return Err("max_tokens should be present".to_string()),
        }
        match by_path.get("temperature") {
            Some(temperature) => {
                assert_eq!(temperature.op, override_op::SET);
                assert_eq!(temperature.value, "0.7");
            }
            None => return Err("temperature should be present".to_string()),
        }
        Ok(())
    }

    #[test]
    fn parse_legacy_map_non_string_value_is_stringified() -> Result<(), String> {
        // Go `fmt.Sprintf("%v", 100)` => "100"; our port mirrors that for numbers.
        let raw = r#"{"max_tokens":100,"stream":true}"#;
        let ops = parse_override_operations(raw)?;
        let ops = match ops {
            Some(ops) => ops,
            None => return Err("legacy map should yield Some".to_string()),
        };
        let mut by_path: std::collections::HashMap<&str, &OverrideOperation> =
            std::collections::HashMap::new();
        for op in &ops {
            if !op.path.is_empty() {
                by_path.insert(op.path.as_str(), op);
            }
        }
        match by_path.get("max_tokens") {
            Some(op) => assert_eq!(op.value, "100"),
            None => return Err("max_tokens should be present".to_string()),
        }
        match by_path.get("stream") {
            Some(op) => assert_eq!(op.value, "true"),
            None => return Err("stream should be present".to_string()),
        }
        Ok(())
    }

    #[test]
    fn parse_invalid_array_errors() {
        match parse_override_operations("[{not json}]") {
            Err(msg) => assert!(
                msg.contains("invalid override operations"),
                "error message should mention operations: {msg}"
            ),
            Ok(other) => panic!("invalid JSON should error, got: {other:?}"),
        }
    }

    #[test]
    fn parse_invalid_object_errors() {
        match parse_override_operations("{not json}") {
            Err(msg) => assert!(
                msg.contains("invalid override parameters"),
                "error message should mention parameters: {msg}"
            ),
            Ok(other) => panic!("invalid JSON should error, got: {other:?}"),
        }
    }

    #[test]
    fn serialize_empty_returns_empty_array() -> Result<(), String> {
        assert_eq!(serialize_override_operations(&[])?, "[]");
        Ok(())
    }

    #[test]
    fn serialize_round_trip_preserves_fields() -> Result<(), Box<dyn std::error::Error>> {
        let ops = vec![
            OverrideOperation {
                op: override_op::SET.to_string(),
                path: "$.temperature".to_string(),
                value: "0.7".to_string(),
                ..Default::default()
            },
            OverrideOperation {
                op: override_op::ARRAY_REMOVE.to_string(),
                path: "$.tools".to_string(),
                r#match: Some(OverrideMatch {
                    path: "name".to_string(),
                    eq: "old-tool".to_string(),
                }),
                ..Default::default()
            },
        ];
        let json = serialize_override_operations(&ops)?;
        let back = match parse_override_operations(&json)? {
            Some(b) => b,
            None => return Err("reparsed should yield Some".into()),
        };
        assert_eq!(ops, back);
        Ok(())
    }
}
