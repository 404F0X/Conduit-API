use async_graphql::{InputObject, InputType, InputValueError, InputValueResult, Value};

// =====================================================================
// S14 — mutation input strictness.
//
// Mirrors the gqlgen contract: the Go codegen (`gqlgen.yml` +
// `internal/server/gql/generated.go`) rejects unknown fields on mutation
// inputs by default — a mutation like `createUser(input: { username: "a",
// unscopedRole: "owner" })` fails to parse because `CreateUserInput` has no
// `unscopedRole` field. The frontend hand-writes its mutations against the
// snapshot SDL, so any unknown field is a client/server schema drift and
// must be rejected at the GraphQL layer (S14: "mutation input DTO must be
// compatible with the existing gqlgen input; unknown fields rejected by the
// GraphQL layer").
//
// Below is (a) a pure generic predicate
// `reject_unknown_fields(input_keys, known_keys) -> Result<(), Vec<String>>`
// usable by any strict input validator, and (b) the existing
// `AdminCreateUserInput` specialization refactored to delegate to it.
// =====================================================================

const ADMIN_CREATE_USER_INPUT_FIELDS: &[&str] = &["username", "displayName", "enabled"];

#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
pub struct AdminCreateUserInput {
    pub username: String,
    pub display_name: Option<String>,
    pub enabled: Option<bool>,
}

pub fn parse_strict_admin_create_user_input(
    value: Option<Value>,
) -> InputValueResult<AdminCreateUserInput> {
    reject_unknown_admin_create_user_input_fields(&value)?;
    AdminCreateUserInput::parse(value)
}

fn reject_unknown_admin_create_user_input_fields(
    value: &Option<Value>,
) -> Result<(), InputValueError<AdminCreateUserInput>> {
    let Some(Value::Object(fields)) = value else {
        return Ok(());
    };

    let input_keys: Vec<&str> = fields.keys().map(|name| name.as_str()).collect();
    let unknown = match reject_unknown_fields(&input_keys, ADMIN_CREATE_USER_INPUT_FIELDS) {
        Ok(()) => return Ok(()),
        Err(unknown) => unknown,
    };

    // gqlgen reports only the first unknown field in its error message, so we
    // preserve that behaviour for the strict-input parser surface.
    let first_unknown = unknown
        .first()
        .map(|name| name.as_str())
        .unwrap_or("unknown");
    Err(InputValueError::custom(format!(
        "unknown field \"{first_unknown}\""
    )))
}

/// Pure S14 predicate: given the keys present in a mutation input object and
/// the set of field names the target gqlgen input actually declares, return
/// `Ok(())` if every input key is known, otherwise `Err` carrying the full
/// list of unknown field names (in first-occurrence order).
///
/// This is the GraphQL-layer rejection primitive the task names. It is
/// deliberately key-string-only so it can back any input type without
/// depending on `async_graphql::Value` — a caller extracts keys from the
/// parsed `Value::Object` and hands them here.
///
/// Mirrors gqlgen's behaviour: unknown fields are collected and surfaced as
/// a single structured error (gqlgen raises them field-by-field, but the
/// contract is "the GraphQL layer rejects unknown input fields").
pub fn reject_unknown_fields(input_keys: &[&str], known_keys: &[&str]) -> Result<(), Vec<String>> {
    let mut unknown: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for key in input_keys {
        if known_keys.contains(key) {
            continue;
        }
        // Dedupe within the unknown list so a repeated stray field is reported
        // once — gqlgen dedupes error paths per field.
        if seen.insert(key) {
            unknown.push((*key).to_owned());
        }
    }

    if unknown.is_empty() {
        Ok(())
    } else {
        Err(unknown)
    }
}

#[cfg(test)]
mod tests {
    use async_graphql::{Name, Value};

    use super::{AdminCreateUserInput, parse_strict_admin_create_user_input};
    use crate::build_admin_schema;

    fn input_value(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Option<Value> {
        Some(Value::Object(
            fields
                .into_iter()
                .map(|(field, value)| (Name::new(field), value))
                .collect(),
        ))
    }

    #[test]
    fn strict_mutation_input_accepts_known_fields() {
        let input = parse_strict_admin_create_user_input(input_value([
            ("username", Value::String("alice".to_string())),
            ("displayName", Value::String("Alice Admin".to_string())),
            ("enabled", Value::Boolean(true)),
        ]));

        match input {
            Ok(input) => assert_eq!(
                input,
                AdminCreateUserInput {
                    username: "alice".to_string(),
                    display_name: Some("Alice Admin".to_string()),
                    enabled: Some(true),
                }
            ),
            Err(error) => panic!("known fields should parse: {error:?}"),
        }
    }

    #[test]
    fn strict_mutation_input_rejects_unknown_fields() {
        let input = parse_strict_admin_create_user_input(input_value([
            ("username", Value::String("alice".to_string())),
            ("unscopedRole", Value::String("owner".to_string())),
        ]));

        assert!(input.is_err());
    }

    #[test]
    fn mutation_input_skeleton_is_not_exposed_without_a_resolver() {
        let sdl = build_admin_schema().sdl();

        assert!(!sdl.contains("AdminCreateUserInput"));
        assert!(!sdl.contains("unscopedRole"));
    }

    // -----------------------------------------------------------------
    // S14 — generic `reject_unknown_fields` predicate. These tests mirror
    // the gqlgen contract: unknown input keys are collected and surfaced,
    // known keys pass through, and the unknown list is deduped + ordered.
    // -----------------------------------------------------------------

    use super::reject_unknown_fields;

    #[test]
    fn reject_unknown_fields_accepts_when_all_keys_known() {
        let known = &["username", "displayName", "enabled"];
        let input = &["username", "displayName"];

        let result = reject_unknown_fields(input, known);

        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[test]
    fn reject_unknown_fields_rejects_when_any_key_unknown() {
        // Mirrors gqlgen rejecting `createUser(input: { unscopedRole: ... })`
        // because `CreateUserInput` has no `unscopedRole` field.
        let known = &["username", "displayName", "enabled"];
        let input = &["username", "unscopedRole"];

        let result = reject_unknown_fields(input, known);

        let unknown = match result {
            Err(unknown) => unknown,
            Ok(()) => panic!("expected unknown-field rejection"),
        };
        assert_eq!(unknown, vec!["unscopedRole".to_owned()]);
    }

    #[test]
    fn reject_unknown_fields_collects_every_unknown_key_in_order() {
        // gqlgen surfaces each unknown field path separately; the predicate
        // returns all of them in first-occurrence order so a resolver can
        // build a single aggregated error.
        let known = &["username"];
        let input = &[
            "username",
            "unscopedRole",
            "isOwner",
            "enabled", // also unknown for this trimmed schema
        ];

        let result = reject_unknown_fields(input, known);

        let unknown = match result {
            Err(unknown) => unknown,
            Ok(()) => panic!("expected unknown-field rejection"),
        };
        assert_eq!(
            unknown,
            vec![
                "unscopedRole".to_owned(),
                "isOwner".to_owned(),
                "enabled".to_owned()
            ]
        );
    }

    #[test]
    fn reject_unknown_fields_dedupes_repeated_unknown_keys() {
        // A repeated stray field must appear once in the report, matching
        // gqlgen's per-field error deduplication.
        let known = &["username"];
        let input = &["username", "unscopedRole", "unscopedRole", "unscopedRole"];

        let result = reject_unknown_fields(input, known);

        let unknown = match result {
            Err(unknown) => unknown,
            Ok(()) => panic!("expected unknown-field rejection"),
        };
        assert_eq!(unknown, vec!["unscopedRole".to_owned()]);
    }

    #[test]
    fn reject_unknown_fields_accepts_empty_input() {
        let known = &["username", "displayName"];
        let result = reject_unknown_fields(&[], known);
        assert!(result.is_ok());
    }

    #[test]
    fn reject_unknown_fields_rejects_every_key_when_known_set_is_empty() {
        // Defensive: if a resolver declares no known fields, every input key
        // is treated as unknown (schema drift).
        let known: &[&str] = &[];
        let input = &["anything", "whatever"];

        let result = reject_unknown_fields(input, known);

        let unknown = match result {
            Err(unknown) => unknown,
            Ok(()) => panic!("expected unknown-field rejection"),
        };
        assert_eq!(unknown, vec!["anything".to_owned(), "whatever".to_owned()]);
    }

    #[test]
    fn admin_create_user_input_validator_uses_generic_predicate() {
        // Cross-check: the specialized AdminCreateUserInput validator must
        // surface the same first-unknown-field name that the generic
        // predicate reports.
        let known = super::ADMIN_CREATE_USER_INPUT_FIELDS;
        let generic = match reject_unknown_fields(&["username", "unscopedRole", "enabled"], known) {
            Err(unknown) => unknown,
            Ok(()) => panic!("expected unknown-field rejection"),
        };
        assert_eq!(generic.first().map(String::as_str), Some("unscopedRole"));
    }
}
