//! GAP-F — Quota-domain Query GraphQL slice.
//!
//! Ports the two read-side quota operations the frontend `system/quotas` and
//! api-key quota-usage views call, which had no Rust Query resolver yet (only
//! the write-side `updateQuotaEnforcementSettings` mutation existed in
//! `crate::mutation`):
//!
//!   - `Query.quotaEnforcementSettings: QuotaEnforcementSettings!`
//!     (snapshot line 9782) — Go `queryResolver.QuotaEnforcementSettings`
//!     (`system.resolvers.go:523-525`) → `systemService.QuotaEnforcementSettings`.
//!   - `Query.apiKeyQuotaUsages(apiKeyId: ID!): [APIKeyProfileQuotaUsage!]!`
//!     (snapshot line 948) — Go `queryResolver.APIKeyQuotaUsages`
//!     (`conduit.resolvers.go:773-802`): `client.APIKey.Get` then
//!     `quotaService.ProfileQuotaUsages`, mapping each usage row onto the
//!     GraphQL shape.
//!
//! ## Types
//!
//! Reused verbatim (not redefined here):
//!   - `QuotaEnforcementSettings` (`crate::mutation`) — the write-side slice
//!     already owns it; this slice adds its `SimpleObject` derive so the same
//!     type serves both the query and the mutation. Snapshot lines 9512-9515
//!     (`{ enabled: Boolean!, mode: QuotaEnforcementMode! }`).
//!   - `APIKeyQuota` (`crate::apikey`) — referenced by
//!     `APIKeyProfileQuotaUsage.quota`.
//!
//! Owned by this slice (snapshot-verbatim):
//!   - `type APIKeyQuotaUsage { requestCount: Int! totalTokens: Int!
//!     totalCost: Decimal! }` (snapshot lines 475-479).
//!   - `type APIKeyQuotaWindow { start: Time end: Time }` (both nullable,
//!     snapshot lines 481-484).
//!   - `type APIKeyProfileQuotaUsage { profileName: String! quota: APIKeyQuota!
//!     window: APIKeyQuotaWindow! usage: APIKeyQuotaUsage! }` (snapshot lines
//!     486-491).
//!
//! ## Service wiring
//!
//! This slice introduces a self-contained host-injected trait
//! [`QuotaQueryServices`] (rather than extending
//! `mutation::QuotaMutationServices`, which would break that module's in-memory
//! test double). The host wires one concrete type implementing both. Unwired =>
//! "quota service is not available".
//!
//! ## Contract note (snapshot vs coordinator brief)
//!
//! `apiKeyQuotaUsages` takes the argument `apiKeyId: ID!` (lower-case `d` — the
//! snapshot spelling, not `apiKeyID`), so the Rust arg carries an explicit
//! `#[graphql(name = "apiKeyId")]`. The admin resolver takes only that one
//! argument; the OpenAPI variant's extra `key`/`name` args
//! (`openapi.resolvers.go:95`) belong to a different schema and are out of scope.

use std::sync::Arc;

use async_graphql::{Context, SimpleObject};

use crate::apikey::APIKeyQuota;
use crate::scalars::{DecimalScalar, TimeScalar};

// ---------------------------------------------------------------------------
// Output object types (snapshot-verbatim)
// ---------------------------------------------------------------------------

/// `type APIKeyQuotaUsage` — snapshot lines 475-479. All three fields non-null.
/// Mirrors Go `APIKeyQuotaUsage` (models_gen.go) built from
/// `biz` usage counters (`conduit.resolvers.go:790-794`). `requestCount` and
/// `totalTokens` are `Int!` (Go `int`); `totalCost` uses the crate's `Decimal`
/// scalar (Go `objects.Decimal`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "APIKeyQuotaUsage")]
pub struct ApiKeyQuotaUsage {
    pub request_count: i64,
    pub total_tokens: i64,
    pub total_cost: DecimalScalar,
}

/// `type APIKeyQuotaWindow { start: Time end: Time }` — snapshot lines 481-484.
/// Both bounds nullable (Go `*time.Time`).
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "APIKeyQuotaWindow")]
pub struct ApiKeyQuotaWindow {
    pub start: Option<TimeScalar>,
    pub end: Option<TimeScalar>,
}

/// `type APIKeyProfileQuotaUsage` — snapshot lines 486-491. All four fields
/// non-null. Mirrors Go `APIKeyProfileQuotaUsage` assembled in
/// `conduit.resolvers.go:785-799`.
#[derive(Debug, Clone, PartialEq, Eq, SimpleObject)]
#[graphql(name = "APIKeyProfileQuotaUsage")]
pub struct ApiKeyProfileQuotaUsage {
    pub profile_name: String,
    pub quota: APIKeyQuota,
    pub window: ApiKeyQuotaWindow,
    pub usage: ApiKeyQuotaUsage,
}

// ---------------------------------------------------------------------------
// Service trait (host-injected)
// ---------------------------------------------------------------------------

/// Error surface for the quota read-side slice. Messages mirror the Go
/// `fmt.Errorf("...: %w")` prefixes so frontend error handling stays stable.
#[derive(Debug, Clone, thiserror::Error)]
pub enum QuotaQueryError {
    #[error("quota service is not available")]
    ServiceUnavailable,
    /// Go `conduit.resolvers.go:775` — `client.APIKey.Get` failure ("failed to
    /// get api key: %w"), which also covers the not-found case.
    #[error("failed to get api key: {0}")]
    GetApiKey(String),
    /// Go `conduit.resolvers.go:780` — `quotaService.ProfileQuotaUsages`
    /// failure ("failed to get api key quota usage: %w").
    #[error("failed to get api key quota usage: {0}")]
    ProfileQuotaUsages(String),
    /// Go `system.resolvers.go:523` — settings read failure. The Go body
    /// returns the default on not-found, so this only fires on a hard IO error.
    #[error("failed to get quota enforcement settings: {0}")]
    EnforcementSettings(String),
}

/// Backs the two quota read queries. The host wires a single concrete type
/// (typically the same one implementing `mutation::QuotaMutationServices`).
///
/// The `api_key_quota_usages` method folds the Go resolver's two service calls
/// (`client.APIKey.Get` + `quotaService.ProfileQuotaUsages`) behind one trait
/// method: the host performs the id decode + lookup and returns the assembled
/// usage rows. `api_key_id` is the raw `ID!` scalar string
/// (`gid://conduit/APIKey/<id>` wire form); the host decodes it.
#[async_trait::async_trait]
pub trait QuotaQueryServices: Send + Sync {
    /// Mirrors Go `queryResolver.QuotaEnforcementSettings`
    /// (system.resolvers.go:523-525): return the persisted settings (or the
    /// default on not-found — the Go service branch returns the default).
    async fn quota_enforcement_settings(
        &self,
    ) -> Result<crate::mutation::QuotaEnforcementSettings, QuotaQueryError>;

    /// Mirrors Go `queryResolver.APIKeyQuotaUsages`
    /// (conduit.resolvers.go:773-802): look up the api key, compute the
    /// per-profile quota usage rows.
    async fn api_key_quota_usages(
        &self,
        api_key_id: &str,
    ) -> Result<Vec<ApiKeyProfileQuotaUsage>, QuotaQueryError>;
}

/// Resolves the injected [`QuotaQueryServices`] from the async-graphql data
/// bag; absent wiring surfaces the "quota service is not available" failure
/// mode (same convention as the other slices).
pub(crate) fn quota_query_services(
    ctx: &Context<'_>,
) -> Result<Arc<dyn QuotaQueryServices>, String> {
    match ctx.data::<Arc<dyn QuotaQueryServices>>() {
        Ok(services) => Ok(Arc::clone(services)),
        Err(_) => Err(QuotaQueryError::ServiceUnavailable.to_string()),
    }
}

// ===========================================================================
// Resolver wiring (for the coordinator).
//
// async-graphql's `#[Object]` macro forbids splitting a root's impl across
// modules (E0119), so this slice contributes no `#[Object] impl QueryRoot`.
// The two resolver bodies are pasted into the single `#[Object] impl QueryRoot`
// in `lib.rs`; the `TestQueryRoot` in the test module below is a byte-for-byte
// reference implementation.
//
// Query methods (paste into `#[Object] impl QueryRoot` in `lib.rs`):
//
// ```ignore
// /// Mirrors Go `Query.quotaEnforcementSettings` (system.resolvers.go:523).
// async fn quota_enforcement_settings(
//     &self,
//     ctx: &Context<'_>,
// ) -> Result<crate::mutation::QuotaEnforcementSettings, String> {
//     let s = crate::quota_ext::quota_query_services(ctx)?;
//     s.quota_enforcement_settings().await.map_err(|e| e.to_string())
// }
//
// /// Mirrors Go `Query.apiKeyQuotaUsages` (conduit.resolvers.go:773).
// async fn api_key_quota_usages(
//     &self,
//     ctx: &Context<'_>,
//     #[graphql(name = "apiKeyId")] api_key_id: async_graphql::ID,
// ) -> Result<Vec<crate::quota_ext::ApiKeyProfileQuotaUsage>, String> {
//     let s = crate::quota_ext::quota_query_services(ctx)?;
//     s.api_key_quota_usages(api_key_id.as_str()).await.map_err(|e| e.to_string())
// }
// ```
// ===========================================================================

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_graphql::{
        Context, EmptyMutation, EmptySubscription, ID, Name, Object, Schema, SchemaBuilder, Value,
    };

    use super::*;
    use crate::mutation::QuotaEnforcementSettings;
    use crate::scalars::QuotaEnforcementMode;
    use crate::sdl_parity::{assert_block_parity, snapshot_text};

    type TestError = Box<dyn std::error::Error>;

    /// Mutex-guard helper that never panics on poison.
    fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// Extract the inner `Value::Object` or panic with a clear message.
    fn as_object(value: &Value) -> &async_graphql::indexmap::IndexMap<Name, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("expected Value::Object, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------
    // In-memory fake service.
    // ---------------------------------------------------------------------

    #[derive(Default)]
    struct FakeQuotaQueryService {
        settings: Arc<Mutex<QuotaEnforcementSettings>>,
        settings_error: Option<QuotaQueryError>,
        usages: Arc<Mutex<Vec<ApiKeyProfileQuotaUsage>>>,
        usages_error: Option<QuotaQueryError>,
        /// Records the id passed to `api_key_quota_usages`.
        seen_api_key_id: Arc<Mutex<Option<String>>>,
    }

    #[async_trait::async_trait]
    impl QuotaQueryServices for FakeQuotaQueryService {
        async fn quota_enforcement_settings(
            &self,
        ) -> Result<QuotaEnforcementSettings, QuotaQueryError> {
            match &self.settings_error {
                Some(err) => Err(err.clone()),
                None => Ok(*lock(&self.settings)),
            }
        }

        async fn api_key_quota_usages(
            &self,
            api_key_id: &str,
        ) -> Result<Vec<ApiKeyProfileQuotaUsage>, QuotaQueryError> {
            *lock(&self.seen_api_key_id) = Some(api_key_id.to_string());
            match &self.usages_error {
                Some(err) => Err(err.clone()),
                None => Ok(lock(&self.usages).clone()),
            }
        }
    }

    // Test-only reference QueryRoot. `#[Object]` cannot be split across modules,
    // so this mirrors what the coordinator pastes into the real QueryRoot
    // (lib.rs). It exercises the resolver bodies against the fake service.
    struct TestQueryRoot;

    #[Object]
    impl TestQueryRoot {
        async fn quota_enforcement_settings(
            &self,
            ctx: &Context<'_>,
        ) -> Result<QuotaEnforcementSettings, String> {
            let s = quota_query_services(ctx)?;
            s.quota_enforcement_settings()
                .await
                .map_err(|e| e.to_string())
        }

        async fn api_key_quota_usages(
            &self,
            ctx: &Context<'_>,
            #[graphql(name = "apiKeyId")] api_key_id: ID,
        ) -> Result<Vec<ApiKeyProfileQuotaUsage>, String> {
            let s = quota_query_services(ctx)?;
            s.api_key_quota_usages(api_key_id.as_str())
                .await
                .map_err(|e| e.to_string())
        }
    }

    type TestSchema = Schema<TestQueryRoot, EmptyMutation, EmptySubscription>;

    fn test_schema_builder() -> SchemaBuilder<TestQueryRoot, EmptyMutation, EmptySubscription> {
        Schema::build(TestQueryRoot, EmptyMutation, EmptySubscription)
    }

    fn schema_with(service: FakeQuotaQueryService) -> TestSchema {
        let arc: Arc<dyn QuotaQueryServices> = Arc::new(service);
        test_schema_builder().data(arc).finish()
    }

    fn sample_usage(profile: &str) -> ApiKeyProfileQuotaUsage {
        ApiKeyProfileQuotaUsage {
            profile_name: profile.to_owned(),
            quota: APIKeyQuota {
                requests: Some(100),
                total_tokens: Some(1000),
                cost: None,
                period: crate::apikey::APIKeyQuotaPeriod {
                    period_type: crate::apikey::APIKeyQuotaPeriodType::AllTime,
                    past_duration: None,
                    calendar_duration: None,
                },
            },
            window: ApiKeyQuotaWindow {
                start: None,
                end: None,
            },
            usage: ApiKeyQuotaUsage {
                request_count: 5,
                total_tokens: 42,
                total_cost: DecimalScalar(rust_decimal::Decimal::new(125, 2)),
            },
        }
    }

    // ---- SDL parity -------------------------------------------------------

    #[test]
    fn sdl_output_types_match_snapshot() -> Result<(), TestError> {
        let schema = test_schema_builder().finish();
        let sdl = schema.sdl();
        let snapshot = snapshot_text()?;
        for header in [
            "type APIKeyQuotaUsage",
            "type APIKeyQuotaWindow",
            "type APIKeyProfileQuotaUsage",
        ] {
            assert_block_parity(&sdl, &snapshot, header, header, &[])?;
        }
        Ok(())
    }

    #[test]
    fn sdl_quota_enforcement_settings_matches_snapshot() -> Result<(), TestError> {
        // Reused output type — assert the full block matches the snapshot.
        let schema = test_schema_builder().finish();
        let sdl = schema.sdl();
        let snapshot = snapshot_text()?;
        assert_block_parity(
            &sdl,
            &snapshot,
            "type QuotaEnforcementSettings",
            "type QuotaEnforcementSettings",
            &[],
        )?;
        Ok(())
    }

    #[test]
    fn sdl_query_root_carries_quota_fields() {
        let schema = test_schema_builder().finish();
        let sdl = schema.sdl();
        assert!(
            sdl.contains("quotaEnforcementSettings: QuotaEnforcementSettings!"),
            "SDL missing quotaEnforcementSettings query:\n{sdl}"
        );
        assert!(
            sdl.contains("apiKeyQuotaUsages(apiKeyId: ID!): [APIKeyProfileQuotaUsage!]!"),
            "SDL missing apiKeyQuotaUsages query:\n{sdl}"
        );
    }

    // ---- resolver: quota_enforcement_settings -----------------------------

    #[tokio::test]
    async fn quota_enforcement_settings_returns_service_data() {
        let fake = FakeQuotaQueryService {
            settings: Arc::new(Mutex::new(QuotaEnforcementSettings {
                enabled: true,
                mode: QuotaEnforcementMode::DePrioritize,
            })),
            ..FakeQuotaQueryService::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute("{ quotaEnforcementSettings { enabled mode } }")
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        let settings = match obj.get(&Name::new("quotaEnforcementSettings")) {
            Some(v) => as_object(v),
            None => panic!("quotaEnforcementSettings field missing"),
        };
        match settings.get(&Name::new("enabled")) {
            Some(Value::Boolean(true)) => {}
            other => panic!("enabled unexpected: {other:?}"),
        }
        match settings.get(&Name::new("mode")) {
            Some(Value::Enum(name)) => assert_eq!(name.as_str(), "DE_PRIORITIZE"),
            other => panic!("mode unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn quota_enforcement_settings_surfaces_error() {
        let fake = FakeQuotaQueryService {
            settings_error: Some(QuotaQueryError::EnforcementSettings("boom".to_owned())),
            ..FakeQuotaQueryService::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute("{ quotaEnforcementSettings { enabled } }")
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("failed to get quota enforcement settings"),
            "msg: {msg}"
        );
    }

    // ---- resolver: api_key_quota_usages -----------------------------------

    #[tokio::test]
    async fn api_key_quota_usages_returns_rows_and_forwards_id() {
        let service = FakeQuotaQueryService::default();
        *lock(&service.usages) = vec![sample_usage("default"), sample_usage("premium")];
        let seen = Arc::clone(&service.seen_api_key_id);
        let schema = schema_with(service);

        let resp = schema
            .execute(
                r#"{ apiKeyQuotaUsages(apiKeyId: "gid://conduit/APIKey/7") {
                    profileName
                    quota { requests totalTokens }
                    window { start end }
                    usage { requestCount totalTokens totalCost }
                } }"#,
            )
            .await;

        assert!(resp.errors.is_empty(), "errors: {:?}", resp.errors);
        let obj = as_object(&resp.data);
        match obj.get(&Name::new("apiKeyQuotaUsages")) {
            Some(Value::List(items)) => assert_eq!(items.len(), 2),
            other => panic!("expected list, got {other:?}"),
        }
        // The raw ID scalar reached the service verbatim.
        assert_eq!(
            lock(&seen).clone(),
            Some("gid://conduit/APIKey/7".to_owned())
        );
    }

    #[tokio::test]
    async fn api_key_quota_usages_surfaces_get_error() {
        let fake = FakeQuotaQueryService {
            usages_error: Some(QuotaQueryError::GetApiKey("not found".to_owned())),
            ..FakeQuotaQueryService::default()
        };
        let schema = schema_with(fake);

        let resp = schema
            .execute(r#"{ apiKeyQuotaUsages(apiKeyId: "9") { profileName } }"#)
            .await;

        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(msg.contains("failed to get api key"), "msg: {msg}");
    }

    // ---- service-unavailable fallback -------------------------------------

    #[tokio::test]
    async fn resolvers_surface_service_unavailable_when_unwired() {
        let schema: TestSchema = test_schema_builder().finish();

        let resp = schema
            .execute("{ quotaEnforcementSettings { enabled } }")
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("quota service is not available"),
            "unexpected msg: {msg}"
        );

        let resp = schema
            .execute(r#"{ apiKeyQuotaUsages(apiKeyId: "1") { profileName } }"#)
            .await;
        assert_eq!(resp.errors.len(), 1);
        let msg = format!("{}", resp.errors[0]);
        assert!(
            msg.contains("quota service is not available"),
            "unexpected msg: {msg}"
        );
    }
}
