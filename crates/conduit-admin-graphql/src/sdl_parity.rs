//! Test-only SDL block-parity helpers shared by the domain-slice test suites
//! (model.rs and future slices; channel.rs predates this module and keeps its
//! private copies).
//!
//! The captured contract snapshot at
//! `tests/contracts/admin_graphql_schema.graphql` is the oracle: for each
//! GraphQL type we extract the field/value lines from BOTH the generated SDL
//! and the snapshot and require set equality (minus documented pending
//! fields). This simultaneously catches missing fields, wrong
//! types/nullability, wrong casing, and invented fields.

use std::collections::BTreeSet;

pub(crate) type ParityError = Box<dyn std::error::Error>;

/// Reads the captured contract snapshot (workspace-root relative first, then
/// crate-relative for `cargo test -p` runs).
pub(crate) fn snapshot_text() -> Result<String, ParityError> {
    std::fs::read_to_string("tests/contracts/admin_graphql_schema.graphql")
        .or_else(|_| {
            std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/contracts/admin_graphql_schema.graphql"
            ))
        })
        .map_err(|err| format!("snapshot read failed: {err}").into())
}

/// Strip a trailing ` @directive(...)` (snapshot uses `@goField` / `@goModel`)
/// and a trailing default value (` = ASC`) so both SDL dialects normalize to
/// plain `name: Type` lines. Also collapse inline argument lists
/// `name(args): Type` (async-graphql's single-line form for connection edge
/// fields) down to `name(…): Type` so they compare equal against the
/// snapshot's multi-line form which [`block_fields`] collapses the same way
/// (mirrors the private helper in `channel::tests`).
fn normalize_field_line(line: &str) -> String {
    let without_directive = match line.find(" @") {
        Some(pos) => line[..pos].trim_end(),
        None => line,
    };
    let without_default = match without_directive.find(" = ") {
        Some(pos) => without_directive[..pos].trim_end(),
        None => without_directive,
    };
    // Collapse inline args: `name(arg: Type, ...): RetType` → `name(…): RetType`.
    if let (Some(open), Some(close)) = (without_default.find('('), without_default.find("): "))
        && open < close
    {
        let name = without_default[..open].trim_end();
        let return_type = &without_default[close + 3..];
        return format!("{name}(…): {return_type}");
    }
    without_default.to_owned()
}

/// Extract the normalized field/value lines of the `header` type block
/// (e.g. `header = "input ModelWhereInput"`). Handles the snapshot's `"""`
/// docstrings, CRLF line endings, trailing directives, and gqlgen-style
/// multi-line argument fields (collapsed to `name(…): ReturnType`).
pub(crate) fn block_fields(text: &str, header: &str) -> Result<BTreeSet<String>, ParityError> {
    let mut lines = text.lines().map(str::trim);
    let exact_open = format!("{header} {{");
    let prefixed_open = format!("{header} ");
    let mut found = false;
    for line in lines.by_ref() {
        if line == exact_open || (line.starts_with(&prefixed_open) && line.ends_with('{')) {
            found = true;
            break;
        }
    }
    if !found {
        return Err(format!("block `{header}` not found").into());
    }

    let mut fields = BTreeSet::new();
    let mut in_doc = false;
    while let Some(line) = lines.next() {
        if line.starts_with("\"\"\"") {
            // Single-line docstrings (`"""text"""`) do not toggle.
            let single_line = line.len() > 3 && line.ends_with("\"\"\"");
            if !single_line {
                in_doc = !in_doc;
            }
            continue;
        }
        if in_doc {
            continue;
        }
        if line == "}" {
            return Ok(fields);
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(field_name) = line.strip_suffix('(') {
            // gqlgen multi-line argument field: consume until `): Type`.
            let mut inner_doc = false;
            for arg_line in lines.by_ref() {
                if arg_line.starts_with("\"\"\"") {
                    let single_line = arg_line.len() > 3 && arg_line.ends_with("\"\"\"");
                    if !single_line {
                        inner_doc = !inner_doc;
                    }
                    continue;
                }
                if inner_doc {
                    continue;
                }
                if let Some(return_type) = arg_line.strip_prefix("): ") {
                    fields.insert(format!(
                        "{field_name}(…): {}",
                        normalize_field_line(return_type)
                    ));
                    break;
                }
            }
            continue;
        }
        fields.insert(normalize_field_line(line));
    }
    Err(format!("block `{header}` never closed").into())
}

/// Assert the generated block equals the snapshot block minus the listed
/// pending fields; also assert nothing was invented (generated ⊆ snapshot)
/// and that every pending field really exists in the snapshot.
pub(crate) fn assert_block_parity(
    sdl: &str,
    snapshot: &str,
    ours_header: &str,
    snapshot_header: &str,
    pending: &[&str],
) -> Result<(), ParityError> {
    let ours = block_fields(sdl, ours_header)?;
    let mut expected = block_fields(snapshot, snapshot_header)?;
    for pending_field in pending {
        assert!(
            expected.remove(*pending_field),
            "pending field `{pending_field}` not found in snapshot block `{snapshot_header}` — stale pending list"
        );
    }
    let missing: Vec<_> = expected.difference(&ours).collect();
    let invented: Vec<_> = ours.difference(&expected).collect();
    assert!(
        missing.is_empty() && invented.is_empty(),
        "block `{snapshot_header}` drift — missing from Rust SDL: {missing:?}; invented (not in snapshot): {invented:?}"
    );
    Ok(())
}

/// Compare a captured block while explicitly allowing fields required by the
/// retained frontend. Intentional extensions remain named and reviewed rather
/// than being mistaken for Go snapshot fields or globally ignored drift.
pub(crate) fn assert_block_parity_with_extensions(
    sdl: &str,
    snapshot: &str,
    ours_header: &str,
    snapshot_header: &str,
    pending: &[&str],
    extensions: &[&str],
) -> Result<(), ParityError> {
    let mut ours = block_fields(sdl, ours_header)?;
    for extension in extensions {
        assert!(
            ours.remove(*extension),
            "frontend extension `{extension}` missing from Rust SDL block `{ours_header}`"
        );
    }
    let mut expected = block_fields(snapshot, snapshot_header)?;
    for pending_field in pending {
        assert!(
            expected.remove(*pending_field),
            "pending field `{pending_field}` not found in snapshot block `{snapshot_header}` — stale pending list"
        );
    }
    let missing: Vec<_> = expected.difference(&ours).collect();
    let invented: Vec<_> = ours.difference(&expected).collect();
    assert!(
        missing.is_empty() && invented.is_empty(),
        "block `{snapshot_header}` drift — missing from Rust SDL: {missing:?}; invented (not approved frontend extensions): {invented:?}"
    );
    Ok(())
}
