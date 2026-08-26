//! RUST-P12-001 S04 + S10 — admin SDL render smoke.
//!
//! Composes an SDL string for the admin schema by gluing together the typed
//! blocks already produced by sibling modules:
//!   - scalar newtypes from [`crate::scalars`] (`Time`, `Map`, `Any`,
//!     `JSONRawMessage`, `Decimal`, `Cursor`) — we only need the GraphQL names,
//!     which are statically known;
//!   - the system-settings enum blocks from [`crate::scalars`] (the
//!     `*_SDL_BLOCK` constants) and [`crate::query::OrderDirection`];
//!   - the Relay `PageInfo` / `Connection` / `Edge` shape from
//!     [`crate::pagination`].
//!
//! Design rule (per the RUST-P12-001 brief): **string composition over the
//! typed `as_str()` / `*_SDL_BLOCK` constants** — no hand-written giant literal
//! of the enum variants. The result is intentionally NOT byte-for-byte equal
//! to the captured snapshot; S10 only requires that it carries the
//! contract-critical identifiers.

use crate::query::OrderDirection;
use crate::scalars::{
    AUTO_SYNC_FREQUENCY_SDL_BLOCK, PROBE_FREQUENCY_SDL_BLOCK, QUOTA_ENFORCEMENT_MODE_SDL_BLOCK,
};

/// Scalar names declared by the admin schema. Sourced from
/// `tests/contracts/admin_graphql_schema.graphql` (lines 6-9, 952, 3319, 3584,
/// 7108, 8808) and mirrored one-for-one by the `#[Scalar(name = "...")]`
/// newtypes in [`crate::scalars`].
const SCALAR_NAMES: &[&str] = &[
    "JSONRawMessage",
    "JSONRawMessageInput",
    "Decimal",
    "DecimalInput",
    "Upload",
    "Cursor",
    "Map",
    "Time",
    "Any",
];

/// The major admin domain areas, each surfaced as one root `Query` field. The
/// field name and return type mirror the Go snapshot's `type Query` block
/// exactly (`tests/contracts/admin_graphql_schema.graphql` line 5198+): every
/// connection field returns a single `<Type>Connection!` object whose `edges`
/// list is the array — Go does NOT wrap connections in `[...]!`. The six
/// entries below are the connection-typed root fields; `node`/`nodes` and the
/// dashboard-stats fields live elsewhere in the snapshot and are out of scope
/// for this smoke. Field shapes are intentionally generic placeholders — the
/// SDL smoke only needs to confirm the areas exist as queryable roots with the
/// correct single-object connection return type; per-area resolvers land in
/// later task batches.
const QUERY_AREAS: &[(&str, &str)] = &[
    ("apiKeys", "APIKeyConnection!"),
    ("channels", "ChannelConnection!"),
    ("models", "ModelConnection!"),
    ("users", "UserConnection!"),
    ("projects", "ProjectConnection!"),
    ("requests", "RequestConnection!"),
];

/// The literal SDL block for `enum OrderDirection`, derived from
/// [`OrderDirection::as_graphql`] rather than re-typed. The two variants are
/// enumerated inline (Go `entgql.OrderDirection` only ever defines `ASC`/`DESC`).
fn order_direction_sdl_block() -> String {
    let mut block = String::from("enum OrderDirection {\n");
    for variant in [OrderDirection::Asc, OrderDirection::Desc] {
        block.push_str(&format!("  {}\n", variant.as_graphql()));
    }
    block.push('}');
    block
}

/// Renders a synthetic admin GraphQL SDL string by composing the typed scalar
/// list, enum blocks, the canonical Relay `PageInfo` / `Connection` / `Edge`
/// shape, and a `Query` root with one field per major admin area.
///
/// This is a pure function: no I/O, no schema build. The output is a
/// contract-shaped *subset* of the captured snapshot
/// (`tests/contracts/admin_graphql_schema.graphql`) — sufficient for the S10
/// smoke which only checks identifier presence, not byte-for-byte equality.
pub fn render_admin_sdl() -> String {
    let mut sdl = String::new();

    // ---- header ------------------------------------------------------
    sdl.push_str("# Conduit API admin schema (RUST-P12-001 S04 synthetic SDL).\n");
    sdl.push_str("# Contract source-of-truth: tests/contracts/admin_graphql_schema.graphql.\n\n");

    // ---- scalars -----------------------------------------------------
    for name in SCALAR_NAMES {
        let _ = name; // suppress unused-var lint when list is empty.
        sdl.push_str(&format!("scalar {name}\n"));
    }
    sdl.push('\n');

    // ---- enums -------------------------------------------------------
    sdl.push_str(QUOTA_ENFORCEMENT_MODE_SDL_BLOCK);
    sdl.push_str("\n\n");
    sdl.push_str(AUTO_SYNC_FREQUENCY_SDL_BLOCK);
    sdl.push_str("\n\n");
    sdl.push_str(PROBE_FREQUENCY_SDL_BLOCK);
    sdl.push_str("\n\n");
    sdl.push_str(&order_direction_sdl_block());
    sdl.push_str("\n\n");

    // ---- Relay connection shape (generic placeholder) ----------------
    sdl.push_str(
        "\
type PageInfo {
  hasNextPage: Boolean!
  hasPreviousPage: Boolean!
  startCursor: Cursor
  endCursor: Cursor
}

type Edge {
  cursor: Cursor!
  node: Any
}

type Connection {
  edges: [Edge!]!
  pageInfo: PageInfo!
}
",
    );
    sdl.push('\n');

    // ---- Query root --------------------------------------------------
    sdl.push_str("type Query {\n");
    // Health/version probes (parity with `lib.rs::QueryRoot`).
    sdl.push_str("  health: String!\n");
    sdl.push_str("  version: String!\n");
    for (field_name, field_type) in QUERY_AREAS {
        sdl.push_str(&format!("  {field_name}: {field_type}\n"));
    }
    sdl.push_str("}\n");

    sdl
}

// =====================================================================
// S10 — snapshot identifier smoke tests.
//
// These are substring (NOT byte-for-byte) assertions: the rendered SDL must
// carry the contract-critical identifiers — the enum literals, scalar names,
// `PageInfo`, `Connection`, `hasNextPage` — so the frontend's introspection
// query surface stays stable across refactors of this crate.
// =====================================================================

#[cfg(test)]
mod tests {
    use super::render_admin_sdl;

    /// Snapshot-driven identifier list helper: reads the canonical snapshot
    /// file from the workspace root (cargo's CWD for this crate) and falls
    /// back to the manifest-dir-relative path used elsewhere in the crate.
    fn snapshot_text() -> Result<String, Box<dyn std::error::Error>> {
        std::fs::read_to_string("tests/contracts/admin_graphql_schema.graphql")
            .or_else(|_| {
                std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/contracts/admin_graphql_schema.graphql"
                ))
            })
            .map_err(|err| {
                format!(
                    "could not read snapshot tests/contracts/admin_graphql_schema.graphql: {err}"
                )
                .into()
            })
    }

    #[test]
    fn s10_render_carries_all_admin_scalar_names() {
        let sdl = render_admin_sdl();
        for scalar in [
            "JSONRawMessage",
            "JSONRawMessageInput",
            "Decimal",
            "DecimalInput",
            "Upload",
            "Cursor",
            "Map",
            "Time",
            "Any",
        ] {
            assert!(
                sdl.contains(&format!("scalar {scalar}")),
                "rendered SDL is missing scalar `{scalar}`"
            );
        }
    }

    #[test]
    fn s10_render_carries_system_settings_enum_literals() {
        let sdl = render_admin_sdl();
        // QuotaEnforcementMode
        assert!(sdl.contains("enum QuotaEnforcementMode"));
        assert!(sdl.contains("EXHAUSTED_ONLY"));
        assert!(sdl.contains("DE_PRIORITIZE"));
        // AutoSyncFrequency
        assert!(sdl.contains("enum AutoSyncFrequency"));
        assert!(sdl.contains("ONE_HOUR"));
        assert!(sdl.contains("SIX_HOURS"));
        assert!(sdl.contains("ONE_DAY"));
        // ProbeFrequency
        assert!(sdl.contains("enum ProbeFrequency"));
        assert!(sdl.contains("ONE_MINUTE"));
        assert!(sdl.contains("FIVE_MINUTES"));
        assert!(sdl.contains("THIRTY_MINUTES"));
    }

    #[test]
    fn s10_render_carries_order_direction_enum() {
        let sdl = render_admin_sdl();
        assert!(sdl.contains("enum OrderDirection"));
        assert!(sdl.contains("ASC"));
        assert!(sdl.contains("DESC"));
    }

    #[test]
    fn s10_render_carries_relay_page_info_and_connection_shape() {
        let sdl = render_admin_sdl();
        assert!(sdl.contains("type PageInfo"));
        assert!(sdl.contains("hasNextPage: Boolean!"));
        assert!(sdl.contains("hasPreviousPage: Boolean!"));
        assert!(sdl.contains("startCursor: Cursor"));
        assert!(sdl.contains("endCursor: Cursor"));
        assert!(sdl.contains("type Connection"));
        assert!(sdl.contains("edges: [Edge!]!"));
        assert!(sdl.contains("pageInfo: PageInfo!"));
        assert!(sdl.contains("type Edge"));
        assert!(sdl.contains("cursor: Cursor!"));
    }

    #[test]
    fn s10_render_carries_one_query_field_per_major_area() {
        let sdl = render_admin_sdl();
        assert!(sdl.contains("type Query {"));
        for area in [
            "channels", "models", "apiKeys", "users", "projects", "requests",
        ] {
            assert!(
                sdl.contains(&format!("{area}:")),
                "rendered Query root is missing the `{area}` field"
            );
        }
        // Parity guard: Go does NOT declare a `dashboard` field on `Query`, and
        // no `Dashboard` type exists in the snapshot. The renderer must not
        // emit one either.
        assert!(
            !sdl.contains("dashboard:"),
            "rendered SDL must not declare a fabricated `dashboard` field (no such root in Go)"
        );
    }

    /// Parity guard: every connection-typed Query root field must use the
    /// single-object `<Type>Connection!` form that Go's snapshot declares, NOT
    /// a `[<Type>Connection!]!` list wrapper. This catches the regression where
    /// the renderer was earlier emitting list-wrapped connection types.
    #[test]
    fn s10_render_uses_single_object_connection_return_types() {
        let sdl = render_admin_sdl();
        for (field, type_name) in [
            ("apiKeys", "APIKeyConnection"),
            ("channels", "ChannelConnection"),
            ("models", "ModelConnection"),
            ("users", "UserConnection"),
            ("projects", "ProjectConnection"),
            ("requests", "RequestConnection"),
        ] {
            assert!(
                sdl.contains(&format!("{field}: {type_name}!")),
                "rendered SDL must declare `{field}: {type_name}!` (single-object form, matching Go)"
            );
            assert!(
                !sdl.contains(&format!("{field}: [{type_name}!")),
                "rendered SDL must NOT list-wrap `{field}` (`[{type_name}!...]` is not the Go shape)"
            );
        }
    }

    /// Cross-check: every literal the rendered SDL claims as a system-settings
    /// enum value must also appear verbatim in the canonical snapshot. This is
    /// the parity guard that prevents us from drifting away from the contract.
    #[test]
    fn s10_rendered_enum_literals_exist_in_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = snapshot_text()?;
        for literal in [
            "EXHAUSTED_ONLY",
            "DE_PRIORITIZE",
            "ONE_HOUR",
            "SIX_HOURS",
            "ONE_DAY",
            "ONE_MINUTE",
            "FIVE_MINUTES",
            "THIRTY_MINUTES",
            "ASC",
            "DESC",
        ] {
            assert!(
                snapshot.contains(literal),
                "snapshot is missing enum literal `{literal}` that the renderer emits"
            );
        }
        Ok(())
    }

    /// Cross-check: every scalar name the renderer emits must appear as a
    /// `scalar <name>` line in the snapshot.
    #[test]
    fn s10_rendered_scalar_names_exist_in_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = snapshot_text()?;
        for scalar in [
            "JSONRawMessage",
            "JSONRawMessageInput",
            "Decimal",
            "DecimalInput",
            "Upload",
            "Cursor",
            "Map",
            "Time",
            "Any",
        ] {
            assert!(
                snapshot.contains(&format!("scalar {scalar}")),
                "snapshot is missing scalar declaration `{scalar}`"
            );
        }
        Ok(())
    }

    /// Cross-check: the snapshot declares the `PageInfo` type and
    /// `hasNextPage` field that the renderer emits for the Relay shape.
    #[test]
    fn s10_rendered_relay_identifiers_exist_in_snapshot() -> Result<(), Box<dyn std::error::Error>>
    {
        let snapshot = snapshot_text()?;
        assert!(
            snapshot.contains("type PageInfo"),
            "snapshot missing `type PageInfo`"
        );
        assert!(
            snapshot.contains("hasNextPage"),
            "snapshot missing `hasNextPage` field"
        );
        Ok(())
    }

    /// Cross-check: every connection-typed Query root field the renderer emits
    /// must also appear in the canonical snapshot's `type Query` block with the
    /// same single-object return type. This pins the SDL smoke to the actual Go
    /// contract rather than a fabricated shape.
    #[test]
    fn s10_rendered_query_connection_fields_exist_in_snapshot()
    -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = snapshot_text()?;
        for (field, type_name) in [
            ("apiKeys", "APIKeyConnection"),
            ("channels", "ChannelConnection"),
            ("models", "ModelConnection"),
            ("users", "UserConnection"),
            ("projects", "ProjectConnection"),
            ("requests", "RequestConnection"),
        ] {
            // The snapshot's Query block declares each root field with a
            // signature like `  apiKeys(\n ... \n  ): APIKeyConnection!`.
            // Asserting the field-name-with-paren and the return-type token is
            // sufficient to confirm parity without parsing the full signature.
            assert!(
                snapshot.contains(&format!("{field}(")),
                "snapshot `Query` block is missing the `{field}(` root field"
            );
            assert!(
                snapshot.contains(&format!("{type_name}!")),
                "snapshot is missing return type `{type_name}!`"
            );
        }
        // Negative guard: the Go snapshot has no `dashboard` Query root and no
        // `Dashboard` type. The renderer must not fabricate one.
        assert!(
            !snapshot.contains("dashboard("),
            "snapshot must not declare a fabricated `dashboard(` Query root"
        );
        assert!(
            !snapshot.contains("type Dashboard "),
            "snapshot must not declare a fabricated `type Dashboard`"
        );
        Ok(())
    }
}
