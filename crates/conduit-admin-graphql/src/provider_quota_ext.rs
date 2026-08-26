//! GAP-C — `Channel.providerQuotaStatus` field + `ProviderQuotaStatus` type.
//!
//! ## What this slice adds
//!
//! The frontend `ProviderQuotaStatuses` operation resolves through the
//! `queryChannels` root field (already ported in `crate::channel_queries`) and
//! reads the per-channel quota snapshot off each `Channel` node's
//! `providerQuotaStatus` field. The GAP-C work is therefore the GraphQL
//! `Channel.providerQuotaStatus: ProviderQuotaStatus @goField(forceResolver:
//! true)` field (snapshot `tests/contracts/admin_graphql_schema.graphql:1868`)
//! plus the `ProviderQuotaStatus` output type and its two enums.
//!
//! Types ported here (all field-for-field from the snapshot):
//!   - `type ProviderQuotaStatus implements Node` (snapshot lines 5031-5064).
//!   - `enum ProviderQuotaStatusProviderType` (snapshot lines 5082-5091) —
//!     bound in Go to `ent/providerquotastatus.ProviderType`
//!     (`providerquotastatus.go:104-111`).
//!   - `enum ProviderQuotaStatusStatus` (snapshot lines 5095-5100) — bound to
//!     `ent/providerquotastatus.Status` (`providerquotastatus.go:133-136`).
//!
//! ## Go reference
//!   - `channelResolver.ProviderQuotaStatus` (`ent.resolvers.go:85-90`):
//!     `obj.ProviderQuotaStatus(ctx)` — ent O2O edge from Channel; returns nil
//!     (→ GraphQL null) when the channel has no quota-status row.
//!   - `providerQuotaStatusResolver.ID` / `.ChannelID`
//!     (`ent.resolvers.go:264-278`): wrap the numeric ids in the GUID wire form
//!     (`gid://conduit/ProviderQuotaStatus/<id>` and
//!     `gid://conduit/Channel/<id>`). The host populates the `id` / `channel_id`
//!     `ID` fields with that wire form; this crate only carries the values.
//!   - `channel: Channel!` back-edge — resolved here via the host-injected
//!     [`crate::channel::ChannelQueryServices`], mirroring the
//!     `RequestExecution.channel` pattern in `crate::request_execution`.
//!
//! ## NOT ported (out of scope — reachable only from a ProviderQuotaStatus
//! root connection query / `hasProviderQuotaStatusWith`, neither of which this
//! slice adds): `input ProviderQuotaStatusOrder`,
//! `enum ProviderQuotaStatusOrderField`, `input ProviderQuotaStatusWhereInput`
//! (snapshot lines 5068-5100+). They are not referenced by the
//! `providerQuotaStatus` field, so leaving them out keeps the SDL closed.
//!
//! ## Service wiring (host / Leader)
//!
//! The admin-graphql crate stays free of DB/IO. The host injects an
//! `Arc<dyn ProviderQuotaStatusServices>` into the schema data bag. Because
//! `Channel.providerQuotaStatus` is a NULLABLE field that the `queryChannels`
//! page reads across every channel, an ABSENT service degrades to `null`
//! (rather than a field error) so the channel list still resolves — matching
//! Go, where a channel without a quota-status edge yields nil. The `channel`
//! back-edge on `ProviderQuotaStatus` is non-null, so it still surfaces a field
//! error when its channel cannot be resolved (same as `RequestExecution.request`).

use std::sync::Arc;

use async_graphql::{ComplexObject, Context, Enum, ID, SimpleObject};

use crate::scalars::{MapScalar, TimeScalar};

// ---------------------------------------------------------------------------
// Enums (snapshot-exact lowercase / snake_case value spellings, pinned so the
// default SCREAMING_SNAKE renaming does not mangle them)
// ---------------------------------------------------------------------------

/// `enum ProviderQuotaStatusProviderType` — snapshot lines 5082-5091, bound to
/// Go `ent/providerquotastatus.ProviderType`
/// (`providerquotastatus.go:104-111`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "ProviderQuotaStatusProviderType")]
pub enum ProviderQuotaStatusProviderType {
    #[graphql(name = "claudecode")]
    Claudecode,
    #[graphql(name = "codex")]
    Codex,
    #[graphql(name = "github_copilot")]
    GithubCopilot,
    #[graphql(name = "nanogpt")]
    Nanogpt,
    #[graphql(name = "wafer")]
    Wafer,
    #[graphql(name = "synthetic")]
    Synthetic,
    #[graphql(name = "neuralwatt")]
    Neuralwatt,
    #[graphql(name = "apertis")]
    Apertis,
    #[graphql(name = "new_api")]
    NewApi,
}

/// `enum ProviderQuotaStatusStatus` — snapshot lines 5095-5100, bound to Go
/// `ent/providerquotastatus.Status` (`providerquotastatus.go:133-136`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
#[graphql(name = "ProviderQuotaStatusStatus")]
pub enum ProviderQuotaStatusStatus {
    #[graphql(name = "available")]
    Available,
    #[graphql(name = "warning")]
    Warning,
    #[graphql(name = "exhausted")]
    Exhausted,
    #[graphql(name = "unknown")]
    Unknown,
}

// ---------------------------------------------------------------------------
// Output type
// ---------------------------------------------------------------------------

/// `type ProviderQuotaStatus implements Node` — snapshot lines 5031-5064.
///
/// Scalar fields are carried verbatim; the non-null `channel: Channel!`
/// back-edge is resolved by the [`ComplexObject`] impl below. Like the other
/// edge-reachable `implements Node` types in this crate (e.g.
/// `RequestExecution`), it is a plain `SimpleObject` — the Relay `Node`
/// interface membership is handled by the global `node`/`nodes` slice, not by
/// listing it in the `Node` union here.
///
/// `id` / `channel_id` carry the GUID wire form the Go resolver produces
/// (`gid://conduit/ProviderQuotaStatus/<id>` / `gid://conduit/Channel/<id>`);
/// the host populates them.
#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(complex)]
#[graphql(name = "ProviderQuotaStatus")]
pub struct ProviderQuotaStatus {
    pub id: ID,
    pub created_at: TimeScalar,
    pub updated_at: TimeScalar,
    /// GraphQL field `channelID` — non-null ID (acronym rename pin; default
    /// camelCase would emit `channelId`). Snapshot line 5035.
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub provider_type: ProviderQuotaStatusProviderType,
    /// Overall status: available / warning / exhausted / unknown (snapshot
    /// line 5041).
    pub status: ProviderQuotaStatusStatus,
    /// Provider-specific quota data — `Map!` (snapshot line 5045). Non-null;
    /// the host supplies at least an empty object.
    pub quota_data: MapScalar,
    /// Timestamp for the next quota reset (primary window) — nullable Time
    /// (snapshot line 5049).
    pub next_reset_at: Option<TimeScalar>,
    /// True when `status` is available or warning (snapshot line 5053).
    pub ready: bool,
    /// Timestamp for the next scheduled quota check — non-null Time (snapshot
    /// line 5057).
    pub next_check_at: TimeScalar,
    /// Adapter used for a manually verified balance probe. `None` means the
    /// status came from a provider-native checker that has not gone through
    /// the administrator verification workflow.
    pub probe_adapter: Option<String>,
    /// Set only after an administrator compares the probe with the upstream
    /// NEW API balance and explicitly confirms it for scheduled refreshes.
    pub probe_verified_at: Option<TimeScalar>,
}

#[ComplexObject]
impl ProviderQuotaStatus {
    /// `ProviderQuotaStatus.channel: Channel!` — snapshot line 5062. Go
    /// auto-resolves the ent O2O back-edge (`obj.Channel(ctx)`). Nullable-safe
    /// on the wire in Go's ent generation, but the snapshot marks it non-null,
    /// so a missing channel surfaces a field error (mirrors
    /// `RequestExecution.request`). We delegate to the host-injected
    /// [`crate::channel::ChannelQueryServices`] with a `where: { id: <fk> }`
    /// predicate and return the first edge's node.
    ///
    /// `self.channel_id` carries the GUID wire form; the channel query service
    /// resolves the same GUID the `Channel.id` field renders, so the equality
    /// predicate matches without further decoding here (the host's predicate
    /// lowering handles the GUID→row-id decode, exactly as for
    /// `RequestExecution.channel`).
    async fn channel(&self, ctx: &Context<'_>) -> Result<crate::channel::Channel, String> {
        let services = crate::channel::channel_query_services(ctx)?;
        let injected = crate::channel::ChannelWhereInput {
            id: Some(self.channel_id.clone()),
            ..Default::default()
        };
        let args = crate::channel::ChannelConnectionArgs {
            after: None,
            first: Some(1),
            before: None,
            last: None,
            order_by: None,
            where_filter: Some(injected),
        };
        let conn = services
            .channels(args)
            .await
            .map_err(|err| err.to_string())?;
        let edges = conn.edges.unwrap_or_default();
        let edge =
            edges.into_iter().flatten().next().ok_or_else(|| {
                "provider quota status's parent channel was not found".to_string()
            })?;
        edge.node
            .ok_or_else(|| "provider quota status's parent channel was not found".to_string())
    }
}

// ---------------------------------------------------------------------------
// Service seam (host-injected)
// ---------------------------------------------------------------------------

/// Backs `Channel.providerQuotaStatus` (Go `channelResolver.ProviderQuotaStatus`,
/// `ent.resolvers.go:85-90`: the ent O2O edge `obj.ProviderQuotaStatus(ctx)`).
/// The host wires the real repository; when absent, the resolver degrades to
/// `null` (see module doc).
#[async_trait::async_trait]
pub trait ProviderQuotaStatusServices: Send + Sync {
    /// Return the quota-status snapshot for `channel_id` (the raw GraphQL `ID!`
    /// wire form of the parent channel), or `None` when the channel has no
    /// quota-status row — mirroring Go's nil edge.
    async fn provider_quota_status_for_channel(
        &self,
        channel_id: &str,
    ) -> Result<Option<ProviderQuotaStatus>, String>;
}

/// Resolve `Channel.providerQuotaStatus`. Called from the `#[ComplexObject] impl
/// Channel` block in `crate::channel`. When no service is wired the field
/// degrades to `null` (the field is nullable and the `queryChannels` page reads
/// it across every channel — a hard error would break the whole list), matching
/// the Go behaviour for a channel without a quota-status edge.
pub async fn resolve_channel_provider_quota_status(
    ctx: &Context<'_>,
    channel_id: &ID,
) -> Result<Option<ProviderQuotaStatus>, String> {
    match ctx.data::<Arc<dyn ProviderQuotaStatusServices>>() {
        Ok(services) => {
            services
                .provider_quota_status_for_channel(channel_id.as_str())
                .await
        }
        // Unwired → null (graceful degrade, see doc).
        Err(_) => Ok(None),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_graphql::{EmptySubscription, Name, Schema, Value};

    use super::*;

    type TestError = Box<dyn std::error::Error>;

    // ---- in-memory service double ---------------------------------------

    struct FakeQuotaServices {
        result: Option<ProviderQuotaStatus>,
    }

    #[async_trait::async_trait]
    impl ProviderQuotaStatusServices for FakeQuotaServices {
        async fn provider_quota_status_for_channel(
            &self,
            _channel_id: &str,
        ) -> Result<Option<ProviderQuotaStatus>, String> {
            Ok(self.result.clone())
        }
    }

    fn sample_status(channel_id: &str) -> ProviderQuotaStatus {
        ProviderQuotaStatus {
            id: ID::from("gid://conduit/ProviderQuotaStatus/1"),
            created_at: TimeScalar(chrono::DateTime::<chrono::Utc>::default()),
            updated_at: TimeScalar(chrono::DateTime::<chrono::Utc>::default()),
            channel_id: ID::from(channel_id.to_string()),
            provider_type: ProviderQuotaStatusProviderType::Codex,
            status: ProviderQuotaStatusStatus::Available,
            quota_data: MapScalar(serde_json::json!({"remaining": 100})),
            next_reset_at: None,
            ready: true,
            next_check_at: TimeScalar(chrono::DateTime::<chrono::Utc>::default()),
            probe_adapter: None,
            probe_verified_at: None,
        }
    }

    // ---- SDL parity ------------------------------------------------------

    fn snapshot_text() -> Result<String, TestError> {
        std::fs::read_to_string("tests/contracts/admin_graphql_schema.graphql")
            .or_else(|_| {
                std::fs::read_to_string(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tests/contracts/admin_graphql_schema.graphql"
                ))
            })
            .map_err(|err| format!("snapshot read failed: {err}").into())
    }

    #[test]
    fn sdl_provider_quota_status_type_matches_snapshot() -> Result<(), TestError> {
        let sdl = crate::build_admin_schema().sdl();
        let snapshot = snapshot_text()?;
        // The Rust host extends the Go snapshot with the administrator-verified
        // balance-probe metadata used by the channels UI.
        crate::sdl_parity::assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "type ProviderQuotaStatus",
            "type ProviderQuotaStatus",
            &[],
            &["probeAdapter: String", "probeVerifiedAt: Time"],
        )?;
        crate::sdl_parity::assert_block_parity_with_extensions(
            &sdl,
            &snapshot,
            "enum ProviderQuotaStatusProviderType",
            "enum ProviderQuotaStatusProviderType",
            &[],
            &["new_api"],
        )?;
        crate::sdl_parity::assert_block_parity(
            &sdl,
            &snapshot,
            "enum ProviderQuotaStatusStatus",
            "enum ProviderQuotaStatusStatus",
            &[],
        )?;
        Ok(())
    }

    #[test]
    fn sdl_channel_declares_provider_quota_status_field() {
        let sdl = crate::build_admin_schema().sdl();
        // The forceResolver field is now present on Channel.
        assert!(
            sdl.contains("providerQuotaStatus: ProviderQuotaStatus"),
            "Channel.providerQuotaStatus field missing from SDL:\n{sdl}"
        );
    }

    // ---- resolver behaviour ---------------------------------------------

    /// The unwired path degrades to `null` (no service in the data bag), and a
    /// wired service returns its snapshot. `resolve_channel_provider_quota_status`
    /// is the helper GAP-C owns; exercise it via a throwaway query field so a
    /// real `Context` (with / without the injected service) drives it.
    #[tokio::test]
    async fn resolve_helper_wired_and_unwired() -> Result<(), TestError> {
        struct QRoot;
        #[async_graphql::Object]
        impl QRoot {
            /// Returns the resolved quota status for a fixed channel id, or null.
            async fn pqs(&self, ctx: &Context<'_>) -> Result<Option<ProviderQuotaStatus>, String> {
                let id = ID::from("gid://conduit/Channel/7");
                resolve_channel_provider_quota_status(ctx, &id).await
            }
        }

        // Wired: returns the snapshot.
        let svc: Arc<dyn ProviderQuotaStatusServices> = Arc::new(FakeQuotaServices {
            result: Some(sample_status("gid://conduit/Channel/7")),
        });
        let wired = Schema::build(QRoot, async_graphql::EmptyMutation, EmptySubscription)
            .data(svc)
            .finish();
        let resp = wired.execute("{ pqs { channelID ready } }").await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        assert!(
            resp.data.to_string().contains("gid://conduit/Channel/7"),
            "wired resolver should surface the status: {}",
            resp.data
        );

        // Unwired: degrades to null (no field error).
        let bare = Schema::build(QRoot, async_graphql::EmptyMutation, EmptySubscription).finish();
        let resp = bare.execute("{ pqs { channelID } }").await;
        assert!(
            resp.errors.is_empty(),
            "unwired must not error: {:?}",
            resp.errors
        );
        let data = match resp.data {
            Value::Object(map) => map,
            other => return Err(format!("expected object, got {other:?}").into()),
        };
        match data.get(&Name::new("pqs")) {
            Some(Value::Null) => Ok(()),
            other => Err(format!("unwired pqs should be null, got {other:?}").into()),
        }
    }

    /// A quota-status object serializes its scalar fields through GraphQL. We
    /// register it as the return of a throwaway root field to exercise the
    /// SimpleObject field resolvers (`quotaData`, `ready`, enum spellings)
    /// without needing a channel node.
    #[tokio::test]
    async fn provider_quota_status_fields_serialize() -> Result<(), TestError> {
        struct QRoot;
        #[async_graphql::Object]
        impl QRoot {
            async fn pqs(&self) -> ProviderQuotaStatus {
                sample_status("gid://conduit/Channel/7")
            }
        }
        let schema = Schema::build(QRoot, async_graphql::EmptyMutation, EmptySubscription).finish();
        let resp = schema
            .execute(
                "{ pqs { channelID providerType status quotaData ready nextResetAt nextCheckAt } }",
            )
            .await;
        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let data = match resp.data {
            Value::Object(map) => map,
            other => return Err(format!("expected object, got {other:?}").into()),
        };
        let pqs = match data.get(&Name::new("pqs")) {
            Some(Value::Object(m)) => m,
            other => return Err(format!("pqs field unexpected: {other:?}").into()),
        };
        // enum lowercase spelling preserved.
        match pqs.get(&Name::new("providerType")) {
            Some(Value::Enum(name)) => assert_eq!(name.as_str(), "codex"),
            other => return Err(format!("providerType unexpected: {other:?}").into()),
        }
        match pqs.get(&Name::new("ready")) {
            Some(Value::Boolean(true)) => {}
            other => return Err(format!("ready unexpected: {other:?}").into()),
        }
        Ok(())
    }
}
