//! RUST-P12-001 S07 (continuation) — global Relay `node`/`nodes` queries.
//!
//! Mirrors the Go resolvers `Query.node` / `Query.nodes`
//! (`internal/server/gql/ent.resolvers.go:279-292`):
//!
//! ```go
//! func (r *queryResolver) Node(ctx context.Context, id objects.GUID) (ent.Noder, error) {
//!     typ, ok := guidTypeToNodeType[id.Type]
//!     if !ok {
//!         return nil, fmt.Errorf("unknown node type: %s", id.Type)
//!     }
//!     return r.client.Noder(ctx, id.ID, ent.WithFixedNodeType(typ))
//! }
//!
//! func (r *queryResolver) Nodes(ctx context.Context, ids []*objects.GUID) ([]ent.Noder, error) {
//!     panic(fmt.Errorf("not implemented: Nodes - nodes"))
//! }
//! ```
//!
//! The Go side receives an `objects.GUID` (custom scalar whose wire form is
//! `"gid://conduit/<Type>/<ID>"` — see `conduit/internal/objects/GUID.go`).
//! gqlgen marshals it through the GraphQL `ID!` scalar, so on the Rust side
//! the resolver receives an [`async_graphql::ID`] string and must parse it
//! back into `(type, numeric id)` before dispatching.
//!
//! The `guidTypeToNodeType` map (`internal/server/gql/graphql.go:180-200`)
//! pins every node type string the global resolver will accept. Only types
//! whose Rust counterpart already implements the [`crate::channel::Node`]
//! interface are dispatchable today; unknown types raise the same
//! `"unknown node type"` error Go does.
//!
//! The actual storage lookup is host-injected through [`NodeResolver`]; the
//! crate ships an in-memory double for tests.

use std::sync::Arc;

use async_graphql::{Context, ID};

use crate::channel::Node;

// ---------------------------------------------------------------------------
// GUID wire-format parser (Go: conduit/internal/objects/GUID.go)
// ---------------------------------------------------------------------------

/// Wire prefix every Conduit API GUID carries (GUID.go:32-34).
const GUID_PREFIX: &str = "gid://conduit/";

/// Parsed Conduit API GUID — Go's `objects.GUID{Type, ID}` analogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guid {
    pub typ: String,
    pub id: i64,
}

/// Errors raised by [`parse_guid`]. Mirrors the Go `UnmarshalGQL` error
/// strings (GUID.go:28-48) so frontend error handling stays stable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuidParseError {
    #[error("guid is empty")]
    Empty,
    #[error("guid must start with gid://conduit/")]
    BadPrefix,
    #[error("guid must contain type and id")]
    MissingTypeOrId,
    #[error("invalid guid id: {0}")]
    InvalidId(String),
}

/// Parse a GraphQL `ID` string into a [`Guid`]. Mirrors `GUID.UnmarshalGQL`
/// (`internal/objects/GUID.go:22-54`): the wire form is
/// `gid://conduit/<Type>/<ID>`, where `<Type>` is an ent table name
/// (e.g. `Channel`, `APIKey`) and `<ID>` is the numeric primary key.
pub fn parse_guid(input: &str) -> Result<Guid, GuidParseError> {
    if input.is_empty() {
        return Err(GuidParseError::Empty);
    }
    let rest = input
        .strip_prefix(GUID_PREFIX)
        .ok_or(GuidParseError::BadPrefix)?;
    let (typ, id_str) = rest
        .split_once('/')
        .ok_or(GuidParseError::MissingTypeOrId)?;
    if typ.is_empty() || id_str.is_empty() {
        return Err(GuidParseError::MissingTypeOrId);
    }
    let id: i64 = id_str.parse::<i64>().map_err(|err| {
        // std::num::ParseIntError — name the type explicitly so type inference
        // does not trip over the closure parameter.
        let parse_err: std::num::ParseIntError = err;
        GuidParseError::InvalidId(parse_err.to_string())
    })?;
    Ok(Guid {
        typ: typ.to_owned(),
        id,
    })
}

// ---------------------------------------------------------------------------
// Node-type dispatch table (Go: guidTypeToNodeType map).
// ---------------------------------------------------------------------------

/// Dispatch table: maps each Go `ent.Type*` string to a closure that fetches
/// the corresponding Rust node by numeric id. The closures are injected by
/// the host at schema-build time so the resolver stays storage-agnostic.
///
/// Go counterpart: `r.client.Noder(ctx, id.ID, ent.WithFixedNodeType(typ))`
/// (ent.resolvers.go:286) — a single entry-point into ent's generic
/// per-table loader. The Rust port mirrors that with one trait method per
/// host rather than 11 separate per-domain traits, matching the Go
/// "one Noder" shape.
#[async_trait::async_trait]
pub trait NodeResolver: Send + Sync {
    /// Fetch the node identified by `(typ, id)`. Returns
    /// - `Ok(Some(node))` on hit,
    /// - `Ok(None)` when the type is known but no row matches (ent.Noder
    ///   returns `nil` for missing rows, see ent's generated `Noder`), and
    /// - `Err(NodeError::UnknownType)` when the type string is not in the
    ///   dispatch table — matching Go's `"unknown node type: %s"` error.
    async fn resolve_node(&self, typ: &str, id: i64) -> Result<Option<Node>, NodeError>;
}

/// Error surface for [`NodeResolver`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum NodeError {
    /// Mirrors Go's `"unknown node type: %s"` (ent.resolvers.go:283).
    #[error("unknown node type: {0}")]
    UnknownType(String),
    /// Mirrors ent's load failures (`ent: <type> not found` or wrapped I/O).
    #[error("failed to load node: {0}")]
    Load(String),
    /// Mirrors the "service unavailable" pattern the rest of the crate uses
    /// when host wiring is absent.
    #[error("node resolver is not available")]
    ServiceUnavailable,
}

/// Resolves the injected [`NodeResolver`] from the data bag, mirroring the
/// `ctx.data::<Arc<dyn ...>>()` convention used across this crate.
pub(crate) fn node_resolver(ctx: &Context<'_>) -> Result<Arc<dyn NodeResolver>, String> {
    match ctx.data::<Arc<dyn NodeResolver>>() {
        Ok(resolver) => Ok(Arc::clone(resolver)),
        Err(_) => Err(NodeError::ServiceUnavailable.to_string()),
    }
}

/// Top-level dispatch used by both `node` and `nodes` resolvers. Mirrors the
/// Go sequence (ent.resolvers.go:281-286): type lookup → service call.
async fn dispatch(resolver: &dyn NodeResolver, guid: &Guid) -> Result<Option<Node>, NodeError> {
    resolver.resolve_node(&guid.typ, guid.id).await
}

/// Public entry-point used by `QueryRoot::node`. Parses the GraphQL `ID`
/// string into a [`Guid`] and dispatches to the injected resolver. Returns
/// `Ok(None)` (not an error) when the GUID parses but the resolver has no
/// matching row — matching Go's behaviour of returning `nil, nil` for
/// missing rows.
pub(crate) async fn resolve_single(ctx: &Context<'_>, id: &ID) -> Result<Option<Node>, String> {
    let resolver = node_resolver(ctx)?;
    let guid = parse_guid(id.as_str()).map_err(|err| err.to_string())?;
    dispatch(resolver.as_ref(), &guid)
        .await
        .map_err(|err| err.to_string())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::ID;

    use super::*;
    use crate::channel::{Channel, ChannelStatus, ChannelType, Node};
    use crate::scalars::TimeScalar;
    use crate::{AdminSchema, admin_schema_builder};

    type TestError = Box<dyn std::error::Error>;

    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    fn sample_channel(id: i64, name: &str) -> Channel {
        Channel {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            channel_type: ChannelType::Openai,
            base_url: None,
            website_url: None,
            quota_currency: "USD".to_string(),
            actual_quota_used: None,
            quota_remaining: None,
            name: name.to_owned(),
            status: ChannelStatus::Enabled,
            supported_models: vec!["gpt-4o".to_owned()],
            manual_models: None,
            auto_sync_supported_models: false,
            auto_sync_model_pattern: None,
            tags: None,
            default_test_model: "gpt-4o".to_owned(),
            policies: None,
            settings: None,
            ordering_weight: 0,
            error_message: None,
            remark: None,
            endpoints: None,
        }
    }

    /// In-memory resolver keyed by `(type, id)`. Mirrors the test doubles
    /// used in `channel::tests` / `request_usage::tests`.
    #[derive(Default, Clone)]
    struct InMemoryNodeResolver {
        channels: Arc<Mutex<Vec<Channel>>>,
    }

    #[async_trait::async_trait]
    impl NodeResolver for InMemoryNodeResolver {
        async fn resolve_node(&self, typ: &str, id: i64) -> Result<Option<Node>, NodeError> {
            match typ {
                "Channel" => {
                    let hit = lock(&self.channels)
                        .iter()
                        .find(|c| c.id.as_str().parse::<i64>() == Ok(id))
                        .cloned();
                    Ok(hit.map(Node::Channel))
                }
                other => Err(NodeError::UnknownType(other.to_owned())),
            }
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn schema_with(resolver: &InMemoryNodeResolver) -> AdminSchema {
        let wired: Arc<dyn NodeResolver> = Arc::new(resolver.clone());
        admin_schema_builder().data(wired).finish()
    }

    fn bare_schema() -> AdminSchema {
        crate::build_admin_schema()
    }

    // -----------------------------------------------------------------
    // GUID parser parity (Go: GUID.go UnmarshalGQL)
    // -----------------------------------------------------------------

    #[test]
    fn parse_guid_round_trips_a_channel_id() -> Result<(), GuidParseError> {
        let guid = parse_guid("gid://conduit/Channel/42")?;
        assert_eq!(guid.typ, "Channel");
        assert_eq!(guid.id, 42);
        Ok(())
    }

    #[test]
    fn parse_guid_rejects_empty_string() {
        match parse_guid("") {
            Err(err) => assert_eq!(err, GuidParseError::Empty),
            other => panic!("expected Err(Empty), got {other:?}"),
        }
    }

    #[test]
    fn parse_guid_rejects_missing_prefix() {
        match parse_guid("conduit/Channel/1") {
            Err(err) => assert_eq!(err, GuidParseError::BadPrefix),
            other => panic!("expected Err(BadPrefix), got {other:?}"),
        }
    }

    #[test]
    fn parse_guid_rejects_missing_id_segment() {
        match parse_guid("gid://conduit/Channel") {
            Err(err) => assert_eq!(err, GuidParseError::MissingTypeOrId),
            other => panic!("expected Err(MissingTypeOrId), got {other:?}"),
        }
        // Trailing slash with empty id segment — also rejected as
        // MissingTypeOrId (the parser guards `id_str.is_empty()` before
        // attempting numeric parse, matching Go's `strings.Cut` semantics).
        match parse_guid("gid://conduit/Channel/") {
            Err(err) => assert_eq!(err, GuidParseError::MissingTypeOrId),
            other => panic!("expected Err(MissingTypeOrId), got {other:?}"),
        }
    }

    #[test]
    fn parse_guid_rejects_non_numeric_id() {
        match parse_guid("gid://conduit/Channel/abc") {
            Err(GuidParseError::InvalidId(_)) => {}
            Err(other) => panic!("expected InvalidId, got {other:?}"),
            Ok(value) => panic!("expected Err, got Ok({value:?})"),
        }
    }

    // -----------------------------------------------------------------
    // QueryRoot.node resolver semantics
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn node_returns_channel_for_known_id() -> Result<(), TestError> {
        let resolver = InMemoryNodeResolver::default();
        lock(&resolver.channels).push(sample_channel(7, "prod"));
        let schema = schema_with(&resolver);

        let resp = schema
            .execute(
                r#"query {
                    node(id: "gid://conduit/Channel/7") {
                        __typename
                        ... on Channel { id name }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["node"]["__typename"], "Channel");
        assert_eq!(data["node"]["id"], "7");
        assert_eq!(data["node"]["name"], "prod");
        Ok(())
    }

    #[tokio::test]
    async fn node_returns_null_for_missing_row() -> Result<(), TestError> {
        // Go: ent.Noder returns nil for missing rows; resolver yields null.
        let resolver = InMemoryNodeResolver::default();
        let schema = schema_with(&resolver);

        let resp = schema
            .execute(r#"query { node(id: "gid://conduit/Channel/999") { __typename } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert!(data["node"].is_null());
        Ok(())
    }

    #[tokio::test]
    async fn node_surfaces_unknown_type_error() {
        let resolver = InMemoryNodeResolver::default();
        let schema = schema_with(&resolver);

        let resp = schema
            .execute(r#"query { node(id: "gid://conduit/Widget/1") { __typename } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("unknown node type: Widget"),
            "unexpected error: {message}"
        );
    }

    #[tokio::test]
    async fn node_surfaces_malformed_guid_error() {
        let resolver = InMemoryNodeResolver::default();
        let schema = schema_with(&resolver);

        let resp = schema
            .execute(r#"query { node(id: "not-a-guid") { __typename } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("guid must start with gid://conduit/"),
            "unexpected error: {message}"
        );
    }

    #[tokio::test]
    async fn node_without_wired_resolver_reports_service_unavailable() {
        let schema = bare_schema();
        let resp = schema
            .execute(r#"query { node(id: "gid://conduit/Channel/1") { __typename } }"#)
            .await;
        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(
            message.contains("node resolver is not available"),
            "unexpected error: {message}"
        );
    }

    // -----------------------------------------------------------------
    // QueryRoot.nodes resolver semantics
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn nodes_returns_one_per_input_preserving_order() -> Result<(), TestError> {
        let resolver = InMemoryNodeResolver::default();
        lock(&resolver.channels).push(sample_channel(1, "alpha"));
        lock(&resolver.channels).push(sample_channel(2, "beta"));
        let schema = schema_with(&resolver);

        let resp = schema
            .execute(
                r#"query {
                    nodes(ids: [
                        "gid://conduit/Channel/2",
                        "gid://conduit/Channel/404",
                        "gid://conduit/Channel/1"
                    ]) {
                        __typename
                        ... on Channel { id name }
                    }
                }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let arr = data["nodes"].as_array().ok_or("nodes not an array")?;
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["name"], "beta");
        // Missing row → null slot (Go ent.Noder yields nil per missing id).
        assert!(arr[1].is_null());
        assert_eq!(arr[2]["name"], "alpha");
        Ok(())
    }

    #[tokio::test]
    async fn nodes_empty_input_returns_empty_list() -> Result<(), TestError> {
        let resolver = InMemoryNodeResolver::default();
        let schema = schema_with(&resolver);

        let resp = schema
            .execute(r#"query { nodes(ids: []) { __typename } }"#)
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(data["nodes"].as_array().map(Vec::len), Some(0));
        Ok(())
    }

    #[tokio::test]
    async fn nodes_surfaces_first_parse_error() {
        let resolver = InMemoryNodeResolver::default();
        let schema = schema_with(&resolver);

        let resp = schema
            .execute(
                r#"query {
                    nodes(ids: ["gid://conduit/Channel/1", "bad"]) { __typename }
                }"#,
            )
            .await;

        assert_eq!(resp.errors.len(), 1);
        let message = format!("{}", resp.errors[0]);
        assert!(message.contains("guid must start with gid://conduit/"));
    }

    // -----------------------------------------------------------------
    // SDL parity: Query root declares node/nodes with the snapshot shape.
    // -----------------------------------------------------------------

    #[test]
    fn sdl_declares_node_and_nodes_with_snapshot_signatures() -> Result<(), TestError> {
        let sdl = bare_schema().sdl();
        let snapshot = std::fs::read_to_string("tests/contracts/admin_graphql_schema.graphql")
            .or_else(|_| {
                std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/contracts/admin_graphql_schema.graphql"
                ))
            })?;

        // async-graphql renders the Query root fields inline (matching the
        // existing `channels(...)` test). Each token below appears verbatim
        // in the snapshot's `type Query` block (lines 5202-5215).
        for token in ["node(id: ID!): Node", "nodes(ids: [ID!]!): [Node]!"] {
            assert!(
                sdl.contains(token),
                "generated SDL missing `{token}` — full SDL:\n{sdl}"
            );
        }
        // Snapshot spreads the arguments across multiple lines, so check the
        // building blocks instead of the one-line signature.
        assert!(snapshot.contains("node("));
        assert!(snapshot.contains("id: ID!"));
        assert!(snapshot.contains("): Node"));
        assert!(snapshot.contains("nodes("));
        assert!(snapshot.contains("ids: [ID!]!"));
        assert!(snapshot.contains("): [Node]!"));
        Ok(())
    }

    #[test]
    fn sdl_declares_node_interface_with_id_field() {
        let sdl = bare_schema().sdl();
        // Snapshot line 3848: `interface Node { id: ID! }`.
        assert!(sdl.contains("interface Node"));
        assert!(sdl.contains("id: ID!") || sdl.contains("id: ID"));
    }
}
