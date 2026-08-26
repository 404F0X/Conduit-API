//! SDL contract surface: catalogs + a structural SDL parser used to compare
//! the exported async-graphql SDL against the frozen Go snapshot
//! `tests/contracts/openapi_graphql_schema.graphql` (captured from
//! `internal/server/gql/openapi/openapi.graphql`).
//!
//! gqlgen and async-graphql lay SDL text out differently (`extend type` vs
//! merged roots, ordering, indentation), so a byte diff is meaningless. The
//! parser below reduces both documents to a structural index — scalars, enums
//! (ordered values), object/input types with per-field type refs, argument
//! maps and descriptions — and [`diff_sdl`] reports every discrepancy in both
//! directions. Field names, argument names, type refs (including nullability
//! tokens like `[APIKeyProfileInput!]!`) and enum values are compared
//! verbatim; descriptions are compared whitespace-normalized.

use std::collections::{BTreeMap, BTreeSet};

// =====================================================================
// Structural SDL index
// =====================================================================

/// One field (or root operation) of an object/input type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SdlField {
    /// Return/field type token with whitespace removed, e.g. `[Int!]`,
    /// `APIKey!`.
    pub ty: String,
    /// Argument name → argument type token (whitespace removed).
    pub args: BTreeMap<String, String>,
    /// Attached `"""` description, verbatim (compare via
    /// [`normalize_description`]).
    pub description: Option<String>,
}

/// One object or input type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SdlType {
    pub description: Option<String>,
    pub fields: BTreeMap<String, SdlField>,
}

/// The structural content of an SDL document. `extend type X` blocks are
/// merged into `X`, so gqlgen's `extend type Query` and async-graphql's
/// `type Query` land in the same bucket.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SdlIndex {
    pub scalars: BTreeSet<String>,
    /// Enum name → values in declaration order (order is part of the
    /// contract snapshot).
    pub enums: BTreeMap<String, Vec<String>>,
    pub objects: BTreeMap<String, SdlType>,
    pub inputs: BTreeMap<String, SdlType>,
}

/// Collapse all whitespace runs so reflowed description text still compares
/// equal.
pub fn normalize_description(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// Read a `"""` description starting at `lines[start]` (whose trimmed form
// begins with `"""`). Returns the description text and the index of the first
// line after it. Handles both single-line (`"""text"""`) and block form.
fn take_description(lines: &[&str], start: usize) -> Result<(String, usize), String> {
    let first = lines[start].trim();
    let rest = &first[3..];
    if let Some(single) = rest.strip_suffix("\"\"\"") {
        // Single-line description (only when there IS content between the
        // quotes; a bare `"""` opens a block).
        if !rest.is_empty() {
            return Ok((single.to_string(), start + 1));
        }
    }

    let mut collected: Vec<String> = Vec::new();
    if !rest.is_empty() {
        collected.push(rest.to_string());
    }
    let mut i = start + 1;
    while i < lines.len() {
        let line = lines[i].trim();
        if let Some(before) = line.strip_suffix("\"\"\"") {
            if !before.is_empty() {
                collected.push(before.to_string());
            }
            return Ok((collected.join("\n"), i + 1));
        }
        collected.push(line.to_string());
        i += 1;
    }
    Err("unterminated description block".to_string())
}

// Parse a field line like `apiKey(id: ID, key: String): APIKey!` or
// `key: String!`.
fn parse_field_line(line: &str) -> Result<(String, SdlField), String> {
    let (head, ty) = if let Some(open) = line.find('(') {
        let close = line
            .rfind(')')
            .ok_or_else(|| format!("field `{line}` has an unterminated argument list"))?;
        let name = line[..open].trim().to_string();
        let args_src = &line[open + 1..close];
        let after = line[close + 1..].trim();
        let ty = after
            .strip_prefix(':')
            .ok_or_else(|| format!("field `{line}` is missing a return type"))?
            .trim();

        let mut args = BTreeMap::new();
        for part in args_src.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (arg_name, arg_ty) = part
                .split_once(':')
                .ok_or_else(|| format!("argument `{part}` of `{name}` is malformed"))?;
            args.insert(arg_name.trim().to_string(), arg_ty.trim().replace(' ', ""));
        }
        (
            name,
            SdlField {
                ty: ty.replace(' ', ""),
                args,
                description: None,
            },
        )
    } else {
        let (name, ty) = line
            .split_once(':')
            .ok_or_else(|| format!("field line `{line}` is malformed"))?;
        (
            name.trim().to_string(),
            SdlField {
                ty: ty.trim().replace(' ', ""),
                args: BTreeMap::new(),
                description: None,
            },
        )
    };
    Ok((head, ty))
}

// Parse the fields of a `type`/`input` block; `i` points at the first line
// after the `{`. Returns the fields and the index after the closing `}`.
fn parse_fields(
    lines: &[&str],
    mut i: usize,
) -> Result<(BTreeMap<String, SdlField>, usize), String> {
    let mut fields = BTreeMap::new();
    let mut pending_desc: Option<String> = None;
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with('#') {
            i += 1;
            continue;
        }
        if line == "}" {
            return Ok((fields, i + 1));
        }
        if line.starts_with("\"\"\"") {
            let (desc, next) = take_description(lines, i)?;
            pending_desc = Some(desc);
            i = next;
            continue;
        }
        let (name, mut field) = parse_field_line(line)?;
        field.description = pending_desc.take();
        fields.insert(name, field);
        i += 1;
    }
    Err("unterminated type block".to_string())
}

// Parse an enum body; `i` points after the `{`.
fn parse_enum_values(lines: &[&str], mut i: usize) -> Result<(Vec<String>, usize), String> {
    let mut values = Vec::new();
    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with('#') {
            i += 1;
            continue;
        }
        if line == "}" {
            return Ok((values, i + 1));
        }
        if line.starts_with("\"\"\"") {
            // Value descriptions are not part of this contract; skip them.
            let (_, next) = take_description(lines, i)?;
            i = next;
            continue;
        }
        values.push(line.to_string());
        i += 1;
    }
    Err("unterminated enum block".to_string())
}

// Skip a `{ ... }` block whose header was already consumed (used for the
// trailing `schema { ... }` block async-graphql emits).
fn skip_block(lines: &[&str], mut i: usize) -> usize {
    while i < lines.len() {
        if lines[i].trim() == "}" {
            return i + 1;
        }
        i += 1;
    }
    i
}

// Extract the type name from a block header like `type APIKey {`,
// `extend type Query {` or `input Foo {` (keyword already stripped).
fn header_name(rest: &str) -> Result<String, String> {
    let name = rest.trim().trim_end_matches('{').trim();
    if name.is_empty() || name.contains(' ') {
        return Err(format!("malformed type header `{rest}`"));
    }
    Ok(name.to_string())
}

/// Parse an SDL document (the subset used by the OpenAPI schema: scalars,
/// enums, object/input types, `extend type`, descriptions, `schema` block,
/// single-line `directive` definitions, `#` comments).
pub fn parse_sdl(text: &str) -> Result<SdlIndex, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut idx = SdlIndex::default();
    let mut pending_desc: Option<String> = None;
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();
        if line.is_empty() || line.starts_with('#') {
            i += 1;
            continue;
        }
        if line.starts_with("\"\"\"") {
            let (desc, next) = take_description(&lines, i)?;
            pending_desc = Some(desc);
            i = next;
            continue;
        }
        if let Some(rest) = line.strip_prefix("scalar ") {
            idx.scalars.insert(rest.trim().to_string());
            pending_desc = None;
            i += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix("enum ") {
            let name = header_name(rest)?;
            let (values, next) = parse_enum_values(&lines, i + 1)?;
            idx.enums.insert(name, values);
            pending_desc = None;
            i = next;
            continue;
        }
        // `extend type X` merges into `X` — gqlgen's snapshot extends the
        // Query/Mutation roots while async-graphql declares them directly.
        let type_rest = line
            .strip_prefix("extend type ")
            .or_else(|| line.strip_prefix("type "));
        if let Some(rest) = type_rest {
            let name = header_name(rest)?;
            let (fields, next) = parse_fields(&lines, i + 1)?;
            let entry = idx.objects.entry(name).or_default();
            if entry.description.is_none() {
                entry.description = pending_desc.take();
            }
            entry.fields.extend(fields);
            pending_desc = None;
            i = next;
            continue;
        }
        if let Some(rest) = line.strip_prefix("input ") {
            let name = header_name(rest)?;
            let (fields, next) = parse_fields(&lines, i + 1)?;
            let entry = idx.inputs.entry(name).or_default();
            if entry.description.is_none() {
                entry.description = pending_desc.take();
            }
            entry.fields.extend(fields);
            pending_desc = None;
            i = next;
            continue;
        }
        if line.starts_with("schema") {
            i = skip_block(&lines, i + 1);
            pending_desc = None;
            continue;
        }
        if line.starts_with("directive") {
            // async-graphql renders directive definitions single-line.
            pending_desc = None;
            i += 1;
            continue;
        }
        return Err(format!("unrecognized SDL construct: `{line}`"));
    }

    Ok(idx)
}

// =====================================================================
// Structural diff
// =====================================================================

fn diff_key_sets<T>(
    kind: &str,
    expected: &BTreeMap<String, T>,
    actual: &BTreeMap<String, T>,
    diffs: &mut Vec<String>,
) {
    for name in expected.keys() {
        if !actual.contains_key(name) {
            diffs.push(format!("missing {kind} `{name}` (present in snapshot)"));
        }
    }
    for name in actual.keys() {
        if !expected.contains_key(name) {
            diffs.push(format!("extra {kind} `{name}` (absent from snapshot)"));
        }
    }
}

fn diff_descriptions(
    context: &str,
    expected: Option<&String>,
    actual: Option<&String>,
    diffs: &mut Vec<String>,
) {
    let expected = expected.map(|d| normalize_description(d));
    let actual = actual.map(|d| normalize_description(d));
    if expected != actual {
        diffs.push(format!(
            "{context}: description mismatch (snapshot: {expected:?}, exported: {actual:?})"
        ));
    }
}

fn diff_type_maps(
    kind: &str,
    expected: &BTreeMap<String, SdlType>,
    actual: &BTreeMap<String, SdlType>,
    diffs: &mut Vec<String>,
) {
    diff_key_sets(kind, expected, actual, diffs);
    for (name, exp_ty) in expected {
        let Some(act_ty) = actual.get(name) else {
            continue;
        };
        diff_descriptions(
            &format!("{kind} `{name}`"),
            exp_ty.description.as_ref(),
            act_ty.description.as_ref(),
            diffs,
        );

        for field in exp_ty.fields.keys() {
            if !act_ty.fields.contains_key(field) {
                diffs.push(format!("{kind} `{name}`: missing field `{field}`"));
            }
        }
        for field in act_ty.fields.keys() {
            if !exp_ty.fields.contains_key(field) {
                diffs.push(format!("{kind} `{name}`: extra field `{field}`"));
            }
        }
        for (field, exp_field) in &exp_ty.fields {
            let Some(act_field) = act_ty.fields.get(field) else {
                continue;
            };
            if exp_field.ty != act_field.ty {
                diffs.push(format!(
                    "{kind} `{name}`.`{field}`: type mismatch (snapshot `{}`, exported `{}`)",
                    exp_field.ty, act_field.ty
                ));
            }
            if exp_field.args != act_field.args {
                diffs.push(format!(
                    "{kind} `{name}`.`{field}`: argument mismatch (snapshot {:?}, exported {:?})",
                    exp_field.args, act_field.args
                ));
            }
            diff_descriptions(
                &format!("{kind} `{name}`.`{field}`"),
                exp_field.description.as_ref(),
                act_field.description.as_ref(),
                diffs,
            );
        }
    }
}

/// Compare two structural indexes; returns one human-readable line per
/// discrepancy (empty = contract holds). Symmetric: missing AND extra
/// constructs are both reported.
pub fn diff_sdl(expected: &SdlIndex, actual: &SdlIndex) -> Vec<String> {
    let mut diffs = Vec::new();

    for scalar in &expected.scalars {
        if !actual.scalars.contains(scalar) {
            diffs.push(format!("missing scalar `{scalar}` (present in snapshot)"));
        }
    }
    for scalar in &actual.scalars {
        if !expected.scalars.contains(scalar) {
            diffs.push(format!("extra scalar `{scalar}` (absent from snapshot)"));
        }
    }

    for name in expected.enums.keys() {
        if !actual.enums.contains_key(name) {
            diffs.push(format!("missing enum `{name}` (present in snapshot)"));
        }
    }
    for name in actual.enums.keys() {
        if !expected.enums.contains_key(name) {
            diffs.push(format!("extra enum `{name}` (absent from snapshot)"));
        }
    }
    for (name, exp_values) in &expected.enums {
        if let Some(act_values) = actual.enums.get(name)
            && exp_values != act_values
        {
            diffs.push(format!(
                "enum `{name}`: values mismatch (snapshot {exp_values:?}, exported {act_values:?})"
            ));
        }
    }

    diff_type_maps("type", &expected.objects, &actual.objects, &mut diffs);
    diff_type_maps("input", &expected.inputs, &actual.inputs, &mut diffs);

    diffs
}

// =====================================================================
// Contract catalogs (kept from the earlier skeleton — they double as an
// at-a-glance operation coverage table).
// =====================================================================

/// Object type names that MUST appear in any conformant OpenAPI SDL, mirrored
/// from `tests/contracts/openapi_graphql_schema.graphql`.
pub const OPENAPI_SDL_EXPECTED_OBJECT_TYPES: &[&str] = &[
    "APIKey",
    "APIKeyProfiles",
    "APIKeyProfile",
    "APIKeyQuota",
    "APIKeyQuotaPeriod",
    "APIKeyQuotaPastDuration",
    "APIKeyQuotaCalendarDuration",
    "ModelMapping",
    "APIKeyProfileQuotaUsage",
    "APIKeyQuotaUsage",
    "APIKeyQuotaWindow",
];

/// Enum type names that MUST appear in any conformant OpenAPI SDL.
pub const OPENAPI_SDL_EXPECTED_ENUM_TYPES: &[&str] = &[
    "ChannelTagsMatchMode",
    "APIKeyQuotaPeriodType",
    "APIKeyQuotaPastDurationUnit",
    "APIKeyQuotaCalendarDurationUnit",
];

/// Input type names that MUST appear in any conformant OpenAPI SDL.
pub const OPENAPI_SDL_EXPECTED_INPUT_TYPES: &[&str] = &[
    "UpdateAPIKeyProfilesInput",
    "APIKeyProfileInput",
    "APIKeyQuotaInput",
    "APIKeyQuotaPeriodInput",
    "APIKeyQuotaPastDurationInput",
    "APIKeyQuotaCalendarDurationInput",
    "ModelMappingInput",
    "LoadApiKeyProfileTemplateInput",
];

/// Query root fields of the Go OpenAPI SDL (`extend type Query`).
pub const OPENAPI_SDL_EXPECTED_QUERY_FIELDS: &[&str] = &["apiKey", "apiKeyQuotaUsages"];

/// Mutation root fields of the Go OpenAPI SDL (`extend type Mutation`).
pub const OPENAPI_SDL_EXPECTED_MUTATION_FIELDS: &[&str] = &[
    "createLLMAPIKey",
    "updateAPIKeyProfiles",
    "loadApiKeyProfileTemplate",
];

/// Back-compat alias kept so earlier callers (`OPENAPI_SDL_EXPECTED_TYPES`)
/// keep compiling. New code should prefer the kind-specific catalogs above.
pub const OPENAPI_SDL_EXPECTED_TYPES: &[&str] = OPENAPI_SDL_EXPECTED_OBJECT_TYPES;

/// Root fields/types that MUST NOT appear on the OpenAPI surface — they are
/// admin-only (admin GraphQL, JWT-gated). The exclusion is asserted against
/// the PARSED Query/Mutation root fields (not raw substrings — `requests` is
/// a legitimate `APIKeyQuota` field but must never be a query root).
pub const OPENAPI_SDL_ADMIN_ONLY_FIELDS: &[&str] = &[
    // Admin-only mutations (admin graphql surface).
    "createUser",
    "deleteUser",
    "updateUser",
    "createChannel",
    "deleteChannel",
    "createProject",
    "deleteProject",
    // Admin-only query roots.
    "users",
    "channels",
    "projects",
    "requests",
    // System / maintenance surfaces.
    "systemSettings",
    "auditLogs",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_text() -> Result<String, Box<dyn std::error::Error>> {
        // The snapshot lives at the workspace root; fall back to the
        // manifest-relative path (same pattern as conduit-admin-graphql).
        std::fs::read_to_string("tests/contracts/openapi_graphql_schema.graphql")
            .or_else(|_| {
                std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/contracts/openapi_graphql_schema.graphql"
                ))
            })
            .map_err(|err| {
                format!(
                    "could not read snapshot tests/contracts/openapi_graphql_schema.graphql: {err}"
                )
                .into()
            })
    }

    fn exported_sdl() -> String {
        let env = crate::memory::fixture(&[]);
        crate::build_openapi_schema(env.services.clone()).sdl()
    }

    // -------------------------------------------------------------------
    // Parser unit tests.
    // -------------------------------------------------------------------

    #[test]
    fn parser_handles_fields_args_and_descriptions() -> Result<(), Box<dyn std::error::Error>> {
        let sdl = "\
scalar Time

enum Mode {
  any
  all
}

\"\"\"
Type doc.
\"\"\"
type Query {
  \"\"\"
  Field doc
  spanning lines.
  \"\"\"
  apiKey(id: ID, key: String): APIKey!
  plain: [Int!]
}

extend type Query {
  merged(input: Foo!): String
}
";
        let idx = parse_sdl(sdl)?;
        assert!(idx.scalars.contains("Time"));
        assert_eq!(
            idx.enums.get("Mode"),
            Some(&vec!["any".to_string(), "all".to_string()])
        );

        let query = idx.objects.get("Query").ok_or("Query missing")?;
        assert_eq!(
            query.description.as_deref().map(normalize_description),
            Some("Type doc.".to_string())
        );
        let api_key = query.fields.get("apiKey").ok_or("apiKey missing")?;
        assert_eq!(api_key.ty, "APIKey!");
        assert_eq!(api_key.args.get("id").map(String::as_str), Some("ID"));
        assert_eq!(api_key.args.get("key").map(String::as_str), Some("String"));
        assert_eq!(
            api_key.description.as_deref().map(normalize_description),
            Some("Field doc spanning lines.".to_string())
        );
        let plain = query.fields.get("plain").ok_or("plain missing")?;
        assert_eq!(plain.ty, "[Int!]");
        // extend type merged into the same bucket.
        assert!(query.fields.contains_key("merged"));
        Ok(())
    }

    #[test]
    fn diff_reports_both_directions() -> Result<(), Box<dyn std::error::Error>> {
        let a = parse_sdl("type T {\n  x: Int!\n}\n")?;
        let b = parse_sdl("type T {\n  x: Int\n  y: String\n}\n")?;
        let diffs = diff_sdl(&a, &b);
        assert!(diffs.iter().any(|d| d.contains("type mismatch")));
        assert!(diffs.iter().any(|d| d.contains("extra field `y`")));
        Ok(())
    }

    // -------------------------------------------------------------------
    // The load-bearing contract test: exported SDL vs snapshot, per
    // type/field/arg/enum-value/description.
    // -------------------------------------------------------------------

    #[test]
    fn exported_sdl_matches_go_snapshot_structurally() -> Result<(), Box<dyn std::error::Error>> {
        let expected = parse_sdl(&snapshot_text()?)?;
        let actual = parse_sdl(&exported_sdl())?;

        let diffs = diff_sdl(&expected, &actual);
        assert!(
            diffs.is_empty(),
            "OpenAPI SDL drifted from the Go snapshot:\n{}",
            diffs.join("\n")
        );
        Ok(())
    }

    /// Parser + snapshot sanity: pin the construct counts of the frozen
    /// snapshot (3 scalars, 4 enums, 11 object types + Query + Mutation,
    /// 8 inputs) so a truncated snapshot or a broken parser fails loudly.
    #[test]
    fn snapshot_has_expected_construct_counts() -> Result<(), Box<dyn std::error::Error>> {
        let idx = parse_sdl(&snapshot_text()?)?;
        assert_eq!(idx.scalars.len(), 3, "scalars: {:?}", idx.scalars);
        assert_eq!(idx.enums.len(), 4, "enums: {:?}", idx.enums.keys());
        assert_eq!(
            idx.objects.len(),
            13,
            "objects (11 + Query + Mutation): {:?}",
            idx.objects.keys()
        );
        assert_eq!(idx.inputs.len(), 8, "inputs: {:?}", idx.inputs.keys());
        Ok(())
    }

    /// Root-surface pin: the exported Query/Mutation expose exactly the
    /// snapshot's operations and none of the admin-only roots (S11-style
    /// exclusion, evaluated on PARSED root fields — substring matching would
    /// false-positive on `APIKeyQuota.requests`).
    #[test]
    fn exported_roots_match_catalog_and_exclude_admin_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let actual = parse_sdl(&exported_sdl())?;

        let query_fields: Vec<&str> = actual
            .objects
            .get("Query")
            .ok_or("Query root missing")?
            .fields
            .keys()
            .map(String::as_str)
            .collect();
        let mutation_fields: Vec<&str> = actual
            .objects
            .get("Mutation")
            .ok_or("Mutation root missing")?
            .fields
            .keys()
            .map(String::as_str)
            .collect();

        // BTreeMap keys are sorted; compare as sets against the catalogs.
        let mut expected_query = OPENAPI_SDL_EXPECTED_QUERY_FIELDS.to_vec();
        expected_query.sort_unstable();
        assert_eq!(query_fields, expected_query);

        let mut expected_mutation = OPENAPI_SDL_EXPECTED_MUTATION_FIELDS.to_vec();
        expected_mutation.sort_unstable();
        assert_eq!(mutation_fields, expected_mutation);

        for banned in OPENAPI_SDL_ADMIN_ONLY_FIELDS {
            assert!(
                !query_fields.contains(banned) && !mutation_fields.contains(banned),
                "admin-only root `{banned}` leaked onto the OpenAPI surface"
            );
        }
        Ok(())
    }

    // -------------------------------------------------------------------
    // Catalog self-consistency (carried over from the earlier skeleton).
    // -------------------------------------------------------------------

    /// Object-type catalog mirrors the 11 Go SDL object types verbatim.
    #[test]
    fn object_type_catalog_matches_go_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(OPENAPI_SDL_EXPECTED_OBJECT_TYPES.len(), 11);
        let idx = parse_sdl(&snapshot_text()?)?;
        for required in OPENAPI_SDL_EXPECTED_OBJECT_TYPES {
            assert!(
                idx.objects.contains_key(*required),
                "`{required}` is not an object type of the snapshot"
            );
        }
        Ok(())
    }

    /// Enum-type catalog mirrors the 4 Go SDL enums verbatim.
    #[test]
    fn enum_type_catalog_matches_go_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(OPENAPI_SDL_EXPECTED_ENUM_TYPES.len(), 4);
        let idx = parse_sdl(&snapshot_text()?)?;
        for required in OPENAPI_SDL_EXPECTED_ENUM_TYPES {
            assert!(
                idx.enums.contains_key(*required),
                "`{required}` is not an enum of the snapshot"
            );
        }
        Ok(())
    }

    /// Input-type catalog mirrors the 8 Go SDL inputs verbatim.
    #[test]
    fn input_type_catalog_matches_go_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(OPENAPI_SDL_EXPECTED_INPUT_TYPES.len(), 8);
        let idx = parse_sdl(&snapshot_text()?)?;
        for required in OPENAPI_SDL_EXPECTED_INPUT_TYPES {
            assert!(
                idx.inputs.contains_key(*required),
                "`{required}` is not an input type of the snapshot"
            );
        }
        Ok(())
    }

    /// Back-compat alias still exposes the 11 object types.
    #[test]
    fn expected_types_alias_points_at_object_catalog() {
        assert_eq!(
            OPENAPI_SDL_EXPECTED_TYPES,
            OPENAPI_SDL_EXPECTED_OBJECT_TYPES
        );
    }

    /// Query/Mutation field catalogs mirror the snapshot's extend blocks, and
    /// the mutation catalog stays disjoint from the admin-only exclusion list.
    #[test]
    fn root_field_catalogs_match_go_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let idx = parse_sdl(&snapshot_text()?)?;

        let query = idx.objects.get("Query").ok_or("snapshot Query missing")?;
        for field in OPENAPI_SDL_EXPECTED_QUERY_FIELDS {
            assert!(query.fields.contains_key(*field));
        }
        assert_eq!(query.fields.len(), OPENAPI_SDL_EXPECTED_QUERY_FIELDS.len());

        let mutation = idx
            .objects
            .get("Mutation")
            .ok_or("snapshot Mutation missing")?;
        for field in OPENAPI_SDL_EXPECTED_MUTATION_FIELDS {
            assert!(mutation.fields.contains_key(*field));
        }
        assert_eq!(
            mutation.fields.len(),
            OPENAPI_SDL_EXPECTED_MUTATION_FIELDS.len()
        );

        for allowed in OPENAPI_SDL_EXPECTED_MUTATION_FIELDS {
            assert!(
                !OPENAPI_SDL_ADMIN_ONLY_FIELDS.contains(allowed),
                "OpenAPI mutation `{allowed}` is also listed as admin-only — catalog contradiction"
            );
        }
        Ok(())
    }

    /// The admin-only exclusion catalog covers the high-risk admin mutations
    /// and query roots.
    #[test]
    fn admin_only_catalog_covers_users_channels_projects() {
        for required in &[
            "createUser",
            "deleteUser",
            "channels",
            "projects",
            "users",
            "requests",
        ] {
            assert!(
                OPENAPI_SDL_ADMIN_ONLY_FIELDS.contains(required),
                "`{required}` must be listed in OPENAPI_SDL_ADMIN_ONLY_FIELDS"
            );
        }
    }
}
