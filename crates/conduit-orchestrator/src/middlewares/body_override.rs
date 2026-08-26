//! Pure JSON body / header override operations ported from
//! `conduit/internal/server/orchestrator/override.go` lines 182-437.
//!
//! These functions manipulate a [`serde_json::Value`] body or a header map
//! according to an [`OverrideOperation`]. They are intentionally free of
//! `PersistenceState`, async, or template rendering -- those concerns are
//! layered on by the middleware wrapper that will be added later.
//!
//! Path navigation uses gjson/sjson **dot-path** semantics:
//! - `"key"` -- top-level key,
//! - `"parent.child"` -- nested key,
//! - `"arr.0"` -- array index.

use std::collections::HashMap;

use conduit_core::objects::overrides::{OverrideMatch, OverrideOperation, override_op};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by body-override operations.
#[derive(Debug, thiserror::Error)]
pub enum BodyOverrideError {
    #[error("array op requires a path")]
    ArrayOpMissingPath,
    #[error("path {path:?} is not an array")]
    NotAnArray { path: String },
    #[error("array_insert requires an index")]
    ArrayInsertMissingIndex,
    #[error("array_remove requires a match")]
    ArrayRemoveMissingMatch,
    #[error("array_remove requires a match path")]
    ArrayRemoveMissingMatchPath,
    #[error("array_remove requires a match eq value")]
    ArrayRemoveMissingMatchEq,
    #[error("unknown override operation: {op}")]
    UnknownOp { op: String },
}

// ---------------------------------------------------------------------------
// Internal: dot-path helpers (gjson/sjson parity)
// ---------------------------------------------------------------------------

/// Navigate a single segment of a dot-path into a `Value`, returning a mutable
/// reference. Handles both object keys and numeric array indices.
fn navigate_one_level<'v>(
    val: &'v mut serde_json::Value,
    seg: &str,
) -> Option<&'v mut serde_json::Value> {
    match val {
        serde_json::Value::Object(map) => map.get_mut(seg),
        serde_json::Value::Array(arr) => {
            let idx: usize = seg.parse().ok()?;
            arr.get_mut(idx)
        }
        _ => None,
    }
}

/// Navigate to the parent container of the leaf segment, returning a mutable
/// reference to the parent and the final segment name.
///
/// Returns `None` when an intermediate segment is missing or not navigable.
fn navigate_to_parent_mut<'v, 'p>(
    root: &'v mut serde_json::Value,
    segments: &'p [&str],
) -> Option<(&'v mut serde_json::Value, &'p str)> {
    if segments.is_empty() {
        return None;
    }
    if segments.len() == 1 {
        return Some((root, segments[0]));
    }
    let mut current = root;
    for &seg in &segments[..segments.len() - 1] {
        current = navigate_one_level(current, seg)?;
    }
    Some((current, segments[segments.len() - 1]))
}

/// Get an immutable reference at a dot-path.
fn get_at_path<'v>(root: &'v serde_json::Value, path: &str) -> Option<&'v serde_json::Value> {
    let mut current = root;
    for seg in path.split('.') {
        if seg.is_empty() {
            continue;
        }
        current = match current {
            serde_json::Value::Object(map) => map.get(seg)?,
            serde_json::Value::Array(arr) => {
                let idx: usize = seg.parse().ok()?;
                arr.get(idx)?
            }
            _ => return None,
        };
    }
    Some(current)
}

/// Set a value at a dot-path, creating intermediate objects as needed.
fn set_at_path(root: &mut serde_json::Value, path: &str, value: serde_json::Value) {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return;
    }

    let mut current = root;
    for &seg in &segments[..segments.len() - 1] {
        // If the next level doesn't exist, create an intermediate object.
        let exists = match current {
            serde_json::Value::Object(map) => map.contains_key(seg),
            serde_json::Value::Array(arr) => seg.parse::<usize>().is_ok_and(|idx| idx < arr.len()),
            _ => false,
        };
        if !exists && let serde_json::Value::Object(map) = current {
            map.insert(
                seg.to_string(),
                serde_json::Value::Object(serde_json::Map::new()),
            );
        }
        match navigate_one_level(current, seg) {
            Some(v) => current = v,
            None => return,
        }
    }

    let last = segments[segments.len() - 1];
    match current {
        serde_json::Value::Object(map) => {
            map.insert(last.to_string(), value);
        }
        serde_json::Value::Array(arr) => {
            if let Ok(idx) = last.parse::<usize>()
                && idx < arr.len()
            {
                arr[idx] = value;
            }
        }
        _ => {}
    }
}

/// Delete a value at a dot-path. Returns whether it existed.
fn delete_at_path(root: &mut serde_json::Value, path: &str) -> bool {
    let segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    match navigate_to_parent_mut(root, &segments) {
        Some((parent, key)) => match parent {
            serde_json::Value::Object(map) => map.remove(key).is_some(),
            serde_json::Value::Array(arr) => {
                if let Ok(idx) = key.parse::<usize>()
                    && idx < arr.len()
                {
                    arr.remove(idx);
                    return true;
                }
                false
            }
            _ => false,
        },
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Array insert mode (mirrors Go `arrayInsertMode`)
// ---------------------------------------------------------------------------

/// Determines where values are spliced into an array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArrayInsertMode {
    /// Insert at position 0 (`array_prepend`).
    Start,
    /// Append to the end (`array_append`).
    End,
    /// Insert at a caller-specified `index` (`array_insert`).
    AtIndex,
}

// ---------------------------------------------------------------------------
// Value parsing (mirrors Go `renderOverrideValue` without template rendering)
// ---------------------------------------------------------------------------

/// Attempt to interpret a string value as JSON. If it looks like a structured
/// value (object, array, number, boolean, null) and parses successfully the
/// parsed `Value` is returned; otherwise the raw string is wrapped in
/// `Value::String`. This mirrors Go `renderOverrideValue` (override.go:97-115).
fn parse_override_value(value: &str) -> serde_json::Value {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return serde_json::Value::String(value.to_string());
    }
    let first = trimmed.as_bytes()[0];
    let looks_like_json = first == b'{'
        || first == b'['
        || first.is_ascii_digit()
        || first == b'-'
        || trimmed == "true"
        || trimmed == "false"
        || trimmed == "null";
    if looks_like_json && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return parsed;
    }
    serde_json::Value::String(value.to_string())
}

// ---------------------------------------------------------------------------
// Public body-override functions
// ---------------------------------------------------------------------------

/// Dispatcher -- apply a single [`OverrideOperation`] to a JSON body.
///
/// Go parity: `applyBodyOperation` (override.go:182-216).
///
/// Condition evaluation and template rendering are intentionally NOT done here;
/// those depend on `RenderContext` and will be handled by the middleware wrapper.
pub fn apply_body_operation(
    body: &mut serde_json::Value,
    op: &OverrideOperation,
) -> Result<(), BodyOverrideError> {
    match op.op.as_str() {
        override_op::SET => {
            apply_body_set(body, op);
            Ok(())
        }
        override_op::DELETE => {
            apply_body_delete(body, op);
            Ok(())
        }
        override_op::RENAME => {
            apply_body_rename(body, op);
            Ok(())
        }
        override_op::COPY => {
            apply_body_copy(body, op);
            Ok(())
        }
        override_op::ARRAY_APPEND => apply_body_array_insert(body, op, ArrayInsertMode::End),
        override_op::ARRAY_PREPEND => apply_body_array_insert(body, op, ArrayInsertMode::Start),
        override_op::ARRAY_INSERT => apply_body_array_insert(body, op, ArrayInsertMode::AtIndex),
        override_op::ARRAY_REMOVE => apply_body_array_remove(body, op),
        _ => Err(BodyOverrideError::UnknownOp { op: op.op.clone() }),
    }
}

/// Set a value at `op.path`. The value string is parsed via
/// [`parse_override_value`] to mirror Go's JSON-parse logic.
/// A value of `"__CONDUIT_CLEAR__"` deletes the path instead.
///
/// Go parity: `applyBodySet` (override.go:218-231).
pub fn apply_body_set(body: &mut serde_json::Value, op: &OverrideOperation) {
    let parsed = parse_override_value(&op.value);
    if parsed.as_str() == Some("__CONDUIT_CLEAR__") {
        delete_at_path(body, &op.path);
        return;
    }
    set_at_path(body, &op.path, parsed);
}

/// Delete the value at `op.path`.
///
/// Go parity: `applyBodyDelete` (override.go:233-235).
pub fn apply_body_delete(body: &mut serde_json::Value, op: &OverrideOperation) {
    delete_at_path(body, &op.path);
}

/// Rename: move the value at `op.from` to `op.to`. If `from` doesn't exist
/// the body is unchanged.
///
/// Go parity: `applyBodyRename` (override.go:237-249).
pub fn apply_body_rename(body: &mut serde_json::Value, op: &OverrideOperation) {
    let value = match get_at_path(body, &op.from) {
        Some(v) => v.clone(),
        None => return,
    };
    delete_at_path(body, &op.from);
    set_at_path(body, &op.to, value);
}

/// Copy the value at `op.from` to `op.to`. If `from` doesn't exist the body
/// is unchanged (no error, matching Go).
///
/// Go parity: `applyBodyCopy` (override.go:251-258).
pub fn apply_body_copy(body: &mut serde_json::Value, op: &OverrideOperation) {
    let value = match get_at_path(body, &op.from) {
        Some(v) => v.clone(),
        None => return,
    };
    set_at_path(body, &op.to, value);
}

/// Insert value(s) into an array at `op.path`.
///
/// Go parity: `applyBodyArrayInsert` (override.go:276-349).
///
/// - If the parsed value is a JSON array and `splat` is true (default), its
///   elements are spread into the target array.
/// - When the target path doesn't exist, a new array is created.
/// - When the target path exists but isn't an array, an error is returned.
/// - For [`ArrayInsertMode::AtIndex`], `op.index` is required; negative
///   values count from the end, out-of-range is clamped to `[0, len]`.
fn apply_body_array_insert(
    body: &mut serde_json::Value,
    op: &OverrideOperation,
    mode: ArrayInsertMode,
) -> Result<(), BodyOverrideError> {
    if op.path.is_empty() {
        return Err(BodyOverrideError::ArrayOpMissingPath);
    }

    let rendered = parse_override_value(&op.value);

    let splat = op.splat.unwrap_or(true);

    let to_insert: Vec<serde_json::Value> = match (&rendered, splat) {
        (serde_json::Value::Array(arr), true) => arr.clone(),
        _ => vec![rendered],
    };

    let existing = get_at_path(body, &op.path).cloned();

    match existing {
        None => {
            // Create a new array at the path.
            set_at_path(body, &op.path, serde_json::Value::Array(to_insert));
            Ok(())
        }
        Some(serde_json::Value::Array(ref current)) => {
            let len = current.len();
            let pos = match mode {
                ArrayInsertMode::Start => 0,
                ArrayInsertMode::End => len,
                ArrayInsertMode::AtIndex => {
                    let idx = op.index.ok_or(BodyOverrideError::ArrayInsertMissingIndex)?;
                    let mut pos = idx as isize;
                    if pos < 0 {
                        pos += len as isize;
                    }
                    if pos < 0 {
                        pos = 0;
                    }
                    if pos as usize > len {
                        len
                    } else {
                        pos as usize
                    }
                }
            };

            let mut merged = Vec::with_capacity(len + to_insert.len());
            merged.extend_from_slice(&current[..pos]);
            merged.extend(to_insert);
            merged.extend_from_slice(&current[pos..]);

            set_at_path(body, &op.path, serde_json::Value::Array(merged));
            Ok(())
        }
        Some(_) => Err(BodyOverrideError::NotAnArray {
            path: op.path.clone(),
        }),
    }
}

/// Remove items from the array at `op.path` whose sub-value at `match.path`
/// equals `match.eq`.
///
/// Go parity: `applyBodyArrayRemove` (override.go:352-392).
fn apply_body_array_remove(
    body: &mut serde_json::Value,
    op: &OverrideOperation,
) -> Result<(), BodyOverrideError> {
    if op.path.is_empty() {
        return Err(BodyOverrideError::ArrayOpMissingPath);
    }

    let m: &OverrideMatch = match &op.r#match {
        Some(m) => m,
        None => return Err(BodyOverrideError::ArrayRemoveMissingMatch),
    };

    if m.path.trim().is_empty() {
        return Err(BodyOverrideError::ArrayRemoveMissingMatchPath);
    }

    if m.eq.trim().is_empty() {
        return Err(BodyOverrideError::ArrayRemoveMissingMatchEq);
    }

    let existing = get_at_path(body, &op.path).cloned();
    match existing {
        None => Ok(()),
        Some(serde_json::Value::Array(arr)) => {
            let match_eq = m.eq.trim();
            let kept: Vec<serde_json::Value> = arr
                .into_iter()
                .filter(|item| {
                    // Keep items that don't match.
                    match get_at_path(item, &m.path) {
                        Some(v) => v.as_str() != Some(match_eq),
                        None => true,
                    }
                })
                .collect();
            set_at_path(body, &op.path, serde_json::Value::Array(kept));
            Ok(())
        }
        Some(_) => Err(BodyOverrideError::NotAnArray {
            path: op.path.clone(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Header override (pure, no template rendering)
// ---------------------------------------------------------------------------

/// Apply a single override operation to a header map.
///
/// Go parity: `applyOverrideOperationToHeaders` (override.go:394-437), minus
/// template rendering and condition evaluation (handled by the middleware
/// wrapper).
///
/// Supported ops: `set`, `delete`, `rename`, `copy`. Unknown ops are logged
/// via `tracing::warn`.
pub fn apply_override_operation_to_headers(
    headers: &mut HashMap<String, String>,
    op: &OverrideOperation,
) {
    match op.op.as_str() {
        override_op::SET => {
            if op.value == "__CONDUIT_CLEAR__" {
                headers.remove(&op.path);
                return;
            }
            headers.insert(op.path.clone(), op.value.clone());
        }
        override_op::DELETE => {
            headers.remove(&op.path);
        }
        override_op::RENAME => {
            if let Some(value) = headers.remove(&op.from) {
                headers.insert(op.to.clone(), value);
            }
        }
        override_op::COPY => {
            if let Some(value) = headers.get(&op.from).cloned() {
                headers.insert(op.to.clone(), value);
            }
        }
        _ => {
            tracing::warn!(op = %op.op, "unknown header override operation");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_core::objects::overrides::{OverrideMatch, OverrideOperation, override_op};
    use serde_json::json;

    // Helper to build a body from a JSON literal.
    fn body(s: &str) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(s)
    }

    // -------------------------------------------------------------------
    // apply_body_set
    // -------------------------------------------------------------------

    #[test]
    fn set_top_level_string() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"temperature": 0.5}"#)?;
        let op = OverrideOperation {
            op: override_op::SET.to_string(),
            path: "temperature".to_string(),
            value: "0.9".to_string(),
            ..Default::default()
        };
        apply_body_set(&mut b, &op);
        assert_eq!(b["temperature"], json!(0.9));
        Ok(())
    }

    #[test]
    fn set_nested_path() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"config": {"model": "gpt-4"}}"#)?;
        let op = OverrideOperation {
            op: override_op::SET.to_string(),
            path: "config.model".to_string(),
            value: "gpt-4o".to_string(),
            ..Default::default()
        };
        apply_body_set(&mut b, &op);
        assert_eq!(b["config"]["model"], json!("gpt-4o"));
        Ok(())
    }

    #[test]
    fn set_creates_intermediate_objects() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{}"#)?;
        let op = OverrideOperation {
            op: override_op::SET.to_string(),
            path: "a.b".to_string(),
            value: "42".to_string(),
            ..Default::default()
        };
        apply_body_set(&mut b, &op);
        assert_eq!(b["a"]["b"], json!(42));
        Ok(())
    }

    #[test]
    fn set_conduit_clear_deletes() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"temperature": 0.5}"#)?;
        let op = OverrideOperation {
            op: override_op::SET.to_string(),
            path: "temperature".to_string(),
            value: "__CONDUIT_CLEAR__".to_string(),
            ..Default::default()
        };
        apply_body_set(&mut b, &op);
        assert!(
            !b.as_object()
                .map_or(false, |o| o.contains_key("temperature"))
        );
        Ok(())
    }

    #[test]
    fn set_json_object_value() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{}"#)?;
        let op = OverrideOperation {
            op: override_op::SET.to_string(),
            path: "metadata".to_string(),
            value: r#"{"key":"val"}"#.to_string(),
            ..Default::default()
        };
        apply_body_set(&mut b, &op);
        assert_eq!(b["metadata"]["key"], json!("val"));
        Ok(())
    }

    // -------------------------------------------------------------------
    // apply_body_delete
    // -------------------------------------------------------------------

    #[test]
    fn delete_existing_key() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"a": 1, "b": 2}"#)?;
        let op = OverrideOperation {
            op: override_op::DELETE.to_string(),
            path: "a".to_string(),
            ..Default::default()
        };
        apply_body_delete(&mut b, &op);
        assert!(!b.as_object().map_or(false, |o| o.contains_key("a")));
        assert_eq!(b["b"], json!(2));
        Ok(())
    }

    #[test]
    fn delete_nested_key() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"config": {"model": "gpt-4", "temp": 0.5}}"#)?;
        let op = OverrideOperation {
            op: override_op::DELETE.to_string(),
            path: "config.temp".to_string(),
            ..Default::default()
        };
        apply_body_delete(&mut b, &op);
        assert!(
            !b["config"]
                .as_object()
                .map_or(false, |o| o.contains_key("temp"))
        );
        assert_eq!(b["config"]["model"], json!("gpt-4"));
        Ok(())
    }

    #[test]
    fn delete_missing_key_is_noop() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"a": 1}"#)?;
        let original = b.clone();
        let op = OverrideOperation {
            op: override_op::DELETE.to_string(),
            path: "nonexistent".to_string(),
            ..Default::default()
        };
        apply_body_delete(&mut b, &op);
        assert_eq!(b, original);
        Ok(())
    }

    // -------------------------------------------------------------------
    // apply_body_rename
    // -------------------------------------------------------------------

    #[test]
    fn rename_existing_key() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"old_name": "hello"}"#)?;
        let op = OverrideOperation {
            op: override_op::RENAME.to_string(),
            from: "old_name".to_string(),
            to: "new_name".to_string(),
            ..Default::default()
        };
        apply_body_rename(&mut b, &op);
        assert!(!b.as_object().map_or(false, |o| o.contains_key("old_name")));
        assert_eq!(b["new_name"], json!("hello"));
        Ok(())
    }

    #[test]
    fn rename_missing_key_is_noop() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"a": 1}"#)?;
        let original = b.clone();
        let op = OverrideOperation {
            op: override_op::RENAME.to_string(),
            from: "nonexistent".to_string(),
            to: "new_name".to_string(),
            ..Default::default()
        };
        apply_body_rename(&mut b, &op);
        assert_eq!(b, original);
        Ok(())
    }

    #[test]
    fn rename_nested_key() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"config": {"old": "value"}}"#)?;
        let op = OverrideOperation {
            op: override_op::RENAME.to_string(),
            from: "config.old".to_string(),
            to: "config.new_key".to_string(),
            ..Default::default()
        };
        apply_body_rename(&mut b, &op);
        assert!(
            !b["config"]
                .as_object()
                .map_or(false, |o| o.contains_key("old"))
        );
        assert_eq!(b["config"]["new_key"], json!("value"));
        Ok(())
    }

    // -------------------------------------------------------------------
    // apply_body_copy
    // -------------------------------------------------------------------

    #[test]
    fn copy_existing_value() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"source": "value"}"#)?;
        let op = OverrideOperation {
            op: override_op::COPY.to_string(),
            from: "source".to_string(),
            to: "dest".to_string(),
            ..Default::default()
        };
        apply_body_copy(&mut b, &op);
        assert_eq!(b["source"], json!("value"));
        assert_eq!(b["dest"], json!("value"));
        Ok(())
    }

    #[test]
    fn copy_missing_source_is_noop() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"a": 1}"#)?;
        let original = b.clone();
        let op = OverrideOperation {
            op: override_op::COPY.to_string(),
            from: "nonexistent".to_string(),
            to: "dest".to_string(),
            ..Default::default()
        };
        apply_body_copy(&mut b, &op);
        assert_eq!(b, original);
        Ok(())
    }

    #[test]
    fn copy_complex_value() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"nested": {"items": [1, 2, 3]}}"#)?;
        let op = OverrideOperation {
            op: override_op::COPY.to_string(),
            from: "nested".to_string(),
            to: "backup".to_string(),
            ..Default::default()
        };
        apply_body_copy(&mut b, &op);
        assert_eq!(b["backup"]["items"], json!([1, 2, 3]));
        Ok(())
    }

    // -------------------------------------------------------------------
    // apply_body_array_insert (append / prepend / at-index)
    // -------------------------------------------------------------------

    #[test]
    fn array_append_to_existing() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"items": [1, 2]}"#)?;
        let op = OverrideOperation {
            op: override_op::ARRAY_APPEND.to_string(),
            path: "items".to_string(),
            value: "3".to_string(),
            ..Default::default()
        };
        apply_body_operation(&mut b, &op)?;
        assert_eq!(b["items"], json!([1, 2, 3]));
        Ok(())
    }

    #[test]
    fn array_prepend_to_existing() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"items": [2, 3]}"#)?;
        let op = OverrideOperation {
            op: override_op::ARRAY_PREPEND.to_string(),
            path: "items".to_string(),
            value: "1".to_string(),
            ..Default::default()
        };
        apply_body_operation(&mut b, &op)?;
        assert_eq!(b["items"], json!([1, 2, 3]));
        Ok(())
    }

    #[test]
    fn array_insert_at_index() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"items": [1, 3]}"#)?;
        let op = OverrideOperation {
            op: override_op::ARRAY_INSERT.to_string(),
            path: "items".to_string(),
            value: "2".to_string(),
            index: Some(1),
            ..Default::default()
        };
        apply_body_operation(&mut b, &op)?;
        assert_eq!(b["items"], json!([1, 2, 3]));
        Ok(())
    }

    #[test]
    fn array_insert_negative_index() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"items": [1, 2, 3]}"#)?;
        let op = OverrideOperation {
            op: override_op::ARRAY_INSERT.to_string(),
            path: "items".to_string(),
            value: "99".to_string(),
            index: Some(-1),
            ..Default::default()
        };
        apply_body_operation(&mut b, &op)?;
        // -1 => len(3) + (-1) = 2 => insert before the last element.
        assert_eq!(b["items"], json!([1, 2, 99, 3]));
        Ok(())
    }

    #[test]
    fn array_insert_creates_new_array() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{}"#)?;
        let op = OverrideOperation {
            op: override_op::ARRAY_APPEND.to_string(),
            path: "items".to_string(),
            value: r#"["a","b"]"#.to_string(),
            ..Default::default()
        };
        // Default splat=true, so array is spread.
        apply_body_operation(&mut b, &op)?;
        assert_eq!(b["items"], json!(["a", "b"]));
        Ok(())
    }

    #[test]
    fn array_insert_splat_false() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"items": [1]}"#)?;
        let op = OverrideOperation {
            op: override_op::ARRAY_APPEND.to_string(),
            path: "items".to_string(),
            value: r#"[2, 3]"#.to_string(),
            splat: Some(false),
            ..Default::default()
        };
        apply_body_operation(&mut b, &op)?;
        // With splat=false, [2,3] is inserted as a single element.
        assert_eq!(b["items"], json!([1, [2, 3]]));
        Ok(())
    }

    #[test]
    fn array_insert_not_array_errors() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"items": "not-an-array"}"#)?;
        let op = OverrideOperation {
            op: override_op::ARRAY_APPEND.to_string(),
            path: "items".to_string(),
            value: "1".to_string(),
            ..Default::default()
        };
        let result = apply_body_operation(&mut b, &op);
        assert!(matches!(result, Err(BodyOverrideError::NotAnArray { .. })));
        Ok(())
    }

    #[test]
    fn array_insert_missing_path_errors() {
        let mut b = json!({});
        let op = OverrideOperation {
            op: override_op::ARRAY_APPEND.to_string(),
            path: String::new(),
            value: "1".to_string(),
            ..Default::default()
        };
        let result = apply_body_operation(&mut b, &op);
        assert!(matches!(result, Err(BodyOverrideError::ArrayOpMissingPath)));
    }

    #[test]
    fn array_insert_at_index_missing_index_errors() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"items": [1]}"#)?;
        let op = OverrideOperation {
            op: override_op::ARRAY_INSERT.to_string(),
            path: "items".to_string(),
            value: "2".to_string(),
            index: None,
            ..Default::default()
        };
        let result = apply_body_operation(&mut b, &op);
        assert!(matches!(
            result,
            Err(BodyOverrideError::ArrayInsertMissingIndex)
        ));
        Ok(())
    }

    #[test]
    fn array_insert_clamps_out_of_range() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"items": [1, 2]}"#)?;
        let op = OverrideOperation {
            op: override_op::ARRAY_INSERT.to_string(),
            path: "items".to_string(),
            value: "99".to_string(),
            index: Some(100),
            ..Default::default()
        };
        apply_body_operation(&mut b, &op)?;
        assert_eq!(b["items"], json!([1, 2, 99]));
        Ok(())
    }

    // -------------------------------------------------------------------
    // apply_body_array_remove
    // -------------------------------------------------------------------

    #[test]
    fn array_remove_matching_items() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(
            r#"{"tools": [
                {"name": "keep-me", "id": 1},
                {"name": "remove-me", "id": 2},
                {"name": "also-keep", "id": 3}
            ]}"#,
        )?;
        let op = OverrideOperation {
            op: override_op::ARRAY_REMOVE.to_string(),
            path: "tools".to_string(),
            r#match: Some(OverrideMatch {
                path: "name".to_string(),
                eq: "remove-me".to_string(),
            }),
            ..Default::default()
        };
        apply_body_operation(&mut b, &op)?;
        let arr = b["tools"].as_array().ok_or("expected array after remove")?;
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], json!("keep-me"));
        assert_eq!(arr[1]["name"], json!("also-keep"));
        Ok(())
    }

    #[test]
    fn array_remove_no_match_keeps_all() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"items": [{"k": "a"}, {"k": "b"}]}"#)?;
        let op = OverrideOperation {
            op: override_op::ARRAY_REMOVE.to_string(),
            path: "items".to_string(),
            r#match: Some(OverrideMatch {
                path: "k".to_string(),
                eq: "nonexistent".to_string(),
            }),
            ..Default::default()
        };
        apply_body_operation(&mut b, &op)?;
        let arr = b["items"].as_array().ok_or("expected array after remove")?;
        assert_eq!(arr.len(), 2);
        Ok(())
    }

    #[test]
    fn array_remove_missing_array_is_noop() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{}"#)?;
        let original = b.clone();
        let op = OverrideOperation {
            op: override_op::ARRAY_REMOVE.to_string(),
            path: "nonexistent".to_string(),
            r#match: Some(OverrideMatch {
                path: "k".to_string(),
                eq: "v".to_string(),
            }),
            ..Default::default()
        };
        apply_body_operation(&mut b, &op)?;
        assert_eq!(b, original);
        Ok(())
    }

    #[test]
    fn array_remove_missing_match_errors() {
        let mut b = json!({"items": [1]});
        let op = OverrideOperation {
            op: override_op::ARRAY_REMOVE.to_string(),
            path: "items".to_string(),
            r#match: None,
            ..Default::default()
        };
        let result = apply_body_operation(&mut b, &op);
        assert!(matches!(
            result,
            Err(BodyOverrideError::ArrayRemoveMissingMatch)
        ));
    }

    #[test]
    fn array_remove_empty_match_path_errors() {
        let mut b = json!({"items": [1]});
        let op = OverrideOperation {
            op: override_op::ARRAY_REMOVE.to_string(),
            path: "items".to_string(),
            r#match: Some(OverrideMatch {
                path: "  ".to_string(),
                eq: "v".to_string(),
            }),
            ..Default::default()
        };
        let result = apply_body_operation(&mut b, &op);
        assert!(matches!(
            result,
            Err(BodyOverrideError::ArrayRemoveMissingMatchPath)
        ));
    }

    #[test]
    fn array_remove_empty_match_eq_errors() {
        let mut b = json!({"items": [1]});
        let op = OverrideOperation {
            op: override_op::ARRAY_REMOVE.to_string(),
            path: "items".to_string(),
            r#match: Some(OverrideMatch {
                path: "k".to_string(),
                eq: "  ".to_string(),
            }),
            ..Default::default()
        };
        let result = apply_body_operation(&mut b, &op);
        assert!(matches!(
            result,
            Err(BodyOverrideError::ArrayRemoveMissingMatchEq)
        ));
    }

    // -------------------------------------------------------------------
    // apply_body_operation (dispatcher)
    // -------------------------------------------------------------------

    #[test]
    fn dispatcher_routes_set() -> Result<(), Box<dyn std::error::Error>> {
        let mut b = body(r#"{"x": 1}"#)?;
        let op = OverrideOperation {
            op: override_op::SET.to_string(),
            path: "x".to_string(),
            value: "2".to_string(),
            ..Default::default()
        };
        apply_body_operation(&mut b, &op)?;
        assert_eq!(b["x"], json!(2));
        Ok(())
    }

    #[test]
    fn dispatcher_unknown_op_errors() {
        let mut b = json!({});
        let op = OverrideOperation {
            op: "magic".to_string(),
            ..Default::default()
        };
        let result = apply_body_operation(&mut b, &op);
        assert!(matches!(result, Err(BodyOverrideError::UnknownOp { .. })));
    }

    // -------------------------------------------------------------------
    // apply_override_operation_to_headers
    // -------------------------------------------------------------------

    #[test]
    fn header_set() {
        let mut headers = HashMap::new();
        let op = OverrideOperation {
            op: override_op::SET.to_string(),
            path: "X-Custom".to_string(),
            value: "hello".to_string(),
            ..Default::default()
        };
        apply_override_operation_to_headers(&mut headers, &op);
        assert_eq!(headers.get("X-Custom").map(|s| s.as_str()), Some("hello"));
    }

    #[test]
    fn header_set_conduit_clear_deletes() {
        let mut headers = HashMap::new();
        headers.insert("X-Remove".to_string(), "old".to_string());
        let op = OverrideOperation {
            op: override_op::SET.to_string(),
            path: "X-Remove".to_string(),
            value: "__CONDUIT_CLEAR__".to_string(),
            ..Default::default()
        };
        apply_override_operation_to_headers(&mut headers, &op);
        assert!(!headers.contains_key("X-Remove"));
    }

    #[test]
    fn header_delete() {
        let mut headers = HashMap::new();
        headers.insert("X-Kill".to_string(), "val".to_string());
        let op = OverrideOperation {
            op: override_op::DELETE.to_string(),
            path: "X-Kill".to_string(),
            ..Default::default()
        };
        apply_override_operation_to_headers(&mut headers, &op);
        assert!(!headers.contains_key("X-Kill"));
    }

    #[test]
    fn header_rename() {
        let mut headers = HashMap::new();
        headers.insert("Old-Header".to_string(), "value".to_string());
        let op = OverrideOperation {
            op: override_op::RENAME.to_string(),
            from: "Old-Header".to_string(),
            to: "New-Header".to_string(),
            ..Default::default()
        };
        apply_override_operation_to_headers(&mut headers, &op);
        assert!(!headers.contains_key("Old-Header"));
        assert_eq!(headers.get("New-Header").map(|s| s.as_str()), Some("value"));
    }

    #[test]
    fn header_copy() {
        let mut headers = HashMap::new();
        headers.insert("Source".to_string(), "val".to_string());
        let op = OverrideOperation {
            op: override_op::COPY.to_string(),
            from: "Source".to_string(),
            to: "Dest".to_string(),
            ..Default::default()
        };
        apply_override_operation_to_headers(&mut headers, &op);
        assert_eq!(headers.get("Source").map(|s| s.as_str()), Some("val"));
        assert_eq!(headers.get("Dest").map(|s| s.as_str()), Some("val"));
    }

    // -------------------------------------------------------------------
    // parse_override_value
    // -------------------------------------------------------------------

    #[test]
    fn parse_override_value_json_object() {
        let val = parse_override_value(r#"{"key":"val"}"#);
        assert_eq!(val, json!({"key": "val"}));
    }

    #[test]
    fn parse_override_value_number() {
        let val = parse_override_value("42");
        assert_eq!(val, json!(42));
    }

    #[test]
    fn parse_override_value_bool() {
        assert_eq!(parse_override_value("true"), json!(true));
        assert_eq!(parse_override_value("false"), json!(false));
    }

    #[test]
    fn parse_override_value_plain_string() {
        let val = parse_override_value("hello world");
        assert_eq!(val, json!("hello world"));
    }

    #[test]
    fn parse_override_value_null() {
        let val = parse_override_value("null");
        assert_eq!(val, serde_json::Value::Null);
    }
}
