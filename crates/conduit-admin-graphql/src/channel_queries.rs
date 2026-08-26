//! GAP-03 — channel list-page Query root fields (extended channel queries).
//!
//! The channels list page's first paint calls four root queries that are NOT
//! covered by the ent-generated `channels` connection slice (see
//! `crate::channel`). All shapes are copied field-for-field from the captured
//! contract snapshot `tests/contracts/admin_graphql_schema.graphql`:
//!
//!   - `allChannelSummarys(includeArchived: Boolean): [Channel!]!`
//!     — snapshot line 936.
//!   - `allChannelTags: [String!]!` — snapshot line 937.
//!   - `countChannelsByType(input: CountChannelsByTypeInput!):
//!     [ChannelTypeCount!]!` — snapshot line 938.
//!   - `queryChannels(input: QueryChannelInput!): ChannelConnection!`
//!     — snapshot line 943.
//!
//! Supporting types (also snapshot-exact):
//!   - `type ChannelTypeCount { type: String! count: Int! }` — snapshot 521.
//!   - `input CountChannelsByTypeInput { statusIn: [ChannelStatus!] }`
//!     — snapshot 918.
//!   - `input QueryChannelInput { after before first last orderBy where
//!     hasTag model }` — snapshot 886-934.
//!
//! The `Channel` / `ChannelConnection` / `ChannelOrder` / `ChannelWhereInput`
//! / `ChannelStatus` / `ChannelOrderSelection` / `resolve_channel_order`
//! symbols are REUSED from [`crate::channel`] (this slice adds nothing to the
//! Channel type itself).
//!
//! ## Go reference resolvers
//!
//!   - `Query.allChannelSummarys` — `internal/server/gql/conduit.resolvers.go`
//!     lines 674-701. Status filter defaults to `[enabled, disabled]`; when
//!     `includeArchived == true` it appends `archived`. Orders `Desc` by
//!     `orderingWeight`. Then, if a project id is present in context, it loads
//!     the project's active profile and applies
//!     `filterChannelsByProjectProfile`
//!     (`channel_visibility_helpers.go:10`).
//!   - `Query.allChannelTags` — `conduit.resolvers.go` lines 704-731. Selects
//!     `tags` from all non-archived channels (`StatusNEQ(archived)`), applies
//!     the same project-profile visibility filter when a project id is
//!     present, then flattens + uniques the tag lists (`lo.Uniq`).
//!   - `Query.countChannelsByType` — `conduit.resolvers.go` lines 733-763.
//!     Groups channels by `type` with `Count()`; the status predicate is
//!     `StatusIn(input.statusIn...)` when `statusIn` is non-empty, otherwise
//!     `StatusNEQ(archived)`.
//!   - `Query.queryChannels` — `conduit.resolvers.go` lines 765-770 →
//!     `biz.ChannelService.QueryChannels` (`biz/channel_query.go:26`). The
//!     resolver first remaps a `CREATED_AT` ordering request to
//!     `ent.DefaultChannelOrder.Field` (order by ID) — identical to the
//!     `channels` slice remap, so we route through
//!     [`crate::channel::resolve_channel_order`]. The service applies the
//!     `where` filter, an optional `hasTag` JSON-contains predicate, and —
//!     when `model` is set — an in-memory `IsModelSupported` filter that
//!     BYPASSES DB pagination (returns every match). All of that
//!     filtering/pagination is a host/DB concern behind the trait seam below.
//!
//! ## Wiring (for the coordinator — `impl QueryRoot` in `lib.rs`)
//!
//! The `#[Object]` root resolvers cannot be split across files, so the four
//! root methods are added to the single `impl QueryRoot` block in `lib.rs`.
//! Each is a thin delegate to the host-injected
//! [`ChannelExtraQueryServices`]; the exact bodies (verified by the test
//! `TestQueryRoot` below, which is a byte-for-byte reference implementation)
//! are:
//!
//! ```ignore
//! async fn all_channel_summarys(
//!     &self,
//!     ctx: &Context<'_>,
//!     include_archived: Option<bool>,
//! ) -> Result<Vec<channel::Channel>, String> {
//!     let services = channel_queries::channel_extra_query_services(ctx)?;
//!     services
//!         .all_channel_summarys(include_archived.unwrap_or(false))
//!         .await
//!         .map_err(|err| err.to_string())
//! }
//!
//! async fn all_channel_tags(&self, ctx: &Context<'_>) -> Result<Vec<String>, String> {
//!     let services = channel_queries::channel_extra_query_services(ctx)?;
//!     services.all_channel_tags().await.map_err(|err| err.to_string())
//! }
//!
//! async fn count_channels_by_type(
//!     &self,
//!     ctx: &Context<'_>,
//!     input: channel_queries::CountChannelsByTypeInput,
//! ) -> Result<Vec<channel_queries::ChannelTypeCount>, String> {
//!     let services = channel_queries::channel_extra_query_services(ctx)?;
//!     services
//!         .count_channels_by_type(input.into())
//!         .await
//!         .map_err(|err| err.to_string())
//! }
//!
//! async fn query_channels(
//!     &self,
//!     ctx: &Context<'_>,
//!     input: channel_queries::QueryChannelInput,
//! ) -> Result<channel::ChannelConnection, String> {
//!     let services = channel_queries::channel_extra_query_services(ctx)?;
//!     services
//!         .query_channels(input.into())
//!         .await
//!         .map_err(|err| err.to_string())
//! }
//! ```

use std::sync::Arc;

use async_graphql::{Context, InputObject, SimpleObject};

use crate::channel::{
    Channel, ChannelConnection, ChannelOrder, ChannelOrderSelection, ChannelStatus,
    ChannelWhereInput, resolve_channel_order,
};
use crate::scalars::CursorScalar;

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// `type ChannelTypeCount { type: String! count: Int! }` — snapshot line 521.
///
/// Go `ChannelTypeCount` (`models_gen.go`) — `Type string` / `Count int`,
/// populated from an ent `GroupBy(type).Aggregate(Count())` scan.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
pub struct ChannelTypeCount {
    // `type` is a Rust keyword; the GraphQL field name is pinned explicitly.
    #[graphql(name = "type")]
    pub channel_type: String,
    pub count: i64,
}

// ---------------------------------------------------------------------------
// Input types
// ---------------------------------------------------------------------------

/// `input CountChannelsByTypeInput { statusIn: [ChannelStatus!] }` — snapshot
/// line 918. `statusIn` is a nullable list of non-null `ChannelStatus`.
#[derive(Debug, Clone, Default, PartialEq, Eq, InputObject)]
pub struct CountChannelsByTypeInput {
    pub status_in: Option<Vec<ChannelStatus>>,
}

/// `input QueryChannelInput` — snapshot lines 886-934. Relay pagination args
/// plus `orderBy` / `where` (reused from the channel slice) and the two
/// channel-specific filters `hasTag` (JSON-contains on the `tags` column) and
/// `model` (in-memory `IsModelSupported` filter).
#[derive(Debug, Clone, Default, PartialEq, InputObject)]
pub struct QueryChannelInput {
    pub after: Option<CursorScalar>,
    pub first: Option<i32>,
    pub before: Option<CursorScalar>,
    pub last: Option<i32>,
    pub order_by: Option<ChannelOrder>,
    // `where` is a Rust keyword; the GraphQL argument name is pinned.
    #[graphql(name = "where")]
    pub where_filter: Option<ChannelWhereInput>,
    pub has_tag: Option<String>,
    pub model: Option<String>,
}

// ---------------------------------------------------------------------------
// Service-level argument structs (GraphQL inputs lowered for the host).
// ---------------------------------------------------------------------------

/// Lowered form of [`CountChannelsByTypeInput`] handed to the service.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CountChannelsByTypeArgs {
    pub status_in: Option<Vec<ChannelStatus>>,
}

impl From<CountChannelsByTypeInput> for CountChannelsByTypeArgs {
    fn from(input: CountChannelsByTypeInput) -> Self {
        Self {
            status_in: input.status_in,
        }
    }
}

/// Lowered form of [`QueryChannelInput`]. Cursor values are the raw `Cursor`
/// scalar strings; `order_by` has already had the Go `CREATED_AT` → default-ID
/// remap applied via [`resolve_channel_order`]; `where_filter` is the full
/// predicate input (the host lowers it into ent/repository predicates).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryChannelsArgs {
    pub after: Option<String>,
    pub first: Option<i32>,
    pub before: Option<String>,
    pub last: Option<i32>,
    pub order_by: Option<ChannelOrderSelection>,
    pub where_filter: Option<ChannelWhereInput>,
    pub has_tag: Option<String>,
    pub model: Option<String>,
}

impl From<QueryChannelInput> for QueryChannelsArgs {
    fn from(input: QueryChannelInput) -> Self {
        Self {
            after: input.after.map(|cursor| cursor.0),
            first: input.first,
            before: input.before.map(|cursor| cursor.0),
            last: input.last,
            // Mirrors the Go resolver's `CREATED_AT` → DefaultChannelOrder
            // remap (conduit.resolvers.go:766-768).
            order_by: resolve_channel_order(input.order_by),
            where_filter: input.where_filter,
            has_tag: input.has_tag,
            model: input.model,
        }
    }
}

// ---------------------------------------------------------------------------
// Service trait (host-injected seam) + error surface.
// ---------------------------------------------------------------------------

/// Error surface for the extended channel queries. Messages mirror the Go
/// `fmt.Errorf` wrap strings so frontend error handling stays stable:
///   - `failed to query channels: %w` (summarys / tags).
///   - `failed to get project: %w` (project-profile visibility lookup).
///   - `failed to query channel type counts: %w` (countChannelsByType).
///   - `queryChannels` returns the raw ent error unwrapped in Go, so
///     [`ChannelExtraQueryError::Query`] carries it verbatim.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ChannelExtraQueryError {
    #[error("channel service is not available")]
    ServiceUnavailable,
    #[error("failed to query channels: {0}")]
    QueryChannels(String),
    #[error("failed to get project: {0}")]
    GetProject(String),
    #[error("failed to query channel type counts: {0}")]
    CountByType(String),
    #[error("{0}")]
    Query(String),
}

#[derive(Debug, Clone)]
pub struct ChannelSensitiveFields {
    pub credentials: Option<crate::channel::ChannelCredentials>,
    pub disabled_api_keys: Vec<crate::channel::DisabledAPIKey>,
}

/// Backs the four channel list-page root queries. The host wires a concrete
/// implementation (ent `r.client.Channel` + `r.channelService` in Go); tests
/// use the in-memory double below.
#[async_trait::async_trait]
pub trait ChannelExtraQueryServices: Send + Sync {
    /// `Query.allChannelSummarys` — status filter `[enabled, disabled]`
    /// (plus `archived` when `include_archived`), ordered `Desc` by
    /// `orderingWeight`, then project-profile visibility filtered when a
    /// project id is present in context (host concern).
    async fn all_channel_summarys(
        &self,
        include_archived: bool,
    ) -> Result<Vec<Channel>, ChannelExtraQueryError>;

    /// `Query.allChannelTags` — unique tags across all non-archived channels
    /// (project-profile visibility filtered when a project id is present).
    async fn all_channel_tags(&self) -> Result<Vec<String>, ChannelExtraQueryError>;

    /// `Query.countChannelsByType` — group-by-type counts. When `status_in`
    /// is empty/absent the host filters `StatusNEQ(archived)`.
    async fn count_channels_by_type(
        &self,
        args: CountChannelsByTypeArgs,
    ) -> Result<Vec<ChannelTypeCount>, ChannelExtraQueryError>;

    /// `Query.queryChannels` — connection query with `where` / `hasTag` /
    /// `model` filters (model filtering bypasses DB pagination in Go).
    async fn query_channels(
        &self,
        args: QueryChannelsArgs,
    ) -> Result<ChannelConnection, ChannelExtraQueryError>;

    async fn channel_sensitive_fields(
        &self,
        _channel_id: &str,
    ) -> Result<Option<ChannelSensitiveFields>, ChannelExtraQueryError> {
        Ok(None)
    }

    async fn live_limiter_stats(
        &self,
        _channel_id: &str,
    ) -> Result<Option<crate::channel::ChannelLimiterStats>, ChannelExtraQueryError> {
        Ok(None)
    }

    async fn channel_model_prices(
        &self,
        _channel_id: &str,
    ) -> Result<Vec<crate::model_ext::ChannelModelPrice>, ChannelExtraQueryError> {
        Ok(Vec::new())
    }
}

/// Resolves the injected [`ChannelExtraQueryServices`] from the async-graphql
/// data bag; absent wiring surfaces the "service is not available" failure
/// mode (same convention as `channel::channel_query_services`).
pub(crate) fn channel_extra_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn ChannelExtraQueryServices>, String> {
    match ctx.data::<Arc<dyn ChannelExtraQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(ChannelExtraQueryError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use async_graphql::{EmptySubscription, ID, Object, Schema, SchemaBuilder};

    use super::*;
    use crate::channel::{ChannelEdge, ChannelType};
    use crate::pagination::connection_from_offset_page;
    use crate::scalars::TimeScalar;
    use crate::sdl_parity::{assert_block_parity, snapshot_text};

    type TestError = Box<dyn std::error::Error>;

    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn epoch() -> TimeScalar {
        TimeScalar(chrono::DateTime::<chrono::Utc>::default())
    }

    /// Map a `ChannelType` to the raw group-by string Go would read from the
    /// DB. Only the variants exercised by the tests are covered (the real
    /// host reads the string straight from ent).
    fn channel_type_name(channel_type: ChannelType) -> String {
        match channel_type {
            ChannelType::Openai => "openai".to_owned(),
            ChannelType::Anthropic => "anthropic".to_owned(),
            other => format!("{other:?}"),
        }
    }

    fn sample_channel(
        id: i64,
        name: &str,
        channel_type: ChannelType,
        status: ChannelStatus,
        ordering_weight: i64,
        tags: Option<Vec<String>>,
    ) -> Channel {
        Channel {
            id: ID::from(id.to_string()),
            created_at: epoch(),
            updated_at: epoch(),
            channel_type,
            base_url: None,
            website_url: None,
            quota_currency: "USD".to_string(),
            actual_quota_used: None,
            quota_remaining: None,
            name: name.to_owned(),
            status,
            supported_models: vec!["gpt-4o".to_owned()],
            manual_models: None,
            auto_sync_supported_models: false,
            auto_sync_model_pattern: None,
            tags,
            default_test_model: "gpt-4o".to_owned(),
            policies: None,
            settings: None,
            ordering_weight,
            error_message: None,
            remark: None,
            endpoints: None,
        }
    }

    // ---------------------------------------------------------------------
    // In-memory service double. Mirrors the Go resolver query shapes without
    // DB/HTTP; project-profile visibility filtering is a host concern and is
    // therefore NOT modelled here (Go applies it only when a project id is in
    // context — the admin data-bag path leaves it unset).
    // ---------------------------------------------------------------------

    #[derive(Default, Clone)]
    struct InMemoryChannelQueryService {
        channels: Arc<Mutex<Vec<Channel>>>,
        captured_query_args: Arc<Mutex<Vec<QueryChannelsArgs>>>,
    }

    #[async_trait::async_trait]
    impl ChannelExtraQueryServices for InMemoryChannelQueryService {
        async fn all_channel_summarys(
            &self,
            include_archived: bool,
        ) -> Result<Vec<Channel>, ChannelExtraQueryError> {
            let mut nodes: Vec<Channel> = lock(&self.channels)
                .iter()
                .filter(|c| match c.status {
                    ChannelStatus::Enabled | ChannelStatus::Disabled => true,
                    ChannelStatus::Archived => include_archived,
                })
                .cloned()
                .collect();
            // Go: Order(ent.Desc(FieldOrderingWeight)).
            nodes.sort_by_key(|b| std::cmp::Reverse(b.ordering_weight));
            Ok(nodes)
        }

        async fn all_channel_tags(&self) -> Result<Vec<String>, ChannelExtraQueryError> {
            // Go: StatusNEQ(archived) → flatten tags → lo.Uniq (first-seen
            // order preserved).
            let mut seen: Vec<String> = Vec::new();
            for channel in lock(&self.channels).iter() {
                if channel.status == ChannelStatus::Archived {
                    continue;
                }
                if let Some(tags) = &channel.tags {
                    for tag in tags {
                        if !seen.contains(tag) {
                            seen.push(tag.clone());
                        }
                    }
                }
            }
            Ok(seen)
        }

        async fn count_channels_by_type(
            &self,
            args: CountChannelsByTypeArgs,
        ) -> Result<Vec<ChannelTypeCount>, ChannelExtraQueryError> {
            let mut counts: BTreeMap<String, i64> = BTreeMap::new();
            for channel in lock(&self.channels).iter() {
                let keep = match &args.status_in {
                    // Non-empty statusIn → StatusIn(...).
                    Some(statuses) if !statuses.is_empty() => statuses.contains(&channel.status),
                    // Empty/absent → StatusNEQ(archived).
                    _ => channel.status != ChannelStatus::Archived,
                };
                if keep {
                    *counts
                        .entry(channel_type_name(channel.channel_type))
                        .or_insert(0) += 1;
                }
            }
            Ok(counts
                .into_iter()
                .map(|(channel_type, count)| ChannelTypeCount {
                    channel_type,
                    count,
                })
                .collect())
        }

        async fn query_channels(
            &self,
            args: QueryChannelsArgs,
        ) -> Result<ChannelConnection, ChannelExtraQueryError> {
            lock(&self.captured_query_args).push(args.clone());

            let nodes: Vec<Channel> = lock(&self.channels).clone();
            let total_count = nodes.len() as i64;
            let page_size = match args.first {
                Some(first) => usize::try_from(first).unwrap_or(0),
                None => nodes.len(),
            };
            let connection = connection_from_offset_page(nodes, 0, page_size);
            Ok(ChannelConnection {
                edges: Some(
                    connection
                        .edges
                        .into_iter()
                        .map(|edge| {
                            Some(ChannelEdge {
                                node: Some(edge.node),
                                cursor: CursorScalar(edge.cursor),
                            })
                        })
                        .collect(),
                ),
                page_info: connection.page_info,
                total_count,
            })
        }
    }

    // ---------------------------------------------------------------------
    // Test-only QueryRoot exposing exactly the four GAP-03 root fields. This
    // doubles as the reference implementation the coordinator copies into the
    // real `impl QueryRoot` in `lib.rs` (the `#[Object]` impl cannot be split
    // across files, so the file-local slice pattern is a probe root here).
    // ---------------------------------------------------------------------

    struct TestQueryRoot;

    #[Object]
    impl TestQueryRoot {
        async fn all_channel_summarys(
            &self,
            ctx: &Context<'_>,
            include_archived: Option<bool>,
        ) -> Result<Vec<Channel>, String> {
            let services = channel_extra_query_services(ctx)?;
            services
                .all_channel_summarys(include_archived.unwrap_or(false))
                .await
                .map_err(|err| err.to_string())
        }

        async fn all_channel_tags(&self, ctx: &Context<'_>) -> Result<Vec<String>, String> {
            let services = channel_extra_query_services(ctx)?;
            services
                .all_channel_tags()
                .await
                .map_err(|err| err.to_string())
        }

        async fn count_channels_by_type(
            &self,
            ctx: &Context<'_>,
            input: CountChannelsByTypeInput,
        ) -> Result<Vec<ChannelTypeCount>, String> {
            let services = channel_extra_query_services(ctx)?;
            services
                .count_channels_by_type(input.into())
                .await
                .map_err(|err| err.to_string())
        }

        async fn query_channels(
            &self,
            ctx: &Context<'_>,
            input: QueryChannelInput,
        ) -> Result<ChannelConnection, String> {
            let services = channel_extra_query_services(ctx)?;
            services
                .query_channels(input.into())
                .await
                .map_err(|err| err.to_string())
        }
    }

    type TestSchema = Schema<TestQueryRoot, async_graphql::EmptyMutation, EmptySubscription>;

    fn test_schema_builder()
    -> SchemaBuilder<TestQueryRoot, async_graphql::EmptyMutation, EmptySubscription> {
        // `Channel implements Node`, so the Relay `Node` interface must be
        // registered explicitly (same as `admin_schema_builder`).
        Schema::build(
            TestQueryRoot,
            async_graphql::EmptyMutation,
            EmptySubscription,
        )
        .register_output_type::<crate::channel::Node>()
    }

    fn bare_test_schema() -> TestSchema {
        test_schema_builder().finish()
    }

    fn schema_with(store: &InMemoryChannelQueryService) -> TestSchema {
        let services: Arc<dyn ChannelExtraQueryServices> = Arc::new(store.clone());
        test_schema_builder().data(services).finish()
    }

    // ---------------------------------------------------------------------
    // SDL parity: new supporting types + input types
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_channel_type_count_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_test_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type ChannelTypeCount",
            "type ChannelTypeCount",
            &[],
        )
    }

    #[test]
    fn sdl_count_channels_by_type_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_test_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input CountChannelsByTypeInput",
            "input CountChannelsByTypeInput",
            &[],
        )
    }

    #[test]
    fn sdl_query_channel_input_matches_snapshot() -> Result<(), TestError> {
        let sdl = bare_test_schema().sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "input QueryChannelInput",
            "input QueryChannelInput",
            &[],
        )
    }

    // ---------------------------------------------------------------------
    // SDL parity: root field signatures (snapshot lines 936-943)
    // ---------------------------------------------------------------------

    #[test]
    fn sdl_root_field_signatures_match_snapshot() -> Result<(), TestError> {
        let sdl = bare_test_schema().sdl();
        let snapshot = snapshot_text()?;
        for signature in [
            "allChannelSummarys(includeArchived: Boolean): [Channel!]!",
            "allChannelTags: [String!]!",
            "countChannelsByType(input: CountChannelsByTypeInput!): [ChannelTypeCount!]!",
            "queryChannels(input: QueryChannelInput!): ChannelConnection!",
        ] {
            assert!(
                sdl.contains(signature),
                "generated SDL missing `{signature}`:\n{sdl}"
            );
            assert!(
                snapshot.contains(signature),
                "snapshot missing `{signature}`"
            );
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Input lowering
    // ---------------------------------------------------------------------

    #[test]
    fn query_channel_input_lowers_created_at_to_default_id_order() {
        let args: QueryChannelsArgs = QueryChannelInput {
            order_by: Some(ChannelOrder {
                direction: crate::channel::OrderDirection::Desc,
                field: crate::channel::ChannelOrderField::CreatedAt,
            }),
            has_tag: Some("prod".to_owned()),
            model: Some("gpt-4o".to_owned()),
            ..QueryChannelInput::default()
        }
        .into();

        assert_eq!(
            args.order_by,
            Some(ChannelOrderSelection {
                direction: crate::channel::OrderDirection::Desc,
                term: crate::channel::ChannelOrderTerm::Id,
            })
        );
        assert_eq!(args.has_tag.as_deref(), Some("prod"));
        assert_eq!(args.model.as_deref(), Some("gpt-4o"));
    }

    // ---------------------------------------------------------------------
    // Resolver: allChannelSummarys
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn all_channel_summarys_excludes_archived_by_default_and_orders_desc_by_weight()
    -> Result<(), TestError> {
        let store = InMemoryChannelQueryService::default();
        {
            let mut guard = lock(&store.channels);
            guard.push(sample_channel(
                1,
                "low",
                ChannelType::Openai,
                ChannelStatus::Enabled,
                1,
                None,
            ));
            guard.push(sample_channel(
                2,
                "high",
                ChannelType::Openai,
                ChannelStatus::Disabled,
                5,
                None,
            ));
            guard.push(sample_channel(
                3,
                "arch",
                ChannelType::Openai,
                ChannelStatus::Archived,
                9,
                None,
            ));
        }
        let schema = schema_with(&store);

        let resp = schema
            .execute("query { allChannelSummarys { id name status } }")
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let rows = &data["allChannelSummarys"];
        // archived excluded → 2 rows, ordered Desc by orderingWeight (high, low).
        assert_eq!(rows.as_array().map(|a| a.len()), Some(2));
        assert_eq!(rows[0]["name"], "high");
        assert_eq!(rows[1]["name"], "low");
        Ok(())
    }

    #[tokio::test]
    async fn all_channel_summarys_include_archived_true_adds_archived() -> Result<(), TestError> {
        let store = InMemoryChannelQueryService::default();
        {
            let mut guard = lock(&store.channels);
            guard.push(sample_channel(
                1,
                "en",
                ChannelType::Openai,
                ChannelStatus::Enabled,
                1,
                None,
            ));
            guard.push(sample_channel(
                2,
                "arch",
                ChannelType::Openai,
                ChannelStatus::Archived,
                2,
                None,
            ));
        }
        let schema = schema_with(&store);

        let resp = schema
            .execute("query { allChannelSummarys(includeArchived: true) { name } }")
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        assert_eq!(
            data["allChannelSummarys"].as_array().map(|a| a.len()),
            Some(2)
        );
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Resolver: allChannelTags
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn all_channel_tags_uniques_across_non_archived_channels() -> Result<(), TestError> {
        let store = InMemoryChannelQueryService::default();
        {
            let mut guard = lock(&store.channels);
            guard.push(sample_channel(
                1,
                "a",
                ChannelType::Openai,
                ChannelStatus::Enabled,
                0,
                Some(vec!["prod".to_owned(), "us".to_owned()]),
            ));
            guard.push(sample_channel(
                2,
                "b",
                ChannelType::Openai,
                ChannelStatus::Disabled,
                0,
                Some(vec!["prod".to_owned(), "eu".to_owned()]),
            ));
            // Archived channel's tags must be excluded.
            guard.push(sample_channel(
                3,
                "c",
                ChannelType::Openai,
                ChannelStatus::Archived,
                0,
                Some(vec!["secret".to_owned()]),
            ));
        }
        let schema = schema_with(&store);

        let resp = schema.execute("query { allChannelTags }").await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let tags: Vec<String> = data["allChannelTags"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(tags, vec!["prod", "us", "eu"]);
        assert!(!tags.contains(&"secret".to_owned()));
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Resolver: countChannelsByType
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn count_channels_by_type_defaults_to_non_archived() -> Result<(), TestError> {
        let store = InMemoryChannelQueryService::default();
        {
            let mut guard = lock(&store.channels);
            guard.push(sample_channel(
                1,
                "a",
                ChannelType::Openai,
                ChannelStatus::Enabled,
                0,
                None,
            ));
            guard.push(sample_channel(
                2,
                "b",
                ChannelType::Openai,
                ChannelStatus::Disabled,
                0,
                None,
            ));
            guard.push(sample_channel(
                3,
                "c",
                ChannelType::Anthropic,
                ChannelStatus::Enabled,
                0,
                None,
            ));
            // Archived excluded when statusIn is absent.
            guard.push(sample_channel(
                4,
                "d",
                ChannelType::Anthropic,
                ChannelStatus::Archived,
                0,
                None,
            ));
        }
        let schema = schema_with(&store);

        let resp = schema
            .execute("query { countChannelsByType(input: {}) { type count } }")
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let rows = data["countChannelsByType"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let counts: BTreeMap<String, i64> = rows
            .iter()
            .filter_map(|row| Some((row["type"].as_str()?.to_owned(), row["count"].as_i64()?)))
            .collect();
        assert_eq!(counts.get("openai"), Some(&2));
        // anthropic: 1 non-archived (the archived one is excluded).
        assert_eq!(counts.get("anthropic"), Some(&1));
        Ok(())
    }

    #[tokio::test]
    async fn count_channels_by_type_honours_status_in_filter() -> Result<(), TestError> {
        let store = InMemoryChannelQueryService::default();
        {
            let mut guard = lock(&store.channels);
            guard.push(sample_channel(
                1,
                "a",
                ChannelType::Openai,
                ChannelStatus::Enabled,
                0,
                None,
            ));
            guard.push(sample_channel(
                2,
                "b",
                ChannelType::Openai,
                ChannelStatus::Archived,
                0,
                None,
            ));
        }
        let schema = schema_with(&store);

        // statusIn: [archived] → only the archived channel is counted.
        let resp = schema
            .execute(
                "query { countChannelsByType(input: { statusIn: [archived] }) { type count } }",
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let rows = data["countChannelsByType"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["type"], "openai");
        assert_eq!(rows[0]["count"], 1);
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Resolver: queryChannels
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn query_channels_returns_connection_and_captures_lowered_args() -> Result<(), TestError>
    {
        let store = InMemoryChannelQueryService::default();
        {
            let mut guard = lock(&store.channels);
            guard.push(sample_channel(
                1,
                "a",
                ChannelType::Openai,
                ChannelStatus::Enabled,
                0,
                None,
            ));
            guard.push(sample_channel(
                2,
                "b",
                ChannelType::Openai,
                ChannelStatus::Enabled,
                0,
                None,
            ));
        }
        let schema = schema_with(&store);

        let resp = schema
            .execute(
                r#"query {
                    queryChannels(input: {
                        first: 1,
                        orderBy: { direction: DESC, field: CREATED_AT },
                        hasTag: "prod",
                        model: "gpt-4o"
                    }) {
                        totalCount
                        edges { node { id name } }
                    }
                }"#,
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = resp.data.into_json()?;
        let connection = &data["queryChannels"];
        assert_eq!(connection["totalCount"], 2);
        assert_eq!(connection["edges"].as_array().map(|a| a.len()), Some(1));

        // The CREATED_AT ordering was remapped to the default ID order and the
        // channel-specific filters were carried through verbatim.
        let captured = lock(&store.captured_query_args);
        assert_eq!(captured.len(), 1);
        let args = &captured[0];
        assert_eq!(args.first, Some(1));
        assert_eq!(
            args.order_by,
            Some(ChannelOrderSelection {
                direction: crate::channel::OrderDirection::Desc,
                term: crate::channel::ChannelOrderTerm::Id,
            })
        );
        assert_eq!(args.has_tag.as_deref(), Some("prod"));
        assert_eq!(args.model.as_deref(), Some("gpt-4o"));
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Service-unavailable failure mode
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn missing_service_surfaces_unavailable_error() -> Result<(), TestError> {
        let schema = bare_test_schema();
        let resp = schema.execute("query { allChannelTags }").await;
        assert!(
            !resp.errors.is_empty(),
            "expected a service-unavailable error"
        );
        assert!(
            resp.errors
                .iter()
                .any(|err| err.message.contains("channel service is not available")),
            "unexpected errors: {:?}",
            resp.errors
        );
        Ok(())
    }
}
